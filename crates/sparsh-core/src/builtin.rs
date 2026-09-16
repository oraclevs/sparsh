#[derive(Debug)]
pub struct BuiltinMetadata {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub usage: &'static str,
    pub mutates_shell_state: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BuiltinOutput {
    pub stdout: Option<String>,
    pub status: i32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BuiltinError {
    pub message: String,
    pub status: i32,
}

pub(crate) struct BuiltinContext {
    pub last_status: i32,
    pub requested_exit: Option<i32>,
}

type Handler = fn(&[String], &mut BuiltinContext) -> Result<BuiltinOutput, BuiltinError>;

struct Builtin {
    metadata: BuiltinMetadata,
    handler: Handler,
}

pub struct BuiltinRegistry {
    entries: Vec<Builtin>,
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        Self {
            entries: vec![
                Builtin {
                    metadata: BuiltinMetadata {
                        name: "cd",
                        aliases: &[],
                        description: "Change the current working directory",
                        usage: "cd [directory]",
                        mutates_shell_state: true,
                    },
                    handler: cd,
                },
                Builtin {
                    metadata: BuiltinMetadata {
                        name: "pwd",
                        aliases: &[],
                        description: "Print the current working directory",
                        usage: "pwd",
                        mutates_shell_state: false,
                    },
                    handler: pwd,
                },
                Builtin {
                    metadata: BuiltinMetadata {
                        name: "exit",
                        aliases: &[],
                        description: "Exit the shell",
                        usage: "exit [status]",
                        mutates_shell_state: true,
                    },
                    handler: exit,
                },
            ],
        }
    }

    pub fn metadata(&self) -> impl Iterator<Item = &BuiltinMetadata> {
        self.entries.iter().map(|entry| &entry.metadata)
    }

    pub fn find(&self, name: &str) -> Option<&BuiltinMetadata> {
        self.entries
            .iter()
            .find(|entry| entry.metadata.name == name || entry.metadata.aliases.contains(&name))
            .map(|entry| &entry.metadata)
    }

    pub(crate) fn execute(
        &self,
        name: &str,
        arguments: &[String],
        context: &mut BuiltinContext,
    ) -> Result<BuiltinOutput, BuiltinError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.metadata.name == name || entry.metadata.aliases.contains(&name))
            .expect("execute is only called after builtin lookup");
        (entry.handler)(arguments, context)
    }
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn cd(arguments: &[String], _context: &mut BuiltinContext) -> Result<BuiltinOutput, BuiltinError> {
    if arguments.len() > 1 {
        return Err(usage_error("cd [directory]"));
    }
    let target = match arguments.first() {
        Some(path) => path.clone(),
        None => std::env::var("HOME").map_err(|_| BuiltinError {
            message: "cd: HOME is not set".into(),
            status: 1,
        })?,
    };
    std::env::set_current_dir(&target).map_err(|error| BuiltinError {
        message: format!("cd: {target}: {error}"),
        status: 1,
    })?;
    Ok(success(None))
}

fn pwd(arguments: &[String], _context: &mut BuiltinContext) -> Result<BuiltinOutput, BuiltinError> {
    if !arguments.is_empty() {
        return Err(usage_error("pwd"));
    }
    let directory = std::env::current_dir().map_err(|error| BuiltinError {
        message: format!("pwd: {error}"),
        status: 1,
    })?;
    Ok(success(Some(format!("{}\n", directory.display()))))
}

fn exit(arguments: &[String], context: &mut BuiltinContext) -> Result<BuiltinOutput, BuiltinError> {
    if arguments.len() > 1 {
        return Err(usage_error("exit [status]"));
    }
    let status = match arguments.first() {
        None => context.last_status,
        Some(value) => value
            .parse::<u8>()
            .map(i32::from)
            .map_err(|_| usage_error("exit [status]"))?,
    };
    context.requested_exit = Some(status);
    Ok(success(None))
}

