use std::path::{Path, PathBuf};

use spar::package::{commands, GitCommandProvider, NetworkPolicy, PackageProvider, PackageStore};

use super::{
    error, success, usage_error, BuiltinContext, BuiltinError, BuiltinRegistry, BuiltinResult,
};

const USAGE: &str =
    "pkg add <alias> <request> | remove <alias> | install [--offline] | update [alias] | tree";

pub(super) struct PkgOutcome {
    pub text: String,
    pub changed: bool,
}

pub(super) fn pkg(
    args: &[String],
    context: &mut BuiltinContext<'_>,
    _: &BuiltinRegistry,
) -> BuiltinResult {
    let Some(home) = context.services.environment.get("HOME").map(PathBuf::from) else {
        return Err(error("pkg: HOME is not set"));
    };
    let root = crate::config_home::root_for_home(&home);
    let store = crate::config_home::store_for(&context.services.environment);
    let outcome = run(args, &root, &store, &GitCommandProvider::default())?;
    if outcome.changed {
        context.requested_reload_config = true;
    }
    Ok(success(Some(outcome.text)))
}

pub(super) fn run(
    args: &[String],
    root: &Path,
    store: &PackageStore,
    provider: &dyn PackageProvider,
) -> Result<PkgOutcome, BuiltinError> {
    let package_error = |failure: spar::package::PackageError| error(format!("pkg: {failure}"));
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["add", alias, request] => {
            let lock = commands::add(root, alias, request, provider, NetworkPolicy::Allow, store)
                .map_err(package_error)?;
            Ok(PkgOutcome {
                text: format!(
                    "added '{alias}' — {} package(s) locked\n",
                    lock.packages.len()
                ),
                changed: true,
            })
        }
        ["remove", alias] => {
            let lock = commands::remove(root, alias, provider, NetworkPolicy::Allow, store)
                .map_err(package_error)?;
            Ok(PkgOutcome {
                text: format!(
                    "removed '{alias}' — {} package(s) locked\n",
                    lock.packages.len()
                ),
                changed: true,
            })
        }
        ["install"] | ["install", "--offline"] => {
            let network = if words.len() == 2 {
                NetworkPolicy::Offline
            } else {
                NetworkPolicy::Allow
            };
            let lock = commands::install(root, provider, network, store).map_err(package_error)?;
            Ok(PkgOutcome {
                text: format!("installed — {} package(s) locked\n", lock.packages.len()),
                changed: true,
            })
        }
        ["update"] | ["update", _] => {
            let alias = words.get(1).copied();
            let lock = commands::update(root, alias, provider, store).map_err(package_error)?;
            Ok(PkgOutcome {
                text: format!("updated — {} package(s) locked\n", lock.packages.len()),
                changed: true,
            })
        }
        ["tree"] => {
            let tree = commands::tree(root).map_err(package_error)?;
            let text = if tree.is_empty() {
                "no dependencies\n".to_string()
            } else {
                tree
            };
            Ok(PkgOutcome {
                text,
                changed: false,
            })
        }
        _ => Err(usage_error(USAGE)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spar::package::StorePaths;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, PackageStore) {
        let home = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tools.path().join("src")).unwrap();
        std::fs::write(
            tools.path().join("spar.package.spar"),
            "struct Package: SparPackage {\n    name = \"my-tools\";\n    version = \"1.0.0\";\n    kind = \"library\";\n};\n",
        )
        .unwrap();
        std::fs::write(
            tools.path().join("src/lib.spar"),
            "function dismantler() -> int { return 42; };\n",
        )
        .unwrap();
        let store = PackageStore::new(StorePaths::new(
            home.path().join("data"),
            home.path().join("cache"),
        ));
        crate::config_home::prepare(home.path(), &store).unwrap();
        (home, tools, store)
    }

    fn run_ok(root: &Path, store: &PackageStore, values: &[&str]) -> PkgOutcome {
        run(&args(values), root, store, &GitCommandProvider::default()).unwrap()
    }

    #[test]
    fn add_writes_manifest_and_lock_and_requests_reload() {
        let (home, tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let request = format!("path:{}", tools.path().display());
        let outcome = run_ok(&root, &store, &["add", "myTools", &request]);
        assert!(outcome.changed);
        assert!(outcome.text.contains("myTools"), "{}", outcome.text);
        let manifest = std::fs::read_to_string(root.join("spar.package.spar")).unwrap();
        assert!(manifest.contains("myTools"), "{manifest}");
        let tree = run_ok(&root, &store, &["tree"]);
        assert!(!tree.changed);
        assert!(tree.text.contains("myTools"), "{}", tree.text);
    }

    #[test]
    fn remove_drops_the_dependency() {
        let (home, tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let request = format!("path:{}", tools.path().display());
        run_ok(&root, &store, &["add", "myTools", &request]);
        let outcome = run_ok(&root, &store, &["remove", "myTools"]);
        assert!(outcome.changed);
        assert!(!run_ok(&root, &store, &["tree"]).text.contains("myTools"));
    }

    #[test]
    fn install_offline_succeeds_for_a_locked_path_dependency() {
        let (home, tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let request = format!("path:{}", tools.path().display());
        run_ok(&root, &store, &["add", "myTools", &request]);
        let outcome = run_ok(&root, &store, &["install", "--offline"]);
        assert!(outcome.changed);
    }

    #[test]
    fn update_reresolves() {
        let (home, tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let request = format!("path:{}", tools.path().display());
        run_ok(&root, &store, &["add", "myTools", &request]);
        assert!(run_ok(&root, &store, &["update"]).changed);
        assert!(run_ok(&root, &store, &["update", "myTools"]).changed);
    }

    #[test]
    fn add_with_a_bad_request_changes_nothing() {
        let (home, _tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let before = std::fs::read_to_string(root.join("spar.package.spar")).unwrap();
        let error = run(
            &args(&["add", "x", "not-a-request"]),
            &root,
            &store,
            &GitCommandProvider::default(),
        )
        .err()
        .expect("bad request must fail");
        assert_eq!(error.status, 1);
        assert_eq!(
            std::fs::read_to_string(root.join("spar.package.spar")).unwrap(),
            before
        );
    }

    #[test]
    fn usage_errors() {
        let (home, _tools, store) = fixture();
        let root = home.path().join(".sparsh");
        for bad in [
            vec![],
            vec!["add", "only-alias"],
            vec!["remove"],
            vec!["frobnicate"],
            vec!["install", "--nope"],
        ] {
            let error = run(&args(&bad), &root, &store, &GitCommandProvider::default())
                .err()
                .expect("usage error");
            assert_eq!(error.status, 2, "{bad:?}");
            assert!(error.message.starts_with("usage: pkg"), "{}", error.message);
        }
    }
}
