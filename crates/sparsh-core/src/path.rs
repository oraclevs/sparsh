use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct PathService {
    directories: Vec<PathBuf>,
    generation: u64,
}

impl PathService {
    pub(crate) fn from_environment(value: Option<&OsStr>) -> Self {
        Self {
            directories: value
                .map(std::env::split_paths)
                .into_iter()
                .flatten()
                .collect(),
            generation: 0,
        }
    }

    pub(crate) fn from_directories<I, P>(directories: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            directories: directories.into_iter().map(Into::into).collect(),
            generation: 0,
        }
    }

    pub(crate) fn directories(&self) -> &[PathBuf] {
        &self.directories
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn prepend(
        &mut self,
        directory: &Path,
        cwd: &Path,
        home: Option<&Path>,
    ) -> Result<(), String> {
        let directory = normalize_input(directory, cwd, home)?;
        if !self.directories.contains(&directory) {
            self.directories.insert(0, directory);
            self.bump_generation();
        }
        Ok(())
    }

    pub(crate) fn append(
        &mut self,
        directory: &Path,
        cwd: &Path,
        home: Option<&Path>,
    ) -> Result<(), String> {
        let directory = normalize_input(directory, cwd, home)?;
        if !self.directories.contains(&directory) {
            self.directories.push(directory);
            self.bump_generation();
        }
        Ok(())
    }

    pub(crate) fn remove(
        &mut self,
        directory: &Path,
        cwd: &Path,
        home: Option<&Path>,
    ) -> Result<(), String> {
        let directory = normalize_input(directory, cwd, home)?;
        let old_len = self.directories.len();
        self.directories.retain(|entry| entry != &directory);
        if self.directories.len() != old_len {
            self.bump_generation();
        }
        Ok(())
    }

    pub(crate) fn replace<I, P>(&mut self, directories: I)
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let directories = directories.into_iter().map(Into::into).collect::<Vec<_>>();
        if self.directories != directories {
            self.directories = directories;
            self.bump_generation();
        }
    }

    pub(crate) fn to_environment(&self) -> Result<OsString, String> {
        std::env::join_paths(&self.directories).map_err(|error| format!("invalid PATH: {error}"))
    }

    pub(crate) fn render(&self) -> String {
        let mut output = self
            .directories
            .iter()
            .map(|directory| directory.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if !output.is_empty() {
            output.push('\n');
        }
        output
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

fn normalize_input(directory: &Path, cwd: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
    let expanded = if directory == Path::new("~") {
        home.ok_or_else(|| "HOME is not set".to_string())?
            .to_path_buf()
    } else if let Ok(rest) = directory.strip_prefix("~") {
        let home = home.ok_or_else(|| "HOME is not set".to_string())?;
        home.join(rest)
    } else if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        cwd.join(directory)
    };
    Ok(lexical_normalize(&expanded))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !path.is_absolute() {
                    normalized.push(component);
                }
            }
            _ => normalized.push(component),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::PathService;

    #[test]
    fn mutation_preserves_order_and_changes_generation() {
        let mut path = PathService::from_directories(["/bin", "/usr/bin"]);
        let generation = path.generation();

        path.prepend(Path::new("/tools/bin"), Path::new("/work"), None)
            .unwrap();

        assert_eq!(
            path.directories(),
            [
                Path::new("/tools/bin"),
                Path::new("/bin"),
                Path::new("/usr/bin")
            ]
        );
        assert!(path.generation() > generation);
    }

    #[test]
    fn duplicate_mutation_is_a_noop_and_remove_drops_every_match() {
        let mut path = PathService::from_directories(["/bin", "/usr/bin", "/bin"]);
        let generation = path.generation();
        path.append(Path::new("/usr/bin"), Path::new("/"), None)
            .unwrap();
        assert_eq!(path.generation(), generation);

        path.remove(Path::new("/bin"), Path::new("/"), None)
            .unwrap();
        assert_eq!(path.directories(), [Path::new("/usr/bin")]);
    }

    #[test]
    fn relative_and_tilde_paths_expand_against_explicit_roots() {
        let mut path = PathService::from_directories(std::iter::empty::<PathBuf>());

        path.append(
            Path::new("tools"),
            Path::new("/work"),
            Some(Path::new("/home/test")),
        )
        .unwrap();
        path.prepend(
            Path::new("~/bin"),
            Path::new("/work"),
            Some(Path::new("/home/test")),
        )
        .unwrap();

        assert_eq!(
            path.directories(),
            [Path::new("/home/test/bin"), Path::new("/work/tools")]
        );
    }

    #[test]
    fn environment_round_trip_preserves_empty_entry() {
        let path = PathService::from_environment(Some(OsStr::new(":/bin")));

        assert_eq!(path.directories(), [Path::new(""), Path::new("/bin")]);
        assert_eq!(path.to_environment().unwrap(), OsStr::new(":/bin"));
    }

    #[test]
    fn invalid_join_is_reported() {
        let path = PathService::from_directories(["/valid", "/bad:entry"]);
        assert!(path.to_environment().unwrap_err().contains("PATH"));
    }
}