fn success(stdout: Option<String>) -> BuiltinOutput {
    BuiltinOutput { stdout, status: 0 }
}

fn usage_error(usage: &str) -> BuiltinError {
    BuiltinError {
        message: format!("usage: {usage}"),
        status: 2,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{BuiltinContext, BuiltinRegistry};
    use crate::PROCESS_STATE;

    struct CwdGuard(PathBuf);

    impl CwdGuard {
        fn capture() -> Self {
            Self(std::env::current_dir().unwrap())
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).unwrap();
        }
    }

    fn context(last_status: i32) -> BuiltinContext {
        BuiltinContext {
            last_status,
            requested_exit: None,
        }
    }

    #[test]
    fn registry_exposes_metadata_for_cd_pwd_and_exit() {
        let registry = BuiltinRegistry::new();
        let metadata = registry.metadata().collect::<Vec<_>>();

        assert_eq!(metadata.len(), 3);
        assert_eq!(metadata[0].name, "cd");
        assert!(metadata[0].mutates_shell_state);
        assert_eq!(metadata[1].name, "pwd");
        assert!(!metadata[1].mutates_shell_state);
        assert_eq!(metadata[2].name, "exit");
        assert!(metadata[2].mutates_shell_state);
        assert!(metadata.iter().all(|entry| !entry.description.is_empty()));
        assert!(metadata.iter().all(|entry| !entry.usage.is_empty()));
    }

    #[test]
    fn pwd_rejects_arguments_and_returns_current_directory() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let registry = BuiltinRegistry::new();
        let mut context = context(0);

        let output = registry.execute("pwd", &[], &mut context).unwrap();
        assert_eq!(
            output.stdout,
            Some(format!("{}\n", std::env::current_dir().unwrap().display()))
        );
        assert_eq!(output.status, 0);

        let error = registry
            .execute("pwd", &["extra".into()], &mut context)
            .unwrap_err();
        assert_eq!(error.status, 2);
        assert!(error.message.contains("usage: pwd"));
    }

    #[test]
    fn cd_changes_directory_and_failure_preserves_it() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let target = tempfile::tempdir().unwrap();
        let registry = BuiltinRegistry::new();
        let mut context = context(0);

        let output = registry
            .execute(
                "cd",
                &[target.path().to_string_lossy().into_owned()],
                &mut context,
            )
            .unwrap();
        assert_eq!(output.status, 0);
        assert_eq!(std::env::current_dir().unwrap(), target.path());

        let before_failure = std::env::current_dir().unwrap();
        let error = registry
            .execute("cd", &["definitely-not-a-directory".into()], &mut context)
            .unwrap_err();
        assert_eq!(error.status, 1);
        assert_eq!(std::env::current_dir().unwrap(), before_failure);
    }

    #[test]
    fn cd_without_an_argument_uses_home() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let target = tempfile::tempdir().unwrap();
        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", target.path());
        let registry = BuiltinRegistry::new();
        let mut context = context(0);

        let result = registry.execute("cd", &[], &mut context);

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        result.unwrap();
        assert_eq!(std::env::current_dir().unwrap(), target.path());
    }

    #[test]
    fn exit_defaults_to_last_status_and_validates_explicit_status() {
        let registry = BuiltinRegistry::new();
        let mut inherited = context(19);
        registry.execute("exit", &[], &mut inherited).unwrap();
        assert_eq!(inherited.requested_exit, Some(19));

        let mut explicit = context(0);
        registry
            .execute("exit", &["255".into()], &mut explicit)
            .unwrap();
        assert_eq!(explicit.requested_exit, Some(255));

        for arguments in [
            vec!["256".into()],
            vec!["bad".into()],
            vec!["1".into(), "2".into()],
        ] {
            let error = registry
                .execute("exit", &arguments, &mut context(0))
                .unwrap_err();
            assert_eq!(error.status, 2);
            assert!(error.message.contains("usage: exit [status]"));
        }
    }
}
