use std::fmt;

use crate::builtin::{BuiltinError, BuiltinOutput, BuiltinRegistry};
use crate::dispatch::{classify, Dispatch};
use crate::execute::execute_plan;
use crate::services::ShellServices;

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
    services: ShellServices,
    should_exit: bool,
    last_status: i32,
}

impl ShellSession {
    pub fn new() -> Self {
        Self::try_new().expect("failed to initialize Sparsh session services")
    }

    pub fn try_new() -> Result<Self, ShellError> {
        Ok(Self {
            spar: spar::Engine::default().session(),
            builtins: BuiltinRegistry::new(),
            services: ShellServices::from_process()
                .map_err(|message| ShellError::Process { message, status: 1 })?,
            should_exit: false,
            last_status: 0,
        })
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
                .and_then(|plan| {
                    execute_plan(&plan, &self.builtins, &mut self.services, self.last_status)
                }),
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
    use std::os::unix::fs::symlink;
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
    fn a_builtin_name_inside_a_pipeline_is_rejected_before_spawning() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("pwd.txt");
        let mut session = ShellSession::new();

        let error = session
            .submit(&format!("pwd | cat > {}", output.display()))
            .unwrap_err();

        assert_eq!(error.status(), 1);
        assert!(error.to_string().contains("builtins in pipelines"));
        assert!(!output.exists());
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

    fn builtin_stdout(result: ShellResult) -> String {
        let ShellResult::Builtin(output) = result else {
            panic!("expected builtin output")
        };
        output.stdout.unwrap_or_default()
    }

    #[test]
    fn service_mutation_affects_later_step_in_same_plan() {
        let tools = tempfile::tempdir().unwrap();
        symlink("/bin/true", tools.path().join("session-tool")).unwrap();
        let mut session = ShellSession::new();

        let result = session
            .submit(&format!(
                "path prepend {}; session-tool",
                tools.path().display()
            ))
            .unwrap();

        assert!(matches!(
            result,
            ShellResult::Process(spar::ShellPlanOutcome { success: true, .. })
        ));
    }

    #[test]
    fn temporary_environment_does_not_mutate_session_environment() {
        let directory = tempfile::tempdir().unwrap();
        let child_environment = directory.path().join("environment");
        let mut session = ShellSession::new();
        session
            .submit(&format!(
                "TEMP_ONLY=yes env > {}",
                child_environment.display()
            ))
            .unwrap();

        let output = builtin_stdout(session.submit("export").unwrap());

        assert!(std::fs::read_to_string(child_environment)
            .unwrap()
            .contains("TEMP_ONLY=yes"));
        assert!(!output.contains("TEMP_ONLY="));
    }

    #[test]
    fn logical_joins_skip_or_run_the_next_step() {
        let mut session = ShellSession::new();

        let first = session.submit("false && sparsh-must-not-run").unwrap();
        assert!(matches!(
            first,
            ShellResult::Process(spar::ShellPlanOutcome { exit_code: 1, .. })
        ));
        let second = session.submit("false || true").unwrap();
        assert!(matches!(
            second,
            ShellResult::Process(spar::ShellPlanOutcome { exit_code: 0, .. })
        ));
    }

    #[test]
    fn aliases_expand_in_pipelines() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output");
        let mut session = ShellSession::new();

        let result = session
            .submit(&format!(
                "alias c = cat; printf x | c > {}",
                output.display()
            ))
            .unwrap();

        assert!(matches!(
            result,
            ShellResult::Process(spar::ShellPlanOutcome { success: true, .. })
        ));
        assert_eq!(std::fs::read(output).unwrap(), b"x");
    }

    #[test]
    fn wrappers_control_alias_and_builtin_resolution() {
        let mut session = ShellSession::new();
        session.submit("alias true = false").unwrap();

        let aliased = session.submit("true").unwrap();
        assert!(matches!(
            aliased,
            ShellResult::Process(spar::ShellPlanOutcome { exit_code: 1, .. })
        ));
        let bypassed = session.submit("command true").unwrap();
        assert!(matches!(
            bypassed,
            ShellResult::Process(spar::ShellPlanOutcome { success: true, .. })
        ));
        let error = session.submit("builtin true").unwrap_err();
        assert_eq!(error.status(), 1);
        assert_eq!(error.to_string(), "true: not a Sparsh builtin");
    }

    #[test]
    fn builtin_redirect_failure_precedes_environment_mutation() {
        let mut session = ShellSession::new();
        session.submit("export KEEP_VALUE=old").unwrap();

        assert!(session
            .submit("export KEEP_VALUE=new > /sparsh-missing-parent/output")
            .is_err());

        let output = builtin_stdout(session.submit("export").unwrap());
        assert!(output.contains("KEEP_VALUE='old'"), "{output}");
    }

    #[test]
    fn input_redirect_reaches_external_command() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input");
        let output = directory.path().join("output");
        std::fs::write(&input, b"payload").unwrap();
        let mut session = ShellSession::new();

        session
            .submit(&format!("cat < {} > {}", input.display(), output.display()))
            .unwrap();

        assert_eq!(std::fs::read(output).unwrap(), b"payload");
    }
}
