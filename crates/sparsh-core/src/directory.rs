use std::path::{Path, PathBuf};

use crate::environment::EnvironmentService;

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectorySnapshot {
    pub current: PathBuf,
    pub previous: Option<PathBuf>,
    pub stack: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct DirectoryService {
    current: PathBuf,
    previous: Option<PathBuf>,
    stack: Vec<PathBuf>,
}

impl DirectoryService {
    pub(crate) fn new() -> Result<Self, String> {
        Ok(Self {
            current: std::env::current_dir().map_err(|error| error.to_string())?,
            previous: None,
            stack: Vec::new(),
        })
    }

    pub(crate) fn current(&self) -> &Path {
        &self.current
    }

    pub(crate) fn set_current_path(
        &mut self,
        target: &Path,
        environment: &mut EnvironmentService,
    ) -> Result<(), String> {
        let target = std::fs::canonicalize(target)
            .map_err(|error| format!("cd: {}: {error}", target.display()))?;
        self.commit_change(target, environment)
    }

    #[cfg(test)]
    pub(crate) fn stack(&self) -> &[PathBuf] {
        &self.stack
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> DirectorySnapshot {
        DirectorySnapshot {
            current: self.current.clone(),
            previous: self.previous.clone(),
            stack: self.stack.clone(),
        }
    }

    pub(crate) fn change(
        &mut self,
        target: &str,
        environment: &mut EnvironmentService,
    ) -> Result<(), String> {
        if target == "-" {
            self.previous(environment)?;
            return Ok(());
        }
        let target = self.resolve(target, environment)?;
        self.commit_change(target, environment)
    }

    pub(crate) fn previous(
        &mut self,
        environment: &mut EnvironmentService,
    ) -> Result<PathBuf, String> {
        let target = self
            .previous
            .clone()
            .ok_or_else(|| "cd: previous directory is not set".to_string())?;
        self.commit_change(target, environment)?;
        Ok(self.current.clone())
    }

    pub(crate) fn push(
        &mut self,
        target: Option<&str>,
        environment: &mut EnvironmentService,
    ) -> Result<(), String> {
        let old = self.current.clone();
        match target {
            Some(target) => {
                let target = self.resolve(target, environment)?;
                self.commit_change(target, environment)?;
                self.stack.insert(0, old);
            }
            None => {
                let target = self
                    .stack
                    .first()
                    .cloned()
                    .ok_or_else(|| "pushd: directory stack is empty".to_string())?;
                self.commit_change(target, environment)?;
                self.stack[0] = old;
            }
        }
        Ok(())
    }

    pub(crate) fn pop(&mut self, environment: &mut EnvironmentService) -> Result<(), String> {
        let target = self
            .stack
            .first()
            .cloned()
            .ok_or_else(|| "popd: directory stack is empty".to_string())?;
        self.commit_change(target, environment)?;
        self.stack.remove(0);
        Ok(())
    }

    pub(crate) fn render(&self) -> String {
        let mut directories = Vec::with_capacity(self.stack.len() + 1);
        directories.push(self.current.display().to_string());
        directories.extend(
            self.stack
                .iter()
                .map(|directory| directory.display().to_string()),
        );
        format!("{}\n", directories.join(" "))
    }

    fn resolve(&self, target: &str, environment: &EnvironmentService) -> Result<PathBuf, String> {
        let target = if target == "~" {
            PathBuf::from(
                environment
                    .get("HOME")
                    .ok_or_else(|| "cd: HOME is not set".to_string())?,
            )
        } else if let Some(rest) = target.strip_prefix("~/") {
            PathBuf::from(
                environment
                    .get("HOME")
                    .ok_or_else(|| "cd: HOME is not set".to_string())?,
            )
            .join(rest)
        } else {
            let path = Path::new(target);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.current.join(path)
            }
        };
        std::fs::canonicalize(&target).map_err(|error| format!("cd: {}: {error}", target.display()))
    }

    fn commit_change(
        &mut self,
        target: PathBuf,
        environment: &mut EnvironmentService,
    ) -> Result<(), String> {
        if !target.is_dir() {
            return Err(format!("cd: {}: not a directory", target.display()));
        }
        let old = std::mem::replace(&mut self.current, target);
        self.previous = Some(old.clone());
        environment.set_os("OLDPWD", old.into_os_string());
        environment.set_os("PWD", self.current.clone().into_os_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

    use super::DirectoryService;
    use crate::environment::EnvironmentService;
    use crate::PROCESS_STATE;

    struct ProcessStateGuard {
        cwd: PathBuf,
        home: Option<OsString>,
    }

    impl ProcessStateGuard {
        fn capture() -> Self {
            Self {
                cwd: std::env::current_dir().unwrap(),
                home: std::env::var_os("HOME"),
            }
        }
    }

    impl Drop for ProcessStateGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.cwd).unwrap();
            match &self.home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn environment(home: &OsStr) -> EnvironmentService {
        EnvironmentService::from_pairs([("HOME", home)])
    }

    #[test]
    fn failed_change_preserves_all_directory_state() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _guard = ProcessStateGuard::capture();
        let root = tempfile::tempdir().unwrap();
        std::env::set_current_dir(root.path()).unwrap();
        let mut environment = environment(root.path().as_os_str());
        let mut directories = DirectoryService::new().unwrap();
        let before = directories.snapshot();

        assert!(directories
            .change("missing-directory", &mut environment)
            .is_err());

        assert_eq!(directories.snapshot(), before);
        assert_eq!(std::env::current_dir().unwrap(), root.path());
    }

    #[test]
    fn pushd_popd_and_cd_dash_update_pwd_oldpwd_atomically() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _guard = ProcessStateGuard::capture();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::env::set_current_dir(first.path()).unwrap();
        let mut environment = environment(first.path().as_os_str());
        let mut directories = DirectoryService::new().unwrap();

        directories
            .push(Some(second.path().to_str().unwrap()), &mut environment)
            .unwrap();
        assert_eq!(directories.current(), second.path());
        assert_eq!(directories.stack(), [first.path()]);
        directories.pop(&mut environment).unwrap();
        assert_eq!(directories.current(), first.path());
        directories.previous(&mut environment).unwrap();
        assert_eq!(directories.current(), second.path());
        assert_eq!(environment.get("PWD"), Some(second.path().as_os_str()));
        assert_eq!(environment.get("OLDPWD"), Some(first.path().as_os_str()));
    }

    #[test]
    fn no_home_rejects_bare_tilde_without_mutation() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _guard = ProcessStateGuard::capture();
        let root = tempfile::tempdir().unwrap();
        std::env::set_current_dir(root.path()).unwrap();
        let mut environment = EnvironmentService::from_pairs([] as [(&str, &str); 0]);
        let mut directories = DirectoryService::new().unwrap();
        let before = directories.snapshot();

        let error = directories.change("~", &mut environment).unwrap_err();

        assert!(error.contains("HOME is not set"));
        assert_eq!(directories.snapshot(), before);
    }

    #[test]
    fn tilde_child_uses_home_directory() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _guard = ProcessStateGuard::capture();
        let home = tempfile::tempdir().unwrap();
        let child = home.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let mut environment = environment(home.path().as_os_str());
        let mut directories = DirectoryService::new().unwrap();

        directories.change("~/child", &mut environment).unwrap();

        assert_eq!(directories.current(), child);
    }

    #[test]
    fn empty_stack_operations_fail_without_mutation() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _guard = ProcessStateGuard::capture();
        let root = tempfile::tempdir().unwrap();
        std::env::set_current_dir(root.path()).unwrap();
        let mut environment = environment(root.path().as_os_str());
        let mut directories = DirectoryService::new().unwrap();
        let before = directories.snapshot();

        assert!(directories
            .pop(&mut environment)
            .unwrap_err()
            .contains("empty"));
        assert!(directories
            .push(None, &mut environment)
            .unwrap_err()
            .contains("empty"));
        assert_eq!(directories.snapshot(), before);
    }

    #[test]
    fn push_without_target_swaps_current_and_stack_head() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _guard = ProcessStateGuard::capture();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::env::set_current_dir(first.path()).unwrap();
        let mut environment = environment(first.path().as_os_str());
        let mut directories = DirectoryService::new().unwrap();
        directories
            .push(Some(second.path().to_str().unwrap()), &mut environment)
            .unwrap();

        directories.push(None, &mut environment).unwrap();

        assert_eq!(directories.current(), first.path());
        assert_eq!(directories.stack(), [second.path()]);
    }
}
