use std::fmt;

use crate::builtin::{BuiltinError, BuiltinOutput, BuiltinRegistry};
use crate::dispatch::{classify, Dispatch};
use crate::execute::execute_plan;

#[derive(Debug)]
pub enum ShellResult {
    Empty,
    Value(spar::ConfigValue),
    Builtin(BuiltinOutput),
    Process(spar::ShellPlanOutcome),
    Exit(i32),
}

#[derive(Debug)]
pub enum ShellError {
    Spar(Vec<spar::SparError>),
    Builtin(BuiltinError),
    Process { message: String, status: i32 },
}

impl ShellError {
    pub fn status(&self) -> i32 {
        match self {
            Self::Spar(_) => 1,
            Self::Builtin(error) => error.status,
            Self::Process { status, .. } => *status,
        }
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spar(errors) => {
                for (index, error) in errors.iter().enumerate() {
                    if index > 0 {
                        writeln!(formatter)?;
                    }
                    write!(formatter, "{error}")?;
                }
                Ok(())
            }
            Self::Builtin(error) => formatter.write_str(&error.message),
            Self::Process { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ShellError {}

pub struct ShellSession {
    spar: spar::Session,
    builtins: BuiltinRegistry,
    should_exit: bool,
    last_status: i32,
}

impl ShellSession {
    pub fn new() -> Self {
        Self {
            spar: spar::Engine::default().session(),
            builtins: BuiltinRegistry::new(),
            should_exit: false,
            last_status: 0,
        }
    }

    pub fn submit(&mut self, input: &str) -> Result<ShellResult, ShellError> {
        let dispatch = classify(input, &self.spar);
        if matches!(dispatch, Dispatch::Empty) {
            return Ok(ShellResult::Empty);
        }

        let result = match dispatch {
            Dispatch::Empty => unreachable!("empty input returned above"),
            Dispatch::SparFragment(fragment) => {
                let terminated;
                let fragment = if fragment.trim_end().ends_with(')') {
                    terminated = format!("{fragment};");
                    &terminated
                } else {
                    fragment
                };
                self.spar
                    .eval(fragment)
                    .map(|()| ShellResult::Empty)
                    .map_err(ShellError::Spar)
            }
            Dispatch::SparValue(name) => Ok(ShellResult::Value(
                self.spar
                    .value(name)
                    .expect("dispatch verified the variable exists")
                    .clone(),
            )),
            Dispatch::Command(command) => spar::parse_shell_plan(command)
                .map_err(|error| ShellError::Spar(vec![error]))
                .and_then(|plan| execute_plan(&plan, &self.builtins, self.last_status)),
        };

        match result {
            Ok(result) => {
                self.last_status = match &result {
                    ShellResult::Empty | ShellResult::Value(_) => 0,
                    ShellResult::Builtin(output) => output.status,
                    ShellResult::Process(outcome) => outcome.exit_code,
                    ShellResult::Exit(status) => *status,
                };
                if matches!(result, ShellResult::Exit(_)) {
                    self.should_exit = true;
                }
                Ok(result)
            }
            Err(error) => {
                self.last_status = error.status();
                Err(error)
            }
        }
    }

    pub fn last_status(&self) -> i32 {
        self.last_status
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }
}

impl Default for ShellSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use spar::ConfigValue;

    use super::{ShellError, ShellResult, ShellSession};
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

    #[test]
    fn spar_variables_persist_and_bare_lookup_returns_the_typed_value() {
        let mut session = ShellSession::new();
        assert!(matches!(
            session.submit("var project: str = \"spar\";").unwrap(),
            ShellResult::Empty
        ));
        let ShellResult::Value(value) = session.submit("project").unwrap() else {
            panic!("expected a typed value");
        };
        assert_eq!(value, ConfigValue::Str("spar".into()));
    }

    #[test]
    fn a_string_value_is_data_and_never_executable_source() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("must-not-exist");
        let mut session = ShellSession::new();
        session
            .submit(&format!(
                "var payload: str = \"touch {}\";",
                target.display()
            ))
            .unwrap();

        assert!(matches!(
            session.submit("payload").unwrap(),
            ShellResult::Value(ConfigValue::Str(_))
        ));
        assert!(!target.exists());
    }

    #[test]
    fn pwd_and_cd_run_in_the_parent_session() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let target = tempfile::tempdir().unwrap();
        let mut session = ShellSession::new();

        assert!(matches!(
            session
                .submit(&format!("cd {}", target.path().display()))
                .unwrap(),
            ShellResult::Builtin(_)
        ));
        let ShellResult::Builtin(output) = session.submit("pwd").unwrap() else {
            panic!("expected pwd builtin output");
        };
        assert_eq!(
            output.stdout,
            Some(format!("{}\n", target.path().display()))
        );
    }

    #[test]
    fn exit_uses_the_previous_status_when_no_status_is_given() {
        let mut session = ShellSession::new();
        let ShellResult::Process(outcome) = session.submit("false").unwrap() else {
            panic!("expected a process outcome");
        };
        assert_eq!(outcome.exit_code, 1);
        assert_eq!(session.last_status(), 1);

        assert!(matches!(
            session.submit("exit").unwrap(),
            ShellResult::Exit(1)
        ));
        assert!(session.should_exit());
    }

    #[test]
    fn pipelines_and_redirects_use_the_shared_plan_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("pipeline.txt");
        let mut session = ShellSession::new();
        let result = session
            .submit(&format!(
                "printf \"alpha\\nbeta\\n\" | grep alpha > {}",
                output.display()
            ))
            .unwrap();

        assert!(matches!(
            result,
            ShellResult::Process(spar::ShellPlanOutcome { success: true, .. })
        ));
        assert_eq!(std::fs::read_to_string(output).unwrap(), "alpha\n");
    }

    #[test]
    fn a_builtin_name_inside_a_pipeline_is_not_run_as_a_parent_builtin() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("pwd.txt");
        let mut session = ShellSession::new();

        let result = session
            .submit(&format!("pwd | cat > {}", output.display()))
            .unwrap();

        assert!(matches!(result, ShellResult::Process(_)));
        assert!(!std::fs::read_to_string(output).unwrap().trim().is_empty());
    }

    #[test]
    fn command_not_found_sets_status_127() {
        let mut session = ShellSession::new();
        let error = session
            .submit("sparsh-command-that-does-not-exist")
            .unwrap_err();

        assert!(matches!(error, ShellError::Process { status: 127, .. }));
        assert_eq!(session.last_status(), 127);
    }
}
