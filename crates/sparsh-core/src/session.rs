use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::builtin::{BuiltinError, BuiltinOutput, BuiltinRegistry};
use crate::dispatch::{classify, Dispatch};
use crate::execute::execute_plan;
use crate::resolver::find_external;
use crate::services::ShellServices;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Builtin,
    Alias,
    External,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct ShellUiSnapshot {
    cwd: PathBuf,
    home: Option<PathBuf>,
    path: Vec<PathBuf>,
    builtins: BTreeSet<String>,
    aliases: BTreeSet<String>,
    functions: BTreeSet<String>,
    environment: BTreeMap<String, OsString>,
}

impl ShellUiSnapshot {
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    pub fn has_environment(&self, name: &str) -> bool {
        self.environment.contains_key(name)
    }

    pub fn environment_value(&self, name: &str) -> Option<&OsStr> {
        self.environment.get(name).map(OsString::as_os_str)
    }

    pub fn has_function(&self, name: &str) -> bool {
        self.functions.contains(name)
    }

    pub fn classify_command(&self, program: &str) -> CommandKind {
        if self.aliases.contains(program) {
            CommandKind::Alias
        } else if self.builtins.contains(program) {
            CommandKind::Builtin
        } else if find_external(program, &self.path, &self.cwd).is_some() {
            CommandKind::External
        } else {
            CommandKind::Unknown
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandDiagnostic {
    NotFound {
        program: String,
        suggestions: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    InteractiveTty,
    NonInteractive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorMode {
    Normal,
    Repl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupMode {
    Interactive,
    Command(String),
    Stdin,
    Login,
    RemoteCommand(String),
}

impl StartupMode {
    pub fn session_mode(&self, stdin_is_tty: bool, stderr_is_tty: bool) -> SessionMode {
        match self {
            Self::Interactive | Self::Login if stdin_is_tty && stderr_is_tty => {
                SessionMode::InteractiveTty
            }
            _ => SessionMode::NonInteractive,
        }
    }
}

#[derive(Debug)]
pub enum ShellResult {
    Empty,
    Value(spar::ConfigValue),
    EditorMode(EditorMode),
    ReloadConfig,
    #[doc(hidden)]
    ExecRequest {
        words: Vec<String>,
        template: Box<spar_command::CommandPlan>,
    },
    #[doc(hidden)]
    SourceRequest(crate::builtin::SourceRequest),
    Builtin(BuiltinOutput),
    Process(spar::ShellPlanOutcome),
    BackgroundJob {
        id: u64,
        pgid: i32,
    },
    CommandStatus {
        status: i32,
        diagnostic: Option<CommandDiagnostic>,
    },
    Exit(i32),
}

#[derive(Debug)]
pub enum ShellError {
    Spar(Vec<spar::SparError>),
    SparSource {
        errors: Vec<spar::SparError>,
        source: String,
        filename: String,
    },
    Config(crate::ConfigLoadError),
    Builtin(BuiltinError),
    CommandNotFound {
        program: String,
        suggestions: Vec<String>,
    },
    Process {
        message: String,
        status: i32,
    },
}

impl ShellError {
    pub fn status(&self) -> i32 {
        match self {
            Self::Spar(_) | Self::SparSource { .. } | Self::Config(_) => 1,
            Self::Builtin(error) => error.status,
            Self::CommandNotFound { .. } => 127,
            Self::Process { status, .. } => *status,
        }
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spar(errors) | Self::SparSource { errors, .. } => {
                for (index, error) in errors.iter().enumerate() {
                    if index > 0 {
                        writeln!(formatter)?;
                    }
                    write!(formatter, "{error}")?;
                }
                Ok(())
            }
            Self::Config(error) => write!(formatter, "{error}"),
            Self::Builtin(error) => formatter.write_str(&error.message),
            Self::CommandNotFound {
                program,
                suggestions,
            } => {
                write!(formatter, "command not found: `{program}`")?;
                if !suggestions.is_empty() {
                    write!(formatter, "\ndid you mean: {}", suggestions.join(", "))?;
                }
                Ok(())
            }
            Self::Process { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ShellError {}

pub struct ShellSession {
    spar: spar::Session,
    builtins: BuiltinRegistry,
    services: ShellServices,
    mode: SessionMode,
    should_exit: bool,
    last_status: i32,
    config: crate::SparshConfig,
    config_generation: u64,
    interactive_source: String,
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
            mode: SessionMode::NonInteractive,
            should_exit: false,
            last_status: 0,
            config: crate::SparshConfig::default(),
            config_generation: 0,
            interactive_source: String::new(),
        })
    }

    pub fn try_new_interactive() -> Result<Self, ShellError> {
        let mut session = Self::try_new()?;
        session.mode = SessionMode::InteractiveTty;
        let cwd = session.services.directories.current().to_path_buf();
        let executable = session
            .services
            .resolver
            .external("ls", &session.services.path, &cwd)
            .ok();
        if let Some(executable) = executable {
            let environment = session.services.environment.snapshot();
            install_color_ls_alias(
                &mut session.services.aliases,
                &executable,
                &cwd,
                &environment,
            );
        }
        Ok(session)
    }

    pub fn try_new_for(
        startup: &StartupMode,
        stdin_is_tty: bool,
        stderr_is_tty: bool,
    ) -> Result<Self, ShellError> {
        let mut session = match startup.session_mode(stdin_is_tty, stderr_is_tty) {
            SessionMode::InteractiveTty => Self::try_new_interactive()?,
            SessionMode::NonInteractive => Self::try_new()?,
        };
        session.services.login_shell = matches!(startup, StartupMode::Login);
        Ok(session)
    }

    pub fn submit_spar(&mut self, source: &str) -> Result<ShellResult, ShellError> {
        let before = self.spar.committed_source().to_string();
        let cwd = self.services.directories.current().to_path_buf();
        let environment = self.services.environment.snapshot();
        let result = self
            .spar
            .eval_interactive_with_context(source, &cwd, &environment)
            .map_err(ShellError::Spar)?;
        let committed = self.spar.committed_source();
        let appended = if before.is_empty() {
            committed.to_string()
        } else {
            committed
                .strip_prefix(&before)
                .and_then(|suffix| suffix.strip_prefix('\n'))
                .unwrap_or(source)
                .to_string()
        };
        append_fragment(&mut self.interactive_source, &appended);
        match result {
            spar::InteractiveEvalResult::Empty => Ok(ShellResult::Empty),
            spar::InteractiveEvalResult::Value(value) => self.handle_interactive_value(value),
        }
    }

    pub fn submit(&mut self, input: &str) -> Result<ShellResult, ShellError> {
        self.poll_jobs()?;
        let cwd = self.services.directories.current().to_path_buf();
        let environment = self.services.environment.snapshot();
        if let Some(plan) = crate::function_pipeline::compose_function_pipeline(
            input,
            &self.spar,
            &cwd,
            &environment,
        )? {
            let result = execute_plan(&plan, &self.builtins, &mut self.services, self.last_status, self.mode);
            return self.finish_submission(result);
        }

        let dispatch = classify(input, &self.spar);
        if matches!(&dispatch, Dispatch::Empty) {
            return Ok(ShellResult::Empty);
        }

        let result = match dispatch {
            Dispatch::Empty => unreachable!("empty input returned above"),
            Dispatch::SparFragment(fragment) => self.submit_spar(fragment),
            Dispatch::SparValue(name) => Ok(ShellResult::Value(
                self.spar
                    .value(name)
                    .expect("dispatch verified the variable exists")
                    .clone(),
            )),
            Dispatch::Command(command) => {
                self.spar
                    .eval_shell_plan_with_context(command, &cwd, &environment)
                    .map_err(ShellError::Spar)
                    .and_then(|plan| {
                        execute_plan(
                            &plan,
                            &self.builtins,
                            &mut self.services,
                            self.last_status,
                            self.mode,
                        )
                    })
            }
        };

        self.finish_submission(result)
    }

    fn finish_submission(&mut self, result: Result<ShellResult, ShellError>) -> Result<ShellResult, ShellError> {
        match result {
            Ok(ShellResult::ExecRequest { words, template }) => {
                self.replace_with_command(words, *template)
            }
            Ok(ShellResult::SourceRequest(request)) => self.source_request(request),
            Ok(ShellResult::ReloadConfig) => {
                self.reload_config()?;
                self.last_status = 0;
                Ok(ShellResult::ReloadConfig)
            }
            Ok(result) => {
                self.last_status = match &result {
                    ShellResult::Empty
                    | ShellResult::Value(_)
                    | ShellResult::EditorMode(_)
                    | ShellResult::ReloadConfig
                    | ShellResult::ExecRequest { .. }
                    | ShellResult::SourceRequest(_) => 0,
                    ShellResult::Builtin(output) => output.status,
                    ShellResult::Process(outcome) => outcome.exit_code,
                    ShellResult::BackgroundJob { .. } => 0,
                    ShellResult::CommandStatus { status, .. } => *status,
                    ShellResult::Exit(status) => *status,
                };
                if matches!(&result, ShellResult::Exit(_)) {
                    self.should_exit = true;
                }
                self.poll_jobs()?;
                Ok(result)
            }
            Err(error) => {
                self.last_status = error.status();
                Err(error)
            }
        }
    }

    pub fn set_history_access(&mut self, history: std::sync::Arc<dyn crate::history::HistoryAccess>) {
        self.services.set_history_access(history);
    }

    fn replace_with_command(
        &mut self,
        words: Vec<String>,
        template: spar_command::CommandPlan,
    ) -> Result<ShellResult, ShellError> {
        crate::execute::replace_with_external_command(&words, template, &mut self.services)?;
        unreachable!("successful exec replaces the Sparsh process")
    }

    fn source_request(&mut self, request: crate::builtin::SourceRequest) -> Result<ShellResult, ShellError> {
        self.source_path(&request.path, request.shell.as_deref())?;
        Ok(ShellResult::Empty)
    }

    fn source_path(&mut self, raw_path: &str, foreign_shell: Option<&str>) -> Result<(), ShellError> {
        let path = self.resolve_session_path(raw_path)?;
        if foreign_shell.is_none() && path.extension().and_then(|ext| ext.to_str()) == Some("spar") {
            return self.source_spar_file(&path);
        }
        self.source_foreign_file(&path, foreign_shell)
    }

    fn resolve_session_path(&self, raw: &str) -> Result<PathBuf, ShellError> {
        let expanded = if raw == "~" {
            PathBuf::from(self.services.environment.get("HOME").ok_or_else(|| ShellError::Process {
                message: "HOME is not set; cannot expand '~'".into(), status: 1,
            })?)
        } else if let Some(rest) = raw.strip_prefix("~/") {
            PathBuf::from(self.services.environment.get("HOME").ok_or_else(|| ShellError::Process {
                message: "HOME is not set; cannot expand '~'".into(), status: 1,
            })?).join(rest)
        } else {
            let path = Path::new(raw);
            if path.is_absolute() { path.to_path_buf() } else { self.services.directories.current().join(path) }
        };
        Ok(expanded)
    }

    fn source_spar_file(&mut self, path: &Path) -> Result<(), ShellError> {
        let source = std::fs::read_to_string(path).map_err(|error| ShellError::Process {
            message: format!("source: {}: {error}", path.display()), status: 1,
        })?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let source = rebase_relative_imports(&source, base);
        let cwd = self.services.directories.current().to_path_buf();
        let environment = self.services.environment.snapshot();
        let before = self.spar.committed_source().to_string();
        self.spar
            .eval_interactive_with_context(&source, &cwd, &environment)
            .map_err(ShellError::Spar)?;
        let committed = self.spar.committed_source();
        let appended = if before.is_empty() {
            committed.to_string()
        } else {
            committed
                .strip_prefix(&before)
                .and_then(|suffix| suffix.strip_prefix('\n'))
                .unwrap_or(&source)
                .to_string()
        };
        append_fragment(&mut self.interactive_source, &appended);
        Ok(())
    }

    fn source_foreign_file(&mut self, path: &Path, requested_shell: Option<&str>) -> Result<(), ShellError> {
        let previous_environment = self.services.environment.snapshot();
        let inferred_shell = self
            .services
            .environment
            .get("SHELL")
            .and_then(|value| Path::new(value).file_name())
            .and_then(|value| value.to_str())
            .filter(|value| matches!(*value, "sh" | "bash" | "zsh"));
        let shell = requested_shell.or(inferred_shell).unwrap_or("sh");
        if !matches!(shell, "sh" | "bash" | "zsh") {
            return Err(ShellError::Builtin(BuiltinError {
                message: format!("source: unsupported foreign shell '{shell}'; expected sh, bash, or zsh"),
                status: 2,
            }));
        }
        let script = r#". "$1" 1>&2 || exit $?; printf '%s\0' "$PWD"; env -0"#;
        let output = Command::new(shell)
            .arg("-c")
            .arg(script)
            .arg("sparsh-source")
            .arg(path)
            .current_dir(self.services.directories.current())
            .env_clear()
            .envs(self.services.environment.snapshot())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| ShellError::Process {
                message: format!("source: failed to start {shell}: {error}"), status: 1,
            })?;
        if !output.status.success() {
            let status = output.status.code().unwrap_or(1);
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ShellError::Process {
                message: if stderr.is_empty() {
                    format!("source: {} exited with status {status}", path.display())
                } else { stderr },
                status,
            });
        }
        let mut fields = output.stdout.split(|byte| *byte == 0);
        let cwd = fields
            .next()
            .filter(|field| !field.is_empty())
            .ok_or_else(|| ShellError::Process {
                message: "source: foreign shell returned no working directory".into(), status: 1,
            })?;
        #[cfg(unix)]
        let cwd = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(OsString::from_vec(cwd.to_vec()))
        };
        #[cfg(not(unix))]
        let cwd = PathBuf::from(String::from_utf8_lossy(cwd).into_owned());

        let mut environment = Vec::new();
        for field in fields.filter(|field| !field.is_empty()) {
            let Some(index) = field.iter().position(|byte| *byte == b'=') else { continue; };
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                environment.push((
                    OsString::from_vec(field[..index].to_vec()),
                    OsString::from_vec(field[index + 1..].to_vec()),
                ));
            }
            #[cfg(not(unix))]
            environment.push((
                OsString::from(String::from_utf8_lossy(&field[..index]).into_owned()),
                OsString::from(String::from_utf8_lossy(&field[index + 1..]).into_owned()),
            ));
        }
        if !cwd.is_dir() {
            return Err(ShellError::Process {
                message: format!("source: resulting cwd {} is not a directory", cwd.display()), status: 1,
            });
        }
        self.services
            .record_python_activation(&previous_environment, &environment);
        self.services.environment.replace_snapshot(environment);
        self.services
            .directories
            .set_current_path(&cwd, &mut self.services.environment)
            .map_err(|message| ShellError::Process { message, status: 1 })?;
        self.services.reload_path_from_environment();
        self.services.path.invalidate_executable_cache();
        Ok(())
    }

    fn handle_interactive_value(
        &mut self,
        value: spar::ConfigValue,
    ) -> Result<ShellResult, ShellError> {
        match value {
            spar::ConfigValue::Shell(plan) => {
                execute_plan(
                        &plan,
                        &self.builtins,
                        &mut self.services,
                        self.last_status,
                        self.mode,
                    )
            }
            other => Ok(ShellResult::Value(other)),
        }
    }

    pub fn reload_config(&mut self) -> Result<(), ShellError> {
        let (source, base_dir) = match crate::config::config_path(&self.services.environment) {
            Some(path) => {
                let source = crate::config::read_source(&path).map_err(ShellError::Config)?;
                let base = path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf();
                (source, base)
            }
            None => (String::new(), self.services.directories.current().to_path_buf()),
        };
        let mut candidate = spar::Engine::default()
            .with_base_dir(base_dir)
            .session();
        let config = crate::config::evaluate_source_in_session(&mut candidate, &source)
            .map_err(ShellError::Config)?;
        if !self.interactive_source.trim().is_empty() {
            candidate
                .eval(&self.interactive_source)
                .map_err(ShellError::Spar)?;
        }
        self.services
            .apply_config(&config)
            .map_err(|message| ShellError::Config(crate::ConfigLoadError::Invalid(message)))?;
        self.spar = candidate;
        self.config = config;
        self.config_generation = self.config_generation.wrapping_add(1);
        Ok(())
    }

    pub fn run_startup_hook(&mut self) -> Result<ShellResult, ShellError> {
        if !self.spar.has_function("startup") {
            return Ok(ShellResult::Empty);
        }
        let cwd = self.services.directories.current().to_path_buf();
        let environment = self.services.environment.snapshot();
        match self
            .spar
            .eval_transient_with_context("startup()", &cwd, &environment)
            .map_err(ShellError::Spar)?
        {
            spar::InteractiveEvalResult::Empty => Ok(ShellResult::Empty),
            spar::InteractiveEvalResult::Value(value) => self.handle_interactive_value(value),
        }
    }

    pub fn history_settings(&self) -> crate::HistorySettings {
        crate::HistorySettings::from_config(&self.services.environment, &self.config.history)
    }

    pub fn prompt_config(&self) -> &crate::PromptConfig {
        &self.config.prompt
    }

    /// Problems found in the `prompt` section of the loaded config (empty when
    /// it is fine). They never stop the shell; the UI reports them.
    pub fn prompt_issues(&self) -> &[crate::PromptIssue] {
        &self.config.prompt_issues
    }

    pub fn config_generation(&self) -> u64 {
        self.config_generation
    }

    pub fn completion_snapshot(&self) -> crate::CompletionSnapshot {
        if !self.config.completion.enabled {
            return crate::CompletionSnapshot::default();
        }
        let cwd = self.services.directories.current().to_path_buf();
        let builtins = self
            .builtins
            .metadata()
            .map(|metadata| (metadata.name.to_string(), metadata.description.to_string()))
            .collect::<BTreeMap<_, _>>();
        let aliases = self
            .services
            .aliases
            .names()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let executables = self.services.path.executable_names(&cwd);
        let spar_identifiers = self
            .spar
            .identifiers()
            .filter(|name| !name.starts_with("Sparsh"))
            .map(str::to_string)
            .collect();
        let spar_functions = self
            .spar
            .function_names()
            .filter(|name| !name.starts_with("Sparsh"))
            .map(|name| {
                (
                    name.to_string(),
                    self.spar
                        .function_parameters(name)
                        .unwrap_or_default()
                        .to_vec(),
                )
            })
            .collect();
        crate::CompletionSnapshot {
            cwd,
            home: self.services.environment.get("HOME").map(PathBuf::from),
            builtins,
            aliases,
            executables,
            spar_identifiers,
            spar_functions,
        }
    }

    pub fn last_status(&self) -> i32 {
        self.last_status
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }

    pub fn jobs_snapshot(&self) -> Vec<crate::ShellJob> {
        self.services.jobs.snapshots()
    }

    pub fn take_job_notifications(&mut self) -> Vec<String> {
        self.services.jobs.take_notifications()
    }

    pub fn refresh_jobs(&mut self) -> Result<(), ShellError> {
        self.services.jobs.poll().map_err(process_io_error)
    }

    fn poll_jobs(&mut self) -> Result<(), ShellError> {
        self.refresh_jobs()
    }

    pub fn ui_snapshot(&self) -> ShellUiSnapshot {
        ShellUiSnapshot {
            cwd: self.services.directories.current().to_path_buf(),
            home: self.services.environment.get("HOME").map(PathBuf::from),
            path: self.services.path.directories().to_vec(),
            builtins: self.builtins.names().into_iter().collect(),
            aliases: self.services.aliases.names().map(str::to_string).collect(),
            functions: self.spar.function_names().map(str::to_string).collect(),
            environment: self
                .services
                .environment
                .snapshot()
                .into_iter()
                .map(|(name, value)| (name.to_string_lossy().into_owned(), value))
                .collect(),
        }
    }
}

fn rebase_relative_imports(source: &str, base: &Path) -> String {
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0usize;
    while cursor < source.len() {
        let Some(relative) = source[cursor..].find("import") else {
            output.push_str(&source[cursor..]);
            break;
        };
        let start = cursor + relative;
        output.push_str(&source[cursor..start]);
        let statement_end = source[start..]
            .find(';')
            .map(|offset| start + offset + 1)
            .unwrap_or(source.len());
        let statement = &source[start..statement_end];
        output.push_str(&rebase_import_statement(statement, base));
        cursor = statement_end;
    }
    output
}

fn rebase_import_statement(statement: &str, base: &Path) -> String {
    if statement.starts_with("import pkg ") {
        return statement.to_string();
    }
    let Some(end_quote) = statement.rfind('"') else {
        return statement.to_string();
    };
    let Some(start_quote) = statement[..end_quote].rfind('"') else {
        return statement.to_string();
    };
    let raw = &statement[start_quote + 1..end_quote];
    if !(raw.starts_with("./") || raw.starts_with("../")) {
        return statement.to_string();
    }
    let absolute = base.join(raw);
    format!(
        "{}{}{}",
        &statement[..start_quote + 1],
        absolute.to_string_lossy(),
        &statement[end_quote..]
    )
}

fn append_fragment(target: &mut String, fragment: &str) {
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(fragment);
}

fn process_io_error(error: std::io::Error) -> ShellError {
    ShellError::Process {
        message: error.to_string(),
        status: 1,
    }
}

impl Drop for ShellSession {
    fn drop(&mut self) {
        if self.mode != SessionMode::NonInteractive {
            return;
        }
        for id in self.services.jobs.ids() {
            loop {
                let Some(job) = self.services.jobs.get(id) else {
                    break;
                };
                match job.state {
                    crate::job::JobState::Done(_) => break,
                    crate::job::JobState::Stopped => {
                        if self.services.jobs.resume(id).is_err() {
                            break;
                        }
                    }
                    crate::job::JobState::Running => {}
                }
                match self.services.jobs.wait_until_stable(id) {
                    Ok(Some(crate::job::JobState::Done(_))) | Ok(None) => break,
                    Ok(Some(crate::job::JobState::Stopped)) => continue,
                    Ok(Some(crate::job::JobState::Running)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
}

impl Default for ShellSession {
    fn default() -> Self {
        Self::new()
    }
}

fn install_color_ls_alias(
    aliases: &mut crate::alias::AliasService,
    executable: &Path,
    cwd: &Path,
    environment: &[(OsString, OsString)],
) {
    let supported = Command::new(executable)
        .args(["--color=auto", "-d", "."])
        .current_dir(cwd)
        .env_clear()
        .envs(environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if supported {
        aliases
            .define("ls", vec!["ls".into(), "--color=auto".into()])
            .expect("the built-in ls alias is valid");
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use spar::ConfigValue;

    use super::{
        install_color_ls_alias, CommandDiagnostic, CommandKind, ShellResult, ShellSession,
    };
    use crate::alias::AliasService;
    use crate::PROCESS_STATE;

    #[derive(Default)]
    struct MemoryHistory {
        entries: Mutex<Vec<String>>,
    }

    impl crate::HistoryAccess for MemoryHistory {
        fn list(&self, limit: Option<usize>) -> Result<Vec<String>, String> {
            let entries = self.entries.lock().map_err(|_| "history lock poisoned".to_string())?;
            let start = limit
                .map(|limit| entries.len().saturating_sub(limit))
                .unwrap_or(0);
            Ok(entries[start..].to_vec())
        }

        fn clear(&self) -> Result<(), String> {
            self.entries
                .lock()
                .map_err(|_| "history lock poisoned".to_string())?
                .clear();
            Ok(())
        }
    }

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
    fn startup_function_is_the_single_startup_hook_and_runs_in_the_live_session() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = ShellSession::new();
        let source = format!(
            "function startup() -> shell {{ return shell {{ cd {}; }}; }};",
            directory.path().display()
        );

        session.submit_spar(&source).unwrap();
        session.run_startup_hook().unwrap();

        assert_eq!(session.ui_snapshot().cwd(), directory.path());
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
            format!("{}\n", target.path().display()).into_bytes()
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
    fn builtin_stage_can_feed_an_external_pipeline_stage() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("pwd.txt");
        let mut session = ShellSession::new();

        session
            .submit(&format!("pwd | cat > {}", output.display()))
            .unwrap();

        let expected = format!("{}\n", session.ui_snapshot().cwd().display());
        assert_eq!(std::fs::read_to_string(output).unwrap(), expected);
    }

    #[test]
    fn configured_alias_to_missing_executable_is_valid_until_execution() {
        let mut session = ShellSession::new();
        let config = crate::SparshConfig {
            aliases: vec![(
                "configured-missing".into(),
                vec!["sparsh-command-that-does-not-exist".into()],
            )],
            ..crate::SparshConfig::default()
        };
        session.services.apply_config(&config).unwrap();

        let result = session.submit("configured-missing").unwrap();

        assert!(matches!(
            result,
            ShellResult::CommandStatus { status: 127, .. }
        ));
    }

    #[test]
    fn reload_with_a_broken_prompt_slot_succeeds_and_reports_issues() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".sparsh")).unwrap();
        std::fs::write(
            home.path().join(".sparsh/sparsh.spar"),
            format!(
                "{}\nstruct Config: SparshConfig {{\n    aliases = [{{ name: \"gs\"; command: [\"git\", \"status\"]; }}];\n    prompt = {{ right: {{ slot1: {{ text: \"{{cpu}}\"; }}; slot2: {{ text: \"{{cpuu}}\"; }}; }}; }};\n}};\n",
                include_str!("../../../examples/sparsh-types.spar")
            ),
        )
        .unwrap();
        let mut session = ShellSession::new();
        session
            .submit(&format!("export HOME={}", home.path().display()))
            .unwrap();
        let before = session.config_generation();

        session.reload_config().expect("prompt problems must not fail the reload");

        assert_ne!(session.config_generation(), before, "the config was applied");
        assert_eq!(session.prompt_issues().len(), 1);
        assert_eq!(session.prompt_issues()[0].slot, Some(2));
        assert!(matches!(
            session.prompt_config().right.slots[1],
            Some(crate::SlotConfig::Broken { .. })
        ));
        assert_eq!(
            session.ui_snapshot().classify_command("gs"),
            CommandKind::Alias,
            "the alias from the same file still applies"
        );
    }

    #[test]
    fn reload_without_home_uses_defaults_instead_of_current_directory_rc() {
        let mut session = ShellSession::new();
        session.submit("unset HOME").unwrap();

        session.reload_config().unwrap();

        assert_eq!(session.prompt_config(), &crate::PromptConfig::default());
    }

    #[test]
    fn command_not_found_sets_status_127() {
        let mut session = ShellSession::new();
        let result = session
            .submit("sparsh-command-that-does-not-exist")
            .unwrap();

        assert!(matches!(
            result,
            ShellResult::CommandStatus { status: 127, .. }
        ));
        assert_eq!(session.last_status(), 127);
    }

    #[test]
    fn command_not_found_preserves_program_and_suggestions() {
        let mut session = ShellSession::new();

        let result = session.submit("pwdd").unwrap();

        let ShellResult::CommandStatus {
            status: 127,
            diagnostic:
                Some(CommandDiagnostic::NotFound {
                    program,
                    suggestions,
                }),
        } = result
        else {
            panic!("expected structured command-not-found result");
        };
        assert_eq!(program, "pwdd");
        assert!(suggestions.iter().any(|name| name == "pwd"));
    }

    fn builtin_stdout(result: ShellResult) -> String {
        let ShellResult::Builtin(output) = result else {
            panic!("expected builtin output")
        };
        String::from_utf8(output.stdout).unwrap()
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

    #[test]
    fn ui_snapshot_reports_cwd_home_builtins_aliases_and_external_commands() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::env::set_current_dir(root.path()).unwrap();
        let tool = bin.join("snapshot-tool");
        std::fs::write(&tool, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut session = ShellSession::new();
        session
            .submit(&format!("export HOME={}", root.path().display()))
            .unwrap();
        session
            .submit(&format!("path prepend {}", bin.display()))
            .unwrap();
        session
            .submit("export VIRTUAL_ENV=/tmp/demo/.venv")
            .unwrap();
        session.submit("alias snap = snapshot-tool").unwrap();
        let snapshot = session.ui_snapshot();

        assert_eq!(snapshot.cwd(), root.path());
        assert_eq!(snapshot.home(), Some(root.path()));
        assert_eq!(
            snapshot.environment_value("VIRTUAL_ENV"),
            Some(std::ffi::OsStr::new("/tmp/demo/.venv"))
        );
        assert_eq!(snapshot.classify_command("cd"), CommandKind::Builtin);
        assert_eq!(snapshot.classify_command("snap"), CommandKind::Alias);
        assert_eq!(
            snapshot.classify_command("snapshot-tool"),
            CommandKind::External
        );
        assert_eq!(
            snapshot.classify_command("missing-snapshot-tool"),
            CommandKind::Unknown
        );
    }

    fn color_ls_fixture(exit_status: i32) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("ls");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\ncase \"$1\" in --color=auto) exit {exit_status};; *) exit 9;; esac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        (directory, executable)
    }

    #[test]
    fn supported_color_ls_is_seeded_as_an_ordinary_alias() {
        let (_directory, executable) = color_ls_fixture(0);
        let mut aliases = AliasService::new();

        install_color_ls_alias(&mut aliases, &executable, Path::new("/"), &[]);

        assert_eq!(aliases.get("ls").unwrap(), ["ls", "--color=auto"]);
        aliases.define("ls", vec!["custom-ls".into()]).unwrap();
        assert_eq!(aliases.get("ls").unwrap(), ["custom-ls"]);
        aliases.remove_many(&["ls".into()]).unwrap();
        assert!(aliases.get("ls").is_none());
    }

    #[test]
    fn missing_command_can_be_recovered_by_or_join() {
        let mut session = ShellSession::new();
        let result = session
            .submit("definitely-not-a-command || printf recovered")
            .unwrap();
        let ShellResult::Process(outcome) = result else {
            panic!("expected process outcome")
        };
        assert!(outcome.success);
        assert_eq!(outcome.exit_code, 0);
    }

    #[test]
    fn missing_command_prevents_and_join_rhs() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("must-not-exist");
        let mut session = ShellSession::new();
        let result = session
            .submit(&format!(
                "definitely-not-a-command && touch {}",
                marker.display()
            ))
            .unwrap();
        let ShellResult::CommandStatus { status, .. } = result else {
            panic!("expected command status")
        };
        assert_eq!(status, 127);
        assert!(!marker.exists());
    }

    #[test]
    fn pwd_stdout_can_be_redirected() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("pwd.txt");
        let mut session = ShellSession::new();
        session
            .submit(&format!("pwd > {}", output.display()))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            format!("{}\n", std::env::current_dir().unwrap().display())
        );
    }

    #[test]
    fn builtin_redirection_failure_happens_before_cd_mutates_state() {
        let original = std::env::current_dir().unwrap();
        let mut session = ShellSession::new();
        let error = session
            .submit("cd /tmp > /definitely/missing/dir/out")
            .unwrap_err();
        assert_eq!(error.status(), 1);
        assert_eq!(session.ui_snapshot().cwd(), original.as_path());
    }

    #[test]
    fn pwd_builtin_can_feed_an_external_pipeline_stage() {
        let mut session = ShellSession::new();
        let result = session.submit("pwd | cat").unwrap();
        assert!(matches!(result, ShellResult::Process(_)));
    }

    #[test]
    fn export_in_pipeline_does_not_mutate_parent_environment() {
        let mut session = ShellSession::new();
        session
            .submit("export SPARSH_PIPELINE_TEST=child | cat")
            .unwrap();
        assert!(!session
            .ui_snapshot()
            .has_environment("SPARSH_PIPELINE_TEST"));
    }

    #[test]
    fn cd_in_pipeline_does_not_mutate_parent_cwd() {
        let mut session = ShellSession::new();
        let before = session.ui_snapshot().cwd().to_path_buf();
        session.submit("cd /tmp | cat").unwrap();
        assert_eq!(session.ui_snapshot().cwd(), before.as_path());
    }

    #[test]
    fn direct_shell_returning_function_call_auto_executes() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("built.txt");
        let mut session = ShellSession::new();
        session
            .submit(&format!(
                "function build() -> shell {{ return shell {{ touch {}; }}; }};",
                marker.display()
            ))
            .unwrap();
        session.submit("build()").unwrap();
        assert!(marker.exists());
    }

    #[test]
    fn mixed_shell_returning_function_executes_locals_and_command_substitution() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("mixed-shell.txt");
        let mut session = ShellSession::new();
        session
            .submit(&format!(
                r#"function writeMarker(output: str) -> shell {{
    return shell {{
        var value: str = $(printf ready);
        printf "%s" "${{value}}" > "${{output}}";
    }};
}};"#
            ))
            .unwrap();
        session
            .submit(&format!(
                r#"writeMarker(output: "{}")"#,
                output.display()
            ))
            .unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "ready");
    }

    #[test]
    fn mixed_shell_returning_function_executes_for_loop_commands() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("loop-marker");
        let one = directory.path().join("loop-marker-one");
        let two = directory.path().join("loop-marker-two");
        let mut session = ShellSession::new();
        session
            .submit(&format!(
                r#"function writeMarkers(output: str) -> shell {{
    return shell {{
        var values: List<str> = ["one", "two"];
        for value in values {{
            touch "${{output}}-${{value}}";
        }}
    }};
}};"#
            ))
            .unwrap();
        session
            .submit(&format!(
                r#"writeMarkers(output: "{}")"#,
                output.display()
            ))
            .unwrap();
        assert!(one.exists());
        assert!(two.exists());
    }

    #[test]
    fn assigning_shell_value_does_not_execute_it() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("must-not-exist");
        let mut session = ShellSession::new();
        session
            .submit(&format!(
                "function build() -> shell {{ return shell {{ touch {}; }}; }};",
                marker.display()
            ))
            .unwrap();
        session.submit("var plan = build();").unwrap();
        assert!(!marker.exists());
    }

    #[test]
    fn background_command_creates_a_persistent_job() {
        let mut session = ShellSession::new();
        let result = session.submit("sleep 1 &").unwrap();
        assert!(matches!(
            result,
            ShellResult::BackgroundJob { id: 1, pgid } if pgid > 0
        ));
        let jobs = session.jobs_snapshot();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id.0, 1);
        assert_eq!(jobs[0].state, crate::JobState::Running);
    }

    #[test]
    fn background_pipeline_uses_one_persistent_job() {
        let mut session = ShellSession::new();
        let result = session.submit("sleep 1 | cat &").unwrap();
        assert!(matches!(result, ShellResult::BackgroundJob { id: 1, .. }));
        assert_eq!(session.jobs_snapshot().len(), 1);
    }

    #[test]
    fn completed_background_job_is_reaped_without_blocking_submit() {
        let mut session = ShellSession::new();
        session.submit("sh -c 'exit 7' &").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        session.submit("true").unwrap();
        let jobs = session.jobs_snapshot();
        assert!(matches!(jobs[0].state, crate::JobState::Done(7)));
    }

    #[test]
    fn jobs_builtin_lists_background_job() {
        let mut session = ShellSession::new();
        session.submit("sleep 1 &").unwrap();
        let output = builtin_stdout(session.submit("jobs").unwrap());
        assert!(output.contains("[1] Running"), "{output}");
        assert!(output.contains("sleep 1"), "{output}");
    }

    #[test]
    fn disown_removes_job_without_waiting_for_it() {
        let mut session = ShellSession::new();
        session.submit("sleep 1 &").unwrap();
        session.submit("disown %1").unwrap();
        assert!(session.jobs_snapshot().is_empty());
    }

    #[test]
    fn wait_builtin_returns_the_background_exit_status() {
        let mut session = ShellSession::new();
        session.submit("sh -c 'exit 7' &").unwrap();
        let ShellResult::Builtin(output) = session.submit("wait %1").unwrap() else {
            panic!("expected wait builtin output");
        };
        assert_eq!(output.status, 7);
        assert!(session.jobs_snapshot().is_empty());
    }

    #[test]
    fn bg_resumes_a_stopped_job() {
        let mut session = ShellSession::new();
        session.submit("sh -c 'kill -STOP $$; sleep 1' &").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        session.submit("jobs").unwrap();
        assert!(matches!(
            session.jobs_snapshot()[0].state,
            crate::JobState::Stopped
        ));
        session.submit("bg %1").unwrap();
        assert!(matches!(
            session.jobs_snapshot()[0].state,
            crate::JobState::Running
        ));
    }

    #[test]
    fn unsupported_color_ls_does_not_create_an_alias() {
        let (_directory, executable) = color_ls_fixture(2);
        let mut aliases = AliasService::new();

        install_color_ls_alias(&mut aliases, &executable, Path::new("/"), &[]);

        assert!(aliases.get("ls").is_none());
    }
    #[test]
    fn shell_returning_function_uses_named_arguments_and_session_environment() {
        let _guard = PROCESS_STATE.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("greeting.txt");
        let mut session = ShellSession::new();

        session.submit("export SPARSH_GREETING=Hello").unwrap();
        session
            .submit_spar(
                "function greet(name: str, output: str) -> shell { return shell { printf \"%s %s\" $SPARSH_GREETING ${name} > ${output}; }; }",
            )
            .unwrap();
        session
            .submit_spar(&format!(
                "greet(name: \"OCC\", output: \"{}\")",
                output.to_string_lossy()
            ))
            .unwrap();

        assert_eq!(std::fs::read_to_string(output).unwrap(), "Hello OCC");
    }

    #[test]
    fn external_command_arguments_expand_tilde_against_session_home() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("needle-file"), b"x").unwrap();
        let output = home.path().join("result.txt");
        let mut session = ShellSession::new();
        session
            .submit(&format!("export HOME={}", home.path().display()))
            .unwrap();

        session
            .submit(&format!("ls ~ | grep needle > {}", output.display()))
            .unwrap();

        assert_eq!(std::fs::read_to_string(output).unwrap(), "needle-file\n");
    }

    #[test]
    fn plain_external_command_does_not_enter_spar_module_parser() {
        let mut session = ShellSession::new();

        let result = session.submit("true").unwrap();

        let ShellResult::Process(outcome) = result else {
            panic!("expected process outcome")
        };
        assert!(outcome.success);
        assert_eq!(outcome.exit_code, 0);
    }

    #[test]
    fn interactive_session_can_run_default_ls_alias_without_cycle() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("visible-file"), b"x").unwrap();
        let output = directory.path().join("listing.txt");
        let mut session = ShellSession::try_new_interactive().unwrap();

        session
            .submit(&format!("ls {} > {}", directory.path().display(), output.display()))
            .unwrap();

        assert!(std::fs::read_to_string(output).unwrap().contains("visible-file"));
    }


    #[test]
    fn practical_io_and_help_builtins_are_native_and_pipeline_capable() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("printf.txt");
        let input = directory.path().join("read.txt");
        std::fs::write(&input, "line-value\n").unwrap();
        let mut session = ShellSession::new();

        assert_eq!(builtin_stdout(session.submit("echo hello Sparsh").unwrap()), "hello Sparsh\n");
        session
            .submit(&format!("printf '%s\\n' alpha | grep alpha > {}", output.display()))
            .unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "alpha\n");

        session
            .submit(&format!("read SPARSH_READ_VALUE < {}", input.display()))
            .unwrap();
        let exported = builtin_stdout(session.submit("export").unwrap());
        assert!(exported.contains("SPARSH_READ_VALUE='line-value'"), "{exported}");

        let help = builtin_stdout(session.submit("help source").unwrap());
        assert!(help.contains("source"), "{help}");
        assert!(help.contains("usage:"), "{help}");
    }

    #[test]
    fn history_builtin_uses_the_editor_history_bridge() {
        let history = Arc::new(MemoryHistory::default());
        history.entries.lock().unwrap().extend([
            "echo one".to_string(),
            "echo two".to_string(),
            "echo three".to_string(),
        ]);
        let mut session = ShellSession::new();
        session.set_history_access(history.clone());

        let output = builtin_stdout(session.submit("history 2").unwrap());
        assert!(output.contains("echo two"), "{output}");
        assert!(output.contains("echo three"), "{output}");
        assert!(!output.contains("echo one"), "{output}");

        session.submit("history -c").unwrap();
        assert!(history.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn logout_requires_login_mode() {
        let mut session = ShellSession::new();
        let error = session.submit("logout").unwrap_err();
        assert_eq!(error.status(), 1);
        assert!(error.to_string().contains("not a login shell"));
    }

    #[test]
    fn source_spar_file_persists_declarations_and_resolves_relative_imports() {
        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join("helper.spar");
        let sourced = directory.path().join("functions.spar");
        std::fs::write(
            &helper,
            r#"export function suffix(value: str) -> str { return "${value}-ok"; };"#,
        )
        .unwrap();
        std::fs::write(
            &sourced,
            r#"import { suffix } from "./helper";
function greet(name: str) -> str { return suffix(value: name); };"#,
        )
        .unwrap();
        let mut session = ShellSession::new();

        session
            .submit(&format!("source {}", sourced.display()))
            .unwrap();

        let ShellResult::Value(ConfigValue::Str(value)) =
            session.submit(r#"greet(name: "OCC")"#).unwrap()
        else {
            panic!("expected sourced function result");
        };
        assert_eq!(value, "OCC-ok");
    }

    #[test]
    fn dot_is_an_alias_for_source_in_the_current_spar_session() {
        let directory = tempfile::tempdir().unwrap();
        let sourced = directory.path().join("values.spar");
        std::fs::write(&sourced, r#"var sourcedValue: str = "loaded";"#).unwrap();
        let mut session = ShellSession::new();

        session.submit(&format!(". {}", sourced.display())).unwrap();

        let ShellResult::Value(ConfigValue::Str(value)) = session.submit("sourcedValue").unwrap()
        else {
            panic!("expected sourced variable");
        };
        assert_eq!(value, "loaded");
    }

    #[test]
    fn foreign_source_imports_environment_and_working_directory_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("activated");
        std::fs::create_dir(&target).unwrap();
        let script = directory.path().join("activate.sh");
        std::fs::write(
            &script,
            format!(
                "export VIRTUAL_ENV='{0}/.venv'\nexport SPARSH_ACTIVATED=yes\ncd '{1}'\n",
                directory.path().display(),
                target.display()
            ),
        )
        .unwrap();
        let mut session = ShellSession::new();

        session
            .submit(&format!("source --shell sh {}", script.display()))
            .unwrap();

        assert_eq!(session.ui_snapshot().cwd(), target.as_path());
        let exported = builtin_stdout(session.submit("export").unwrap());
        assert!(exported.contains("SPARSH_ACTIVATED='yes'"), "{exported}");
        assert!(exported.contains("VIRTUAL_ENV="), "{exported}");
    }


    #[test]
    fn deactivate_restores_environment_changed_by_python_activation_only() {
        let directory = tempfile::tempdir().unwrap();
        let venv = directory.path().join(".venv");
        let bin = venv.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join("activate");
        std::fs::write(
            &script,
            format!(
                "export VIRTUAL_ENV='{0}'\nexport VIRTUAL_ENV_PROMPT='(.venv) '\nexport PATH='{1}':\"$PATH\"\nexport ACTIVATION_ONLY=yes\n",
                venv.display(),
                bin.display(),
            ),
        )
        .unwrap();

        let mut session = ShellSession::new();
        session.submit("export USER_DURING_VENV=before").unwrap();
        let before_path = session
            .ui_snapshot()
            .environment_value("PATH")
            .map(std::ffi::OsString::from)
            .expect("PATH before activation");

        session
            .submit(&format!("source --shell sh {}", script.display()))
            .unwrap();
        session.submit("export USER_DURING_VENV=after").unwrap();

        assert_eq!(
            session.ui_snapshot().environment_value("VIRTUAL_ENV"),
            Some(venv.as_os_str())
        );
        assert_eq!(
            session.ui_snapshot().environment_value("ACTIVATION_ONLY"),
            Some(std::ffi::OsStr::new("yes"))
        );

        session.submit("deactivate").unwrap();

        let snapshot = session.ui_snapshot();
        assert!(snapshot.environment_value("VIRTUAL_ENV").is_none());
        assert!(snapshot.environment_value("VIRTUAL_ENV_PROMPT").is_none());
        assert!(snapshot.environment_value("ACTIVATION_ONLY").is_none());
        assert_eq!(
            snapshot.environment_value("PATH"),
            Some(before_path.as_os_str())
        );
        assert_eq!(
            snapshot.environment_value("USER_DURING_VENV"),
            Some(std::ffi::OsStr::new("after"))
        );
    }

    #[test]
    fn deactivate_without_sourced_python_environment_is_an_error() {
        let mut session = ShellSession::new();

        let error = session.submit("deactivate").unwrap_err();

        assert_eq!(error.status(), 1);
        assert!(error.to_string().contains("no active Python virtual environment"));
    }

    #[test]
    fn misspelled_deactive_suggests_deactivate() {
        let mut session = ShellSession::new();

        let result = session.submit("deactive").unwrap();

        let ShellResult::CommandStatus {
            status: 127,
            diagnostic:
                Some(CommandDiagnostic::NotFound {
                    suggestions,
                    ..
                }),
        } = result
        else {
            panic!("expected structured command-not-found result");
        };
        assert!(
            suggestions.iter().any(|name| name == "deactivate"),
            "{suggestions:?}"
        );
    }

    #[test]
    fn failed_foreign_source_leaves_environment_and_cwd_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("broken.sh");
        std::fs::write(
            &script,
            "export SPARSH_ATOMIC=changed\ncd /tmp\nreturn 7\n",
        )
        .unwrap();
        let mut session = ShellSession::new();
        session.submit("export SPARSH_ATOMIC=before").unwrap();
        let before_cwd = session.ui_snapshot().cwd().to_path_buf();

        let error = session
            .submit(&format!("source --shell sh {}", script.display()))
            .unwrap_err();

        assert_eq!(error.status(), 7);
        assert_eq!(session.ui_snapshot().cwd(), before_cwd.as_path());
        let exported = builtin_stdout(session.submit("export").unwrap());
        assert!(exported.contains("SPARSH_ATOMIC='before'"), "{exported}");
        assert!(!exported.contains("SPARSH_ATOMIC='changed'"), "{exported}");
    }

    #[test]
    fn executable_spar_file_without_shebang_runs_through_spar_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("main.spar");
        let output = directory.path().join("output.txt");
        std::fs::write(
            &script,
            r#"function main() -> int { println(message: "spar-ok"); return 0; };"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut session = ShellSession::new();

        session
            .submit(&format!("{} > {}", script.display(), output.display()))
            .unwrap();

        assert_eq!(std::fs::read_to_string(output).unwrap(), "spar-ok\n");
    }

    #[test]
    fn executable_spar_file_can_feed_an_external_pipeline() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("main.spar");
        let output = directory.path().join("filtered.txt");
        std::fs::write(
            &script,
            r#"function main() -> int { println(message: "alpha"); println(message: "beta"); return 0; };"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut session = ShellSession::new();

        session
            .submit(&format!(
                "{} | grep beta > {}",
                script.display(),
                output.display()
            ))
            .unwrap();

        assert_eq!(std::fs::read_to_string(output).unwrap(), "beta\n");
    }

    #[test]
    fn executable_dot_spar_with_foreign_shebang_respects_that_interpreter() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("foreign.spar");
        let output = directory.path().join("foreign.txt");
        std::fs::write(&script, "#!/bin/sh\nprintf foreign\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut session = ShellSession::new();

        session
            .submit(&format!("{} > {}", script.display(), output.display()))
            .unwrap();

        assert_eq!(std::fs::read_to_string(output).unwrap(), "foreign");
    }

    #[test]
    fn executable_spar_path_still_requires_executable_permission() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("main.spar");
        std::fs::write(&script, "function main() -> int { return 0; };").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut session = ShellSession::new();

        let error = session.submit(script.to_str().unwrap()).unwrap_err();

        assert_eq!(error.status(), 127);
        assert!(error.to_string().contains("not executable"), "{error}");
    }

    #[test]
    fn chmod_remains_an_external_command() {
        let session = ShellSession::new();
        assert_eq!(session.ui_snapshot().classify_command("chmod"), CommandKind::External);
    }

    #[test]
    fn repl_builtin_requests_repl_editor_mode() {
        let mut session = ShellSession::new();
        assert!(matches!(
            session.submit("repl").unwrap(),
            ShellResult::EditorMode(super::EditorMode::Repl)
        ));
    }

}
