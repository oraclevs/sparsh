//! The `~/.sparsh` directory as a Spar package: layout, one-time migration
//! from the old flat layout, fresh-install seeding, and the package locator
//! used for `import pkg`.

use std::path::{Path, PathBuf};

use spar::package::{
    commands, GitCommandProvider, Lockfile, ModuleLocator, NetworkPolicy, PackageKind,
    PackageStore, StorePaths, PACKAGE_LOCK_FILE, PACKAGE_MANIFEST_FILE,
};

use crate::environment::EnvironmentService;

pub(crate) const BACKUP_EXISTS_NOTICE: &str =
    "~/.sparsh.bak exists; move it and restart to migrate";
const TEMPLATE: &str = include_str!("../templates/config.spar");
const PACKAGE_NAME: &str = "sparsh-config";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Prepared {
    AlreadyPackage,
    Seeded,
    Migrated,
    LegacyKept { notice: String },
}

#[derive(Debug)]
pub(crate) struct PrepareError {
    pub step: &'static str,
    pub message: String,
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "cannot migrate ~/.sparsh ({}): {}",
            self.step, self.message
        )
    }
}

impl std::error::Error for PrepareError {}

fn step_error(step: &'static str, error: impl std::fmt::Display) -> PrepareError {
    PrepareError {
        step,
        message: error.to_string(),
    }
}

pub(crate) fn root_for_home(home: &Path) -> PathBuf {
    home.join(".sparsh")
}

pub(crate) fn backup_for_home(home: &Path) -> PathBuf {
    home.join(".sparsh.bak")
}

/// `~/.sparsh/src/config.spar`, or the legacy flat `~/.sparsh/sparsh.spar`
/// when migration was skipped and the package entry does not exist.
pub(crate) fn entry_for_home(home: &Path) -> PathBuf {
    let root = root_for_home(home);
    let entry = root.join("src/config.spar");
    let legacy = root.join("sparsh.spar");
    if !entry.is_file() && legacy.is_file() {
        legacy
    } else {
        entry
    }
}

