use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::path::PathService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolutionMode {
    Normal,
    BypassAlias,
    BuiltinOnly,
    ExternalOnly,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandResolution {
    Builtin { name: String },
    External { path: PathBuf, cached: bool },
    Missing { name: String },
}

#[derive(Debug, Clone)]
struct CacheEntry {
    path: PathBuf,
    generation: u64,
}

#[derive(Debug, Default)]
pub(crate) struct CommandResolver {
    cache: BTreeMap<String, CacheEntry>,
}

impl CommandResolver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn external(
        &mut self,
        program: &str,
        path: &PathService,
        cwd: &Path,
    ) -> Result<PathBuf, String> {
        if program.contains('/') {
            return explicit_path(program, cwd);
        }
        if let Some(entry) = self.cache.get(program) {
            if entry.generation == path.generation() && is_executable(&entry.path) {
                return Ok(entry.path.clone());
            }
        }
        self.cache.remove(program);
        let resolved = search(program, path.directories(), cwd)?;
        self.cache.insert(
            program.to_string(),
            CacheEntry {
                path: resolved.clone(),
                generation: path.generation(),
            },
        );
        Ok(resolved)
    }

    pub(crate) fn external_uncached(
        &self,
        program: &str,
        directories: &[PathBuf],
        cwd: &Path,
    ) -> Result<PathBuf, String> {
        if program.contains('/') {
            explicit_path(program, cwd)
        } else {
            search(program, directories, cwd)
        }
    }

    pub(crate) fn clear(&mut self) {
        self.cache.clear();
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.cache
            .iter()
            .map(|(name, entry)| (name.as_str(), entry.path.as_path()))
    }

    pub(crate) fn suggestions(
        &self,
        missing: &str,
        path: &PathService,
        additional: &[String],
    ) -> Vec<String> {
        let mut names = additional.iter().cloned().collect::<BTreeSet<_>>();
        for directory in path.directories() {
            let directory = if directory.as_os_str().is_empty() {
                Path::new(".")
            } else {
                directory.as_path()
            };
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                if is_executable(&entry.path()) {
                    if let Some(name) = entry.file_name().to_str() {
                        names.insert(name.to_string());
                    }
                }
            }
        }

        let maximum = if missing.len() >= 8 { 3 } else { 2 };
        let mut matches = names
            .into_iter()
            .filter_map(|name| {
                let distance = edit_distance(missing, &name);
                (distance <= maximum).then_some((distance, name))
            })
            .collect::<Vec<_>>();
        matches.sort();
        matches.into_iter().take(5).map(|(_, name)| name).collect()
    }
}

fn explicit_path(program: &str, cwd: &Path) -> Result<PathBuf, String> {
    let path = Path::new(program);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    if is_executable(&path) {
        Ok(path)
    } else if path.exists() {
        Err(format!("not executable: `{}`", path.display()))
    } else {
        Err(format!("executable not found: `{}`", path.display()))
    }
}

fn search(program: &str, directories: &[PathBuf], cwd: &Path) -> Result<PathBuf, String> {
    for directory in directories {
        let directory = if directory.as_os_str().is_empty() {
            cwd.to_path_buf()
        } else if directory.is_absolute() {
            directory.clone()
        } else {
            cwd.join(directory)
        };
        let candidate = directory.join(program);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!("command not found: `{program}`"))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_character) in left.chars().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_character) in right.iter().enumerate() {
            current.push(std::cmp::min(
                std::cmp::min(current[right_index] + 1, previous[right_index + 1] + 1),
                previous[right_index] + usize::from(left_character != *right_character),
            ));
        }
        previous = current;
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::CommandResolver;
    use crate::path::PathService;

    fn executable_dir(name: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join(name);
        std::fs::write(&executable, b"executable fixture").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        directory
    }

    #[test]
    fn path_generation_invalidates_cached_resolution() {
        let first = executable_dir("tool");
        let second = executable_dir("tool");
        let mut resolver = CommandResolver::new();
        let mut path = PathService::from_directories([first.path()]);
        assert_eq!(
            resolver.external("tool", &path, Path::new("/")).unwrap(),
            first.path().join("tool")
        );

        path.replace([second.path()]);

        assert_eq!(
            resolver.external("tool", &path, Path::new("/")).unwrap(),
            second.path().join("tool")
        );
    }

    #[test]
    fn stale_cache_entry_is_researched() {
        let first = executable_dir("tool");
        let second = executable_dir("tool");
        let mut resolver = CommandResolver::new();
        let path = PathService::from_directories([first.path(), second.path()]);
        assert_eq!(
            resolver.external("tool", &path, Path::new("/")).unwrap(),
            first.path().join("tool")
        );
        std::fs::remove_file(first.path().join("tool")).unwrap();

        assert_eq!(
            resolver.external("tool", &path, Path::new("/")).unwrap(),
            second.path().join("tool")
        );
    }

    #[test]
    fn explicit_path_requires_regular_executable_file() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("tool");
        std::fs::write(&file, b"not executable").unwrap();
        let mut resolver = CommandResolver::new();
        let path = PathService::from_directories(["/bin"]);

        let error = resolver
            .external(file.to_str().unwrap(), &path, Path::new("/"))
            .unwrap_err();

        assert!(error.contains("not executable"), "{error}");
    }

    #[test]
    fn temporary_path_bypasses_session_cache() {
        let cached = executable_dir("tool");
        let temporary = executable_dir("tool");
        let mut resolver = CommandResolver::new();
        let path = PathService::from_directories([cached.path()]);
        resolver.external("tool", &path, Path::new("/")).unwrap();

        let resolved = resolver
            .external_uncached("tool", &[temporary.path().to_path_buf()], Path::new("/"))
            .unwrap();

        assert_eq!(resolved, temporary.path().join("tool"));
    }

    #[test]
    fn suggestions_are_bounded_and_include_nearby_path_names() {
        let directory = executable_dir("cargo");
        let resolver = CommandResolver::new();
        let path = PathService::from_directories([directory.path()]);

        assert_eq!(resolver.suggestions("cargoo", &path, &[]), ["cargo"]);
        assert!(resolver.suggestions("unrelated", &path, &[]).is_empty());
    }
}