pub(crate) fn store_for(environment: &EnvironmentService) -> PackageStore {
    let home = environment
        .get("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let data = environment
        .get("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let cache = environment
        .get("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"));
    PackageStore::new(StorePaths::new(data, cache))
}

/// The store a session whose `HOME` is `home` (and which has no `XDG_*`
/// overrides) will use; lets tests build the same store `store_for` derives.
#[cfg(test)]
pub(crate) fn store_for_paths_for_tests(home: &Path) -> PackageStore {
    PackageStore::new(StorePaths::new(
        home.join(".local/share"),
        home.join(".cache"),
    ))
}

pub(crate) fn locator_for(
    root: &Path,
    store: &PackageStore,
) -> Result<Option<ModuleLocator>, String> {
    let lock_path = root.join(PACKAGE_LOCK_FILE);
    if !lock_path.is_file() {
        return Ok(None);
    }
    let lockfile = Lockfile::read(&lock_path)
        .map_err(|error| format!("{}: {error}; run: pkg install", lock_path.display()))?;
    Ok(Some(ModuleLocator::for_root(lockfile, store.clone())))
}

pub(crate) fn prepare(home: &Path, store: &PackageStore) -> Result<Prepared, PrepareError> {
    // Tests must never migrate or seed the developer's real home directory.
    #[cfg(test)]
    assert!(
        std::env::var_os("HOME").as_deref() != Some(home.as_os_str()),
        "test attempted to prepare the real HOME ({}); set HOME to a temp dir first",
        home.display()
    );
    let root = root_for_home(home);
    if root.join(PACKAGE_MANIFEST_FILE).is_file() {
        return Ok(Prepared::AlreadyPackage);
    }
    if root.join("sparsh.spar").is_file() {
        migrate(home, &root, store)
    } else {
        seed(&root, store)?;
        Ok(Prepared::Seeded)
    }
}

fn migrate(home: &Path, root: &Path, store: &PackageStore) -> Result<Prepared, PrepareError> {
    let backup = backup_for_home(home);
    if backup.exists() {
        return Ok(Prepared::LegacyKept {
            notice: BACKUP_EXISTS_NOTICE.to_string(),
        });
    }
    if let Err(error) = copy_dir_recursive(root, &backup) {
        let _ = std::fs::remove_dir_all(&backup);
        return Err(step_error("backup", error));
    }
    match migrate_in_place(root, store) {
        Ok(()) => Ok(Prepared::Migrated),
        Err(error) => {
            restore(home, root, &backup)?;
            Err(error)
        }
    }
}

fn migrate_in_place(root: &Path, store: &PackageStore) -> Result<(), PrepareError> {
    let src = root.join("src");
    std::fs::create_dir_all(&src).map_err(|error| step_error("create src/", error))?;
    let mut moves = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|error| step_error("read directory", error))? {
        let entry = entry.map_err(|error| step_error("read directory", error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "src"
            || name.starts_with('.')
            || name == PACKAGE_MANIFEST_FILE
            || name == PACKAGE_LOCK_FILE
        {
            continue;
        }
        let path = entry.path();
        let movable = if path.is_file() {
            path.extension()
                .is_some_and(|extension| extension == "spar")
        } else {
            path.is_dir() && contains_spar(&path)
        };
        if movable {
            let target = if name == "sparsh.spar" {
                src.join("config.spar")
            } else {
                src.join(&name)
            };
            moves.push((path, target));
        }
    }
    for (from, to) in moves {
        if to.exists() {
            return Err(step_error(
                "move files",
                format!("{} already exists", to.display()),
            ));
        }
        std::fs::rename(&from, &to).map_err(|error| step_error("move files", error))?;
    }
    write_manifest_and_lock(root, store)
}

fn seed(root: &Path, store: &PackageStore) -> Result<(), PrepareError> {
    let entry = root.join("src/config.spar");
    std::fs::create_dir_all(root.join("src")).map_err(|error| step_error("create src/", error))?;
    if !entry.exists() {
        std::fs::write(&entry, TEMPLATE).map_err(|error| step_error("write config.spar", error))?;
    }
    write_manifest_and_lock(root, store)
}

fn write_manifest_and_lock(root: &Path, store: &PackageStore) -> Result<(), PrepareError> {
    commands::init(root, PACKAGE_NAME, PackageKind::Config)
        .map_err(|error| step_error("write manifest", error))?;
    commands::install(
        root,
        &GitCommandProvider::default(),
        NetworkPolicy::Offline,
        store,
    )
    .map_err(|error| step_error("write lockfile", error))?;
    Ok(())
}

fn restore(home: &Path, root: &Path, backup: &Path) -> Result<(), PrepareError> {
    let aside = home.join(".sparsh.failed-migration");
    if aside.exists() {
        return Err(step_error(
            "restore",
            format!(
                "{} already exists; original kept in {}",
                aside.display(),
                backup.display()
            ),
        ));
    }
    std::fs::rename(root, &aside).map_err(|error| step_error("restore", error))?;
    copy_dir_recursive(backup, root).map_err(|error| step_error("restore", error))
}

fn contains_spar(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            contains_spar(&path)
        } else {
            path.extension()
                .is_some_and(|extension| extension == "spar")
        }
    })
}

fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir_recursive(&source, &target)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_store(dir: &Path) -> PackageStore {
        PackageStore::new(StorePaths::new(
            dir.join("store-data"),
            dir.join("store-cache"),
        ))
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn flat_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".sparsh");
        write(&root.join("sparsh.spar"), "var a: int = 1;\n");
        write(
            &root.join("functions.spar"),
            "function f() -> int { return 1; };\n",
        );
        write(&root.join("sparsh-types.spar"), "// types\n");
        write(&root.join("lib/util.spar"), "var u: int = 2;\n");
        write(&root.join("notes.txt"), "keep me\n");
        write(&root.join(".hidden"), "dot\n");
        home
    }

    #[test]
    fn migrates_flat_layout_into_a_package() {
        let home = flat_home();
        let store = test_store(home.path());
        let outcome = prepare(home.path(), &store).unwrap();
        assert_eq!(outcome, Prepared::Migrated);

        let root = home.path().join(".sparsh");
        assert_eq!(
            fs::read_to_string(root.join("src/config.spar")).unwrap(),
            "var a: int = 1;\n"
        );
        assert!(root.join("src/functions.spar").is_file());
        assert!(root.join("src/sparsh-types.spar").is_file());
        assert!(
            root.join("src/lib/util.spar").is_file(),
            "directories holding .spar files move too"
        );
        assert!(root.join("notes.txt").is_file(), "non-spar files stay");
        assert!(root.join(".hidden").is_file(), "dotfiles stay");
        assert!(!root.join("sparsh.spar").exists());

        let manifest_path = root.join("spar.package.spar");
        let manifest = spar::package::PackageManifest::parse(
            &fs::read_to_string(&manifest_path).unwrap(),
            &manifest_path,
        )
        .unwrap();
        assert_eq!(manifest.name, "sparsh-config");
        assert_eq!(manifest.kind.as_str(), "config");
        assert!(root.join("spar.package.lock.spar").is_file());
    }

    #[test]
    fn backup_is_a_byte_identical_copy_of_the_original() {
        let home = flat_home();
        prepare(home.path(), &test_store(home.path())).unwrap();
        let backup = backup_for_home(home.path());
        assert_eq!(
            fs::read_to_string(backup.join("sparsh.spar")).unwrap(),
            "var a: int = 1;\n"
        );
        assert_eq!(
            fs::read_to_string(backup.join("lib/util.spar")).unwrap(),
            "var u: int = 2;\n"
        );
        assert_eq!(
            fs::read_to_string(backup.join("notes.txt")).unwrap(),
            "keep me\n"
        );
        assert!(
            !backup.join("spar.package.spar").exists(),
            "backup predates the manifest"
        );
    }

    #[test]
    fn migration_is_idempotent() {
        let home = flat_home();
        let store = test_store(home.path());
        prepare(home.path(), &store).unwrap();
        assert_eq!(
            prepare(home.path(), &store).unwrap(),
            Prepared::AlreadyPackage
        );
    }

    #[test]
    fn existing_backup_blocks_migration_and_keeps_the_flat_layout() {
        let home = flat_home();
        fs::create_dir(backup_for_home(home.path())).unwrap();
        let outcome = prepare(home.path(), &test_store(home.path())).unwrap();
        assert_eq!(
            outcome,
            Prepared::LegacyKept {
                notice: BACKUP_EXISTS_NOTICE.to_string()
            }
        );
        let root = home.path().join(".sparsh");
        assert!(root.join("sparsh.spar").is_file());
        assert!(!root.join("spar.package.spar").exists());
        assert_eq!(entry_for_home(home.path()), root.join("sparsh.spar"));
    }

    #[test]
    fn fresh_install_seeds_the_template_without_a_backup() {
        let home = tempfile::tempdir().unwrap();
        let outcome = prepare(home.path(), &test_store(home.path())).unwrap();
        assert_eq!(outcome, Prepared::Seeded);
        let root = home.path().join(".sparsh");
        assert!(root.join("spar.package.spar").is_file());
        assert!(root.join("spar.package.lock.spar").is_file());
        assert!(fs::read_to_string(root.join("src/config.spar"))
            .unwrap()
            .contains("pkg add"));
        assert!(!backup_for_home(home.path()).exists());
        assert_eq!(entry_for_home(home.path()), root.join("src/config.spar"));
    }

    #[test]
    fn seeding_never_overwrites_an_existing_entry_file() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".sparsh/src/config.spar"),
            "var mine: int = 9;\n",
        );
        prepare(home.path(), &test_store(home.path())).unwrap();
        assert_eq!(
            fs::read_to_string(home.path().join(".sparsh/src/config.spar")).unwrap(),
            "var mine: int = 9;\n"
        );
    }

    #[test]
    fn failure_restores_the_original_tree() {
        let home = flat_home();
        // A file named `src` makes `create_dir_all(src)` fail.
        write(&home.path().join(".sparsh/src"), "i am a file\n");
        let error = prepare(home.path(), &test_store(home.path())).unwrap_err();
        assert!(
            error.to_string().contains("cannot migrate ~/.sparsh"),
            "{error}"
        );
        let root = home.path().join(".sparsh");
        assert_eq!(
            fs::read_to_string(root.join("sparsh.spar")).unwrap(),
            "var a: int = 1;\n"
        );
        assert!(root.join("functions.spar").is_file());
        assert!(!root.join("spar.package.spar").exists());
        assert!(
            home.path().join(".sparsh.failed-migration").exists(),
            "failed tree kept aside"
        );
    }

    #[test]
    fn locator_is_none_without_a_lockfile_and_some_with_one() {
        let home = tempfile::tempdir().unwrap();
        let store = test_store(home.path());
        let root = home.path().join(".sparsh");
        fs::create_dir_all(&root).unwrap();
        assert!(locator_for(&root, &store).unwrap().is_none());
        prepare(home.path(), &store).unwrap();
        assert!(locator_for(&root, &store).unwrap().is_some());
    }

    #[test]
    fn malformed_lockfile_is_reported_with_the_fix() {
        let home = tempfile::tempdir().unwrap();
        let store = test_store(home.path());
        prepare(home.path(), &store).unwrap();
        let root = home.path().join(".sparsh");
        fs::write(
            root.join("spar.package.lock.spar"),
            "this is not a lockfile {{{",
        )
        .unwrap();
        let message = locator_for(&root, &store)
            .err()
            .expect("malformed lock is an error");
        assert!(message.contains("spar.package.lock.spar"), "{message}");
        assert!(message.contains("pkg install"), "{message}");
    }
}
