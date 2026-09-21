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

fn listing_error(message: String) -> ShellResult {
    ShellResult::Builtin(crate::BuiltinOutput {
        stdout: Vec::new(),
        stderr: format!("{message}\n").into_bytes(),
        status: 2,
    })
}

#[derive(Debug)]
pub enum ShellResult {
    Empty,
    Value(spar::ConfigValue),
    Structured(spar::InteractiveRuntimeValue),
    EditorMode(EditorMode),
    ReloadConfig,
    /// A builtin that printed output and also requested a config reload;
    /// the session reloads, then hands the output back as `Builtin`.
    #[doc(hidden)]
    ReloadConfigWith(BuiltinOutput),
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
    /// Spar errors for `source`, the exact text that was given to Spar. Their
    /// spans are relative to it, so the error renders under the right line.
    pub fn from_spar(errors: Vec<spar::SparError>, source: &str) -> Self {
        Self::SparSource {
            errors,
            source: source.to_string(),
            filename: "<sparsh>".to_string(),
        }
    }

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

/// What an interactive prompt needs from its Spar session: structured results
/// (tables, pretty JSON) rendered by the prompt, and the `std/data` functions
/// (`where`, `map`, `take`, ...) usable without an `import`.
fn configure_terminal_session(session: &mut spar::Session) {
    session.set_structured_terminal(true);
    session.enable_data_prelude();
}

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
    last_interactive_value: Option<spar::Value>,
    config_notices: Vec<String>,
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
            last_interactive_value: None,
            config_notices: Vec::new(),
        })
    }

    pub fn try_new_interactive() -> Result<Self, ShellError> {
        let mut session = Self::try_new()?;
        session.mode = SessionMode::InteractiveTty;
        configure_terminal_session(&mut session.spar);
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
            .eval_interactive_preview_with_context(
                source,
                &cwd,
                &environment,
                self.last_interactive_value.clone(),
                20,
            )
            .map_err(|errors| ShellError::from_spar(errors, source))?;
        let committed = self.spar.committed_source();
        let appended = if committed == before {
            String::new()
        } else if before.is_empty() {
            committed.to_string()
        } else {
            committed
                .strip_prefix(&before)
                .and_then(|suffix| suffix.strip_prefix('\n'))
                .unwrap_or_default()
                .to_string()
        };
        append_fragment(&mut self.interactive_source, &appended);
        let result = match result {
            spar::InteractivePreviewResult::Empty => ShellResult::Empty,
            spar::InteractivePreviewResult::Value(value) => self.handle_interactive_value(value)?,
            spar::InteractivePreviewResult::RuntimeValue(value) => ShellResult::Structured(value),
            spar::InteractivePreviewResult::Process(outcome) => ShellResult::Process(outcome),
        };
        self.remember_interactive_value(&result);
        Ok(result)
    }

    pub fn submit(&mut self, input: &str) -> Result<ShellResult, ShellError> {
        self.poll_jobs()?;
        let cwd = self.services.directories.current().to_path_buf();
        let environment = self.services.environment.snapshot();
        if input.trim() == "_" {
            if let Some(value) = self.last_interactive_value.clone() {
                let result = match value.clone().try_into_config(&spar::Span::dummy()) {
                    Ok(value) => self.present_value(value),
                    Err(_) => ShellResult::Structured(spar::InteractiveRuntimeValue {
                        value,
                        stream_preview: false,
                        truncated: false,
                        presentation: spar::InteractivePresentation::Value,
                    }),
                };
                return self.finish_submission(Ok(result));
            }
            let result = self.submit_spar(input);
            return self.finish_submission(result);
        }
        if self.mode == SessionMode::InteractiveTty {
            if let Some(result) = self.value_pipeline_from_structured_source(input, &cwd) {
                return self.finish_submission(result);
            }
            if let Some(request) = crate::listing::parse_request(input) {
                let result = match crate::listing::list(&request, &cwd) {
                    Ok(value) => ShellResult::Structured(spar::InteractiveRuntimeValue {
                        value,
                        stream_preview: false,
                        truncated: false,
                        presentation: spar::InteractivePresentation::Pipeline,
                    }),
                    Err(message) => listing_error(message),
                };
                return self.finish_submission(Ok(result));
            }
        }
        if let Some(plan) = crate::function_pipeline::compose_function_pipeline(
            input,
            &self.spar,
            &cwd,
            &environment,
        )? {
            let result = execute_plan(
                &plan,
                &self.builtins,
                &mut self.services,
                self.last_status,
                self.mode,
            );
            return self.finish_submission(result);
        }

        // Mixed byte/value pipelines are shell syntax rather than ordinary Spar
        // expressions. Execute the raw prompt body through Spar's declaration-safe
        // interactive shell preview path so a terminal pipeline may return a
        // structured value without committing a synthetic `shell { ... }` wrapper.
        if crate::dispatch::is_mixed_byte_pipeline(input) {
            let preview = self
                .spar
                .eval_interactive_shell_preview_with_context(
                    input,
                    &cwd,
                    &environment,
                    self.last_interactive_value.clone(),
                    20,
                )
                .map_err(|errors| ShellError::from_spar(errors, input.trim()));
            let result = preview.map(|preview| match preview {
                spar::InteractivePreviewResult::RuntimeValue(value) => {
                    ShellResult::Structured(value)
                }
                spar::InteractivePreviewResult::Process(outcome) => ShellResult::Process(outcome),
                spar::InteractivePreviewResult::Empty => ShellResult::Empty,
                spar::InteractivePreviewResult::Value(value) => ShellResult::Value(value),
            });
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
            Dispatch::Command(command) => self
                .spar
                .eval_shell_plan_with_context(command, &cwd, &environment)
                .map_err(|errors| ShellError::from_spar(errors, command))
                .and_then(|plan| {
                    execute_plan(
                        &plan,
                        &self.builtins,
                        &mut self.services,
                        self.last_status,
                        self.mode,
                    )
                }),
        };

        self.finish_submission(result)
    }

    /// Submission for scripts (`-c`, piped stdin). A string returned by a Spar
    /// expression is data, so it is written as-is; a bare variable name still
    /// shows the quoted, Spar-literal form, as does the prompt.
    pub fn submit_script(&mut self, input: &str) -> Result<ShellResult, ShellError> {
        let result = self.submit(input)?;
        Ok(match result {
            ShellResult::Value(spar::ConfigValue::Str(text))
                if !crate::dispatch::is_bare_identifier(input.trim()) =>
            {
                ShellResult::Builtin(crate::BuiltinOutput {
                    stdout: format!("{text}\n").into_bytes(),
                    stderr: Vec::new(),
                    status: 0,
                })
            }
            other => other,
        })
    }

    fn finish_submission(
        &mut self,
        result: Result<ShellResult, ShellError>,
    ) -> Result<ShellResult, ShellError> {
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
            Ok(ShellResult::ReloadConfigWith(output)) => {
                self.reload_config()?;
                self.last_status = output.status;
                Ok(ShellResult::Builtin(output))
            }
            Ok(result) => {
                self.remember_interactive_value(&result);
                self.last_status = match &result {
                    ShellResult::Empty
                    | ShellResult::Value(_)
                    | ShellResult::Structured(_)
                    | ShellResult::EditorMode(_)
                    | ShellResult::ReloadConfig
                    | ShellResult::ReloadConfigWith(_)
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

    fn remember_interactive_value(&mut self, result: &ShellResult) {
        match result {
            ShellResult::Value(value) => {
                self.last_interactive_value = Some(spar::Value::from_config(value.clone()));
            }
            ShellResult::Structured(value) => {
                // Stream previews are materialized by Spar before reaching Sparsh,
                // so `_` never holds a one-shot live Stream resource.
                self.last_interactive_value = Some(value.value.clone());
            }
            _ => {}
        }
    }

    pub fn set_history_access(
        &mut self,
        history: std::sync::Arc<dyn crate::history::HistoryAccess>,
    ) {
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

    fn source_request(
        &mut self,
        request: crate::builtin::SourceRequest,
    ) -> Result<ShellResult, ShellError> {
        self.source_path(&request.path, request.shell.as_deref())?;
        Ok(ShellResult::Empty)
    }

    fn source_path(
        &mut self,
        raw_path: &str,
        foreign_shell: Option<&str>,
    ) -> Result<(), ShellError> {
        let path = self.resolve_session_path(raw_path)?;
        if foreign_shell.is_none() && path.extension().and_then(|ext| ext.to_str()) == Some("spar")
        {
            return self.source_spar_file(&path);
        }
        self.source_foreign_file(&path, foreign_shell)
    }

    fn resolve_session_path(&self, raw: &str) -> Result<PathBuf, ShellError> {
        let expanded = if raw == "~" {
            PathBuf::from(self.services.environment.get("HOME").ok_or_else(|| {
                ShellError::Process {
                    message: "HOME is not set; cannot expand '~'".into(),
                    status: 1,
                }
            })?)
        } else if let Some(rest) = raw.strip_prefix("~/") {
            PathBuf::from(self.services.environment.get("HOME").ok_or_else(|| {
                ShellError::Process {
                    message: "HOME is not set; cannot expand '~'".into(),
                    status: 1,
                }
            })?)
            .join(rest)
        } else {
            let path = Path::new(raw);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.services.directories.current().join(path)
            }
        };
        Ok(expanded)
    }

    fn source_spar_file(&mut self, path: &Path) -> Result<(), ShellError> {
        let source = std::fs::read_to_string(path).map_err(|error| ShellError::Process {
            message: format!("source: {}: {error}", path.display()),
            status: 1,
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

    fn source_foreign_file(
        &mut self,
        path: &Path,
        requested_shell: Option<&str>,
    ) -> Result<(), ShellError> {
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
                message: format!(
                    "source: unsupported foreign shell '{shell}'; expected sh, bash, or zsh"
                ),
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
                message: format!("source: failed to start {shell}: {error}"),
                status: 1,
            })?;
        if !output.status.success() {
            let status = output.status.code().unwrap_or(1);
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ShellError::Process {
                message: if stderr.is_empty() {
                    format!("source: {} exited with status {status}", path.display())
                } else {
                    stderr
                },
                status,
            });
        }
        let mut fields = output.stdout.split(|byte| *byte == 0);
        let cwd = fields
            .next()
            .filter(|field| !field.is_empty())
            .ok_or_else(|| ShellError::Process {
                message: "source: foreign shell returned no working directory".into(),
                status: 1,
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
            let Some(index) = field.iter().position(|byte| *byte == b'=') else {
                continue;
            };
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
                message: format!("source: resulting cwd {} is not a directory", cwd.display()),
                status: 1,
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
            spar::ConfigValue::Shell(plan) => execute_plan(
                &plan,
                &self.builtins,
                &mut self.services,
                self.last_status,
                self.mode,
            ),
            other => Ok(self.present_value(other)),
        }
    }

    /// Records and lists of records are data, not one-line literals: at a
    /// terminal they are handed to the prompt as structured values so it can
    /// draw tables and trees. Scalars and scripts keep the Spar literal form.
    fn present_value(&self, value: spar::ConfigValue) -> ShellResult {
        let is_container = |value: &spar::ConfigValue| {
            matches!(
                value,
                spar::ConfigValue::Section(_) | spar::ConfigValue::List(_)
            )
        };
        let structured = self.mode == SessionMode::InteractiveTty
            && match &value {
                spar::ConfigValue::Section(fields) => !fields.is_empty(),
                spar::ConfigValue::List(items) => items.iter().any(is_container),
                _ => false,
            };
        if !structured {
            return ShellResult::Value(value);
        }
        ShellResult::Structured(spar::InteractiveRuntimeValue {
            value: spar::Value::from_config(value),
            stream_preview: false,
            truncated: false,
            presentation: spar::InteractivePresentation::Value,
        })
    }

    /// `ls |> stages` and `_ |> to FORMAT`: a structured source feeding a value
    /// pipeline. `to FORMAT` encodes the value for display; anything else runs
    /// as `_ |> stages` on the fresh listing.
    fn value_pipeline_from_structured_source(
        &mut self,
        input: &str,
        cwd: &std::path::Path,
    ) -> Option<Result<ShellResult, ShellError>> {
        let (value, stages) =
            if let Some((request, stages)) = crate::listing::parse_value_pipeline(input) {
                match crate::listing::list(&request, cwd) {
                    Ok(value) => (value, stages),
                    Err(message) => return Some(Ok(listing_error(message))),
                }
            } else {
                let (source, stages) = input.split_once("|>")?;
                if source.trim() != "_" {
                    return None;
                }
                (self.last_interactive_value.clone()?, stages.trim())
            };
        if let Some(format) = crate::listing::encode_stage(stages) {
            let registry = spar::StructuredFormatRegistry::builtin();
            let Some(descriptor) = registry.descriptor(format) else {
                return Some(Ok(listing_error(format!(
                    "to: unknown format `{format}`; try json, yaml, toml, csv, tsv, jsonl, lines or text"
                ))));
            };
            self.last_interactive_value = Some(value.clone());
            return Some(Ok(ShellResult::Structured(spar::InteractiveRuntimeValue {
                value,
                stream_preview: false,
                truncated: false,
                presentation: spar::InteractivePresentation::Encoded(descriptor.name()),
            })));
        }
        self.last_interactive_value = Some(value);
        Some(self.submit_spar(&format!("_ |> {stages}")))
    }

    pub fn take_config_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.config_notices)
    }

    pub fn reload_config(&mut self) -> Result<(), ShellError> {
        self.config_notices.clear();
        let store = crate::config_home::store_for(&self.services.environment);
        let mut package_root: Option<PathBuf> = None;
        if let Some(home) = self.services.environment.get("HOME").map(PathBuf::from) {
            match crate::config_home::prepare(&home, &store) {
                Ok(crate::config_home::Prepared::LegacyKept { notice }) => {
                    self.config_notices.push(notice);
                }
                Ok(_) => package_root = Some(crate::config_home::root_for_home(&home)),
                Err(error) => self.config_notices.push(error.to_string()),
            }
        }
        let (source, base_dir) = match crate::config::config_path(&self.services.environment) {
            Some(path) => {
                let source = crate::config::read_source(&path).map_err(ShellError::Config)?;
                let base = path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf();
                (source, base)
            }
            None => (
                String::new(),
                self.services.directories.current().to_path_buf(),
            ),
        };
        let mut engine = spar::Engine::default()
            .with_base_dir(base_dir)
            .with_package_command("pkg");
        if let Some(root) = &package_root {
            match crate::config_home::locator_for(root, &store) {
                Ok(Some(locator)) => engine = engine.with_locator(locator),
                Ok(None) => {}
                Err(message) => {
                    return Err(ShellError::Config(crate::ConfigLoadError::Invalid(message)))
                }
            }
        }
        let mut candidate = engine.session();
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
        // The replacement session must keep the terminal features, or a
        // config reload at startup silently turns them off.
        if self.mode == SessionMode::InteractiveTty {
            configure_terminal_session(&mut candidate);
        }
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

    pub fn keybindings(&self) -> &[crate::KeybindingConfig] {
        &self.config.keybindings
    }

    /// Pager keys: the built-in set with `pagerKeybindings` applied on top.
    pub fn pager_keybindings(&self) -> Vec<crate::PagerKeybindingConfig> {
        crate::merged_pager_keybindings(&self.config.pager_keybindings)
    }

    /// The last structured value, i.e. what `_` holds.
    pub fn last_structured_value(&self) -> Option<&spar::Value> {
        self.last_interactive_value.as_ref()
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
    if fragment.trim().is_empty() {
        return;
    }
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
            while let Some(job) = self.services.jobs.get(id) {
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
            let entries = self
                .entries
                .lock()
                .map_err(|_| "history lock poisoned".to_string())?;
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

        session
            .reload_config()
            .expect("prompt problems must not fail the reload");

        assert_ne!(
            session.config_generation(),
            before,
            "the config was applied"
        );
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
    fn reload_applies_keybinding_overrides_and_bumps_editor_generation() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".sparsh")).unwrap();
        std::fs::write(
            home.path().join(".sparsh/sparsh.spar"),
            format!(
                "{}\nstruct Config: SparshConfig {{\n    keybindings = [{{ key: \"ctrl+l\"; action: \"clearScreen\"; }}];\n}};\n",
                include_str!("../../../examples/sparsh-types.spar")
            ),
        )
        .unwrap();
        let mut session = ShellSession::new();
        session
            .submit(&format!("export HOME={}", home.path().display()))
            .unwrap();
        let before = session.config_generation();

        session.reload_config().unwrap();

        assert_ne!(session.config_generation(), before);
        assert_eq!(session.keybindings().len(), 1);
        assert_eq!(
            session.keybindings()[0].action,
            crate::KeybindingAction::ClearScreen
        );
        assert_eq!(
            session.keybindings()[0].chord,
            crate::KeyChord::parse("ctrl+l").unwrap()
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
        // Resolves external programs through PATH, which other tests mutate.
        let _lock = PROCESS_STATE.lock().unwrap();
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
        // `printf` is a native builtin, so the recovered branch reports either
        // a process outcome or builtin output — both are a success.
        match result {
            ShellResult::Process(outcome) => {
                assert!(outcome.success);
                assert_eq!(outcome.exit_code, 0);
            }
            ShellResult::Builtin(output) => {
                assert_eq!(output.status, 0);
                assert_eq!(output.stdout, b"recovered");
            }
            other => panic!("expected process or builtin outcome, got {other:?}"),
        }
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
            .submit(
                r#"function writeMarker(output: str) -> shell {
    return shell {
        var value: str = $(printf ready);
        printf "%s" "${value}" > "${output}";
    };
};"#,
            )
            .unwrap();
        session
            .submit(&format!(r#"writeMarker(output: "{}")"#, output.display()))
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
            .submit(
                r#"function writeMarkers(output: str) -> shell {
    return shell {
        var values: List<str> = ["one", "two"];
        for value in values {
            touch "${output}-${value}";
        }
    };
};"#,
            )
            .unwrap();
        session
            .submit(&format!(r#"writeMarkers(output: "{}")"#, output.display()))
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
        session.submit("var plan: shell = build();").unwrap();
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
            .submit(&format!(
                "ls {} > {}",
                directory.path().display(),
                output.display()
            ))
            .unwrap();

        assert!(std::fs::read_to_string(output)
            .unwrap()
            .contains("visible-file"));
    }

    #[test]
    fn practical_io_and_help_builtins_are_native_and_pipeline_capable() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("printf.txt");
        let input = directory.path().join("read.txt");
        std::fs::write(&input, "line-value\n").unwrap();
        let mut session = ShellSession::new();

        assert_eq!(
            builtin_stdout(session.submit("echo hello Sparsh").unwrap()),
            "hello Sparsh\n"
        );
        session
            .submit(&format!(
                "printf '%s\\n' alpha | grep alpha > {}",
                output.display()
            ))
            .unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "alpha\n");

        session
            .submit(&format!("read SPARSH_READ_VALUE < {}", input.display()))
            .unwrap();
        let exported = builtin_stdout(session.submit("export").unwrap());
        assert!(
            exported.contains("SPARSH_READ_VALUE='line-value'"),
            "{exported}"
        );

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
            r#"function suffix(value: str) -> str { return "${value}-ok"; };"#,
        )
        .unwrap();
        std::fs::write(
            &sourced,
            r#"import "./helper.spar" as helper;
function greet(name: str) -> str { return helper::suffix(value: name); };"#,
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
        assert!(error
            .to_string()
            .contains("no active Python virtual environment"));
    }

    #[test]
    fn misspelled_deactive_suggests_deactivate() {
        let mut session = ShellSession::new();

        let result = session.submit("deactive").unwrap();

        let ShellResult::CommandStatus {
            status: 127,
            diagnostic: Some(CommandDiagnostic::NotFound { suggestions, .. }),
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
        std::fs::write(&script, "export SPARSH_ATOMIC=changed\ncd /tmp\nreturn 7\n").unwrap();
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
        assert_eq!(
            session.ui_snapshot().classify_command("chmod"),
            CommandKind::External
        );
    }

    #[test]
    fn expression_only_preview_is_not_added_to_replay_source() {
        let mut session = ShellSession::new();

        session
            .submit_spar("42")
            .expect("expression preview should succeed");

        assert!(session.interactive_source.is_empty());
    }

    #[test]
    fn declaration_plus_preview_replays_only_the_committed_declaration() {
        let mut session = ShellSession::new();

        let result = session
            .submit_spar("var answer: int = 41; answer + 1")
            .expect("declaration plus preview should succeed");
        assert!(matches!(
            result,
            ShellResult::Value(spar::ConfigValue::Int(42))
        ));
        assert!(session.interactive_source.contains("var answer: int = 41"));
        assert!(
            !session.interactive_source.contains("answer + 1"),
            "preview-only expressions must not be replayed after config reload"
        );

        session
            .submit("unset HOME")
            .expect("HOME should be removable in the isolated shell environment");
        session
            .reload_config()
            .expect("reloading should replay only committed interactive source");

        let answer = session
            .submit_spar("answer")
            .expect("replayed declaration should remain available");
        assert!(matches!(
            answer,
            ShellResult::Value(spar::ConfigValue::Int(41))
        ));
    }

    #[test]
    fn direct_mixed_pipeline_without_to_returns_a_structured_table() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit_spar(r#"import pkg { where } from "std/data";"#)
            .unwrap();

        let result = session
            .submit(
                "printf 'name,age,team\\nObi,24,core\\nAda,31,ops\\n' | from csv |> where(fn(row) => row.age > 20)",
            )
            .expect("direct prompt mixed pipeline should execute");

        let ShellResult::Structured(preview) = result else {
            panic!("expected structured result, got {result:?}");
        };
        assert_eq!(
            preview.presentation,
            spar::InteractivePresentation::Pipeline
        );
        let spar::Value::Table(table) = preview.value else {
            panic!("expected table");
        };
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn interactive_ls_returns_a_structured_listing_with_hidden_files_and_wins_over_an_alias() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join(".env"), "A=1").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit(&format!("cd {}", dir.path().display()))
            .unwrap();

        let ShellResult::Structured(preview) = session.submit("ls").unwrap() else {
            panic!("expected structured listing");
        };
        assert_eq!(
            preview.presentation,
            spar::InteractivePresentation::Pipeline
        );
        let spar::Value::Table(table) = preview.value else {
            panic!("expected table");
        };
        let names = table
            .rows()
            .iter()
            .map(|row| match row {
                spar::Value::Object(fields) => format!("{:?}", fields["name"]),
                other => panic!("record expected: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "String(\"src\")",
                "String(\".env\")",
                "String(\"Cargo.toml\")"
            ]
        );
    }

    #[test]
    fn ls_and_underscore_feed_value_pipelines_including_to_format() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello").unwrap();
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit(&format!("cd {}", dir.path().display()))
            .unwrap();

        let ShellResult::Structured(encoded) = session.submit("ls |> to yaml").unwrap() else {
            panic!("expected encoded listing");
        };
        assert_eq!(
            encoded.presentation,
            spar::InteractivePresentation::Encoded("yaml")
        );

        let ShellResult::Structured(taken) = session.submit("ls |> take(1)").unwrap() else {
            panic!("expected sliced listing");
        };
        let spar::Value::Table(table) = taken.value else {
            panic!("table expected");
        };
        assert_eq!(table.len(), 1);

        let ShellResult::Structured(again) = session.submit("_ |> to json").unwrap() else {
            panic!("expected encoded value");
        };
        assert_eq!(
            again.presentation,
            spar::InteractivePresentation::Encoded("json")
        );

        let ShellResult::Builtin(error) = session.submit("ls |> to wat").unwrap() else {
            panic!("expected an error message");
        };
        assert!(String::from_utf8_lossy(&error.stderr).contains("unknown format `wat`"));
    }

    #[test]
    fn interactive_ls_with_pipes_or_unknown_flags_still_runs_the_external_command() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        let piped = session.submit("ls | cat").unwrap();
        assert!(!matches!(piped, ShellResult::Structured(_)), "{piped:?}");
    }

    #[test]
    fn direct_scoc_env_without_to_reuses_structured_table_presentation() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        let result = session
            .submit("printf 'A=1\\nB=2\\n' | from env")
            .expect("SCOC env prompt pipeline should execute");

        let ShellResult::Structured(preview) = result else {
            panic!("expected structured result, got {result:?}");
        };
        assert_eq!(
            preview.presentation,
            spar::InteractivePresentation::Pipeline
        );
        let spar::Value::Table(table) = preview.value else {
            panic!("expected SCOC env table");
        };
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn direct_terminal_to_json_returns_encoded_structured_result() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        let result = session
            .submit("printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> to json")
            .unwrap();

        let ShellResult::Structured(preview) = result else {
            panic!("expected structured result, got {result:?}");
        };
        assert_eq!(
            preview.presentation,
            spar::InteractivePresentation::Encoded("json")
        );
    }

    #[test]
    fn direct_mixed_preview_keeps_full_value_in_underscore_and_failure_does_not_replace_it() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        let rows = (0..75)
            .map(|n| format!("row{n},{n}"))
            .collect::<Vec<_>>()
            .join("\\n");
        let command = format!("printf 'name,value\\n{rows}\\n' | from csv");

        let first = session.submit(&command).unwrap();
        let ShellResult::Structured(first) = first else {
            panic!("expected structured result");
        };
        let spar::Value::Table(table) = &first.value else {
            panic!("expected table");
        };
        assert_eq!(table.len(), 75);

        assert!(session
            .submit("printf 'x\\n1\\n' | from csv |> definitelyMissing()")
            .is_err());

        let recalled = session.submit("_").unwrap();
        let ShellResult::Structured(recalled) = recalled else {
            panic!("expected structured underscore");
        };
        let spar::Value::Table(table) = recalled.value else {
            panic!("expected table");
        };
        assert_eq!(table.len(), 75);
    }

    #[test]
    fn structured_result_becomes_the_previous_interactive_value() {
        let mut session = ShellSession::new();
        let result = session
            .submit_spar(
                r#"
                import pkg { collectTable } from "std/data";
                struct User { name: str = ""; age: int = 0; };
                [User(name: "Obi", age: 24), User(name: "Ada", age: 31)]
                    |> collectTable()
                "#,
            )
            .expect("structured preview should succeed");
        assert!(matches!(result, ShellResult::Structured(_)));

        let previous = session
            .submit("_")
            .expect("underscore should return the previous value");
        let ShellResult::Structured(previous) = previous else {
            panic!("expected structured previous value");
        };
        let spar::Value::Table(table) = previous.value else {
            panic!("expected previous Table value");
        };
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn interactive_session_returns_mixed_pipeline_as_a_structured_value() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        let result = session
            .submit("printf 'name,age\\nObi,24\\n' | from csv |> to json")
            .unwrap();
        assert!(matches!(result, ShellResult::Structured(_)), "{result:?}");
        let bare = session
            .submit("printf 'name,age\\nObi,24\\n' | from csv")
            .unwrap();
        assert!(matches!(bare, ShellResult::Structured(_)), "bare: {bare:?}");

        // Loading the config replaces the Spar session; that must not turn
        // structured rendering off (this broke the real prompt).
        session.submit("unset HOME").unwrap();
        session.reload_config().unwrap();
        let after_reload = session
            .submit("printf 'name,age\\nObi,24\\n' | from csv")
            .unwrap();
        assert!(
            matches!(after_reload, ShellResult::Structured(_)),
            "after reload: {after_reload:?}"
        );
    }

    #[test]
    fn interactive_prompt_can_use_data_functions_without_an_import_even_after_reload() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        let source =
            "printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> where(fn(r) => r.age > 24)";
        let first = session.submit(source).unwrap();
        assert!(matches!(first, ShellResult::Structured(_)), "{first:?}");

        session.submit("unset HOME").unwrap();
        session.reload_config().unwrap();
        let second = session.submit(source).unwrap();
        assert!(matches!(second, ShellResult::Structured(_)), "{second:?}");

        // The user's own names still win, and an explicit import is harmless.
        session.submit("var mut count: int = 0;").unwrap();
        session.submit("count = count + 1;").unwrap();
        session
            .submit("import pkg { take } from \"std/data\";")
            .unwrap();
        let third = session.submit(source).unwrap();
        assert!(matches!(third, ShellResult::Structured(_)), "{third:?}");
    }

    #[test]
    fn scripts_still_need_an_explicit_data_import() {
        let mut session = ShellSession::try_new().unwrap();
        let result = session.submit("printf 'a\\n1\\n' | from csv |> where(fn(r) => r.a > 0)");
        assert!(result.is_err(), "{result:?}");
    }

    /// Serves the same canned HTTP response `requests` times on a loopback
    /// port and returns its URL.
    fn serve(requests: usize, content_type: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}/")
    }

    fn serve_once(content_type: &'static str, body: &'static str) -> String {
        serve(1, content_type, body)
    }

    #[test]
    fn await_at_the_prompt_fetches_and_keeps_the_response_for_underscore() {
        let url = serve_once("application/json", r#"{"name":"ditto","cry":null}"#);
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("import pkg { get } from \"std/http\";")
            .unwrap();

        // No promise is shown: `await` waits and returns the response.
        let result = session
            .submit(&format!("await get(url: \"{url}\")"))
            .unwrap();
        let ShellResult::Structured(response) = result else {
            panic!("expected a structured response, got {result:?}");
        };
        let spar::Value::Object(fields) = &response.value else {
            panic!("expected a record");
        };
        assert_eq!(fields.get("status"), Some(&spar::Value::Int(200)));
        assert_eq!(
            fields.get("contentType"),
            Some(&spar::Value::String("application/json".into()))
        );

        // `_` is the response, so `.json()` works on it, and the JSON null
        // survives instead of turning into 0.
        let ShellResult::Structured(json) = session.submit("_.json()").unwrap() else {
            panic!("expected the decoded JSON record");
        };
        let spar::Value::Object(record) = &json.value else {
            panic!("expected a record");
        };
        assert_eq!(
            record.get("name"),
            Some(&spar::Value::String("ditto".into()))
        );
        assert!(
            matches!(record.get("cry"), Some(spar::Value::Void)),
            "{record:?}"
        );

        // The decoded record is now `_`; its fields are reachable.
        let name = session.submit("_.name").unwrap();
        assert!(
            matches!(&name, ShellResult::Value(spar::ConfigValue::Str(text)) if text == "ditto"),
            "{name:?}"
        );
    }

    #[test]
    fn response_status_is_reachable_through_underscore() {
        let url = serve_once("text/html", "<p>hi</p>");
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("import pkg { get } from \"std/http\";")
            .unwrap();
        session
            .submit(&format!("await get(url: \"{url}\")"))
            .unwrap();

        let status = session.submit("_.status").unwrap();
        assert!(
            matches!(&status, ShellResult::Value(spar::ConfigValue::Int(200))),
            "{status:?}"
        );
    }

    const STATS: &str = r#"{"name":"ditto","stats":[{"stat":"hp","base":48},{"stat":"attack","base":48},{"stat":"speed","base":48},{"stat":"special","base":10}],"meta":{"id":1}}"#;

    #[test]
    fn a_fetched_json_list_can_be_chained_through_where_select_and_take_in_one_line() {
        let url = serve(3, "application/json", STATS);
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("import pkg { get } from \"std/http\";")
            .unwrap();

        let ShellResult::Structured(rows) = session
            .submit(&format!(
                "(await get(url: \"{url}\")).json().stats |> where(fn(s) => s.base > 40) |> select([\"stat\"])"
            ))
            .unwrap()
        else {
            panic!("expected a structured result");
        };
        let spar::Value::List(items) = &rows.value else {
            panic!("expected a list of records, got {:?}", rows.value);
        };
        assert_eq!(items.len(), 3);

        // `count` ends the chain with a plain number.
        let count = session
            .submit(&format!(
                "(await get(url: \"{url}\")).json().stats |> where(fn(s) => s.stat == \"hp\") |> count()"
            ))
            .unwrap();
        assert!(
            matches!(&count, ShellResult::Value(spar::ConfigValue::Int(1))),
            "{count:?}"
        );

        // A record where a list is needed fails with a clear message.
        let error = session
            .submit(&format!(
                "(await get(url: \"{url}\")).json().meta |> take(1)"
            ))
            .unwrap_err();
        assert!(error.to_string().contains("list"), "unclear error: {error}");
    }

    #[test]
    fn repl_builtin_requests_repl_editor_mode() {
        let mut session = ShellSession::new();
        assert!(matches!(
            session.submit("repl").unwrap(),
            ShellResult::EditorMode(super::EditorMode::Repl)
        ));
    }

    fn write_tools_package(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("spar.package.spar"),
            "struct Package: SparPackage {\n    name = \"my-tools\";\n    version = \"1.0.0\";\n    kind = \"library\";\n};\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.spar"),
            "function dismantler() -> int { return 42; };\n",
        )
        .unwrap();
    }

    fn session_with_home(home: &std::path::Path) -> ShellSession {
        let mut session = ShellSession::new();
        session
            .submit(&format!("export HOME={}", home.display()))
            .unwrap();
        session
            .submit("unset XDG_DATA_HOME XDG_CACHE_HOME")
            .unwrap();
        session
    }

    /// A HOME with a seeded config package that depends on `my-tools` (a `path:` dependency).
    fn home_with_tools_dependency(config_source: &str) -> (tempfile::TempDir, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        write_tools_package(tools.path());
        let store = crate::config_home::store_for_paths_for_tests(home.path());
        crate::config_home::prepare(home.path(), &store).unwrap();
        let root = home.path().join(".sparsh");
        spar::package::commands::add(
            &root,
            "myTools",
            &format!("path:{}", tools.path().display()),
            &spar::package::GitCommandProvider::default(),
            spar::package::NetworkPolicy::Offline,
            &store,
        )
        .unwrap();
        std::fs::write(root.join("src/config.spar"), config_source).unwrap();
        (home, tools)
    }

    #[test]
    fn startup_migrates_a_flat_config_and_keeps_it_working() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".sparsh")).unwrap();
        std::fs::write(
            home.path().join(".sparsh/sparsh.spar"),
            format!(
                "{}\nstruct Config: SparshConfig {{\n    keybindings = [{{ key: \"ctrl+l\"; action: \"clearScreen\"; }}];\n}};\n",
                include_str!("../../../examples/sparsh-types.spar")
            ),
        )
        .unwrap();
        let mut session = session_with_home(home.path());

        session.reload_config().unwrap();

        assert_eq!(session.keybindings().len(), 1);
        assert!(home.path().join(".sparsh/spar.package.spar").is_file());
        assert!(home.path().join(".sparsh/src/config.spar").is_file());
        assert!(home.path().join(".sparsh.bak/sparsh.spar").is_file());
    }

    #[test]
    fn config_can_import_a_path_dependency() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency(
            "import pkg { dismantler } from \"myTools\";\nvar answer: int = dismantler();\n",
        );
        let mut session = session_with_home(home.path());

        session.reload_config().unwrap();

        let result = session.submit_spar("answer").unwrap();
        assert!(
            matches!(result, ShellResult::Value(spar::ConfigValue::Int(42))),
            "{result:?}"
        );
    }

    #[test]
    fn prompt_can_import_a_dependency_and_call_it() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency("// empty\n");
        let mut session = session_with_home(home.path());
        session.reload_config().unwrap();

        session
            .submit_spar("import pkg { dismantler } from \"myTools\";")
            .unwrap();
        let result = session.submit_spar("dismantler()").unwrap();

        assert!(
            matches!(result, ShellResult::Value(spar::ConfigValue::Int(42))),
            "{result:?}"
        );
    }

    #[test]
    fn script_can_import_a_dependency() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency("// empty\n");
        let mut session = session_with_home(home.path());
        session.reload_config().unwrap();

        let result = session
            .submit_script(
                "import pkg { dismantler } from \"myTools\";\nvar n: int = dismantler();\n",
            )
            .unwrap();

        assert!(
            !matches!(result, ShellResult::CommandStatus { status, .. } if status != 0),
            "{result:?}"
        );
    }

    #[test]
    fn unknown_dependency_alias_points_at_pkg_add() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency("// empty\n");
        let mut session = session_with_home(home.path());
        session.reload_config().unwrap();

        let error = session
            .submit_spar("import pkg { x } from \"nope\";")
            .expect_err("unknown alias must fail");

        let text = format!("{error:?}");
        assert!(text.contains("pkg add nope"), "{text}");
    }

    #[test]
    fn existing_backup_keeps_the_flat_config_and_reports_a_notice() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".sparsh")).unwrap();
        std::fs::create_dir(home.path().join(".sparsh.bak")).unwrap();
        std::fs::write(
            home.path().join(".sparsh/sparsh.spar"),
            "var flat: int = 5;\n",
        )
        .unwrap();
        let mut session = session_with_home(home.path());

        session.reload_config().unwrap();

        assert_eq!(
            session.take_config_notices(),
            vec!["~/.sparsh.bak exists; move it and restart to migrate".to_string()]
        );
        let result = session.submit_spar("flat").unwrap();
        assert!(
            matches!(result, ShellResult::Value(spar::ConfigValue::Int(5))),
            "{result:?}"
        );
    }

    #[test]
    fn malformed_lockfile_fails_reload_with_a_pkg_install_hint() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency("// empty\n");
        std::fs::write(
            home.path().join(".sparsh/spar.package.lock.spar"),
            "not a lock {{{",
        )
        .unwrap();
        let mut session = session_with_home(home.path());

        let error = session
            .reload_config()
            .expect_err("bad lock must fail the reload");

        assert!(error.to_string().contains("pkg install"), "{error}");
    }

    #[test]
    fn pkg_add_makes_the_dependency_importable_without_restart() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        write_tools_package(tools.path());
        let mut session = session_with_home(home.path());
        session.reload_config().unwrap();

        let added = session
            .submit(&format!("pkg add myTools path:{}", tools.path().display()))
            .unwrap();
        let ShellResult::Builtin(output) = added else {
            panic!("pkg add should report its result, got {added:?}");
        };
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("added 'myTools'"),
            "{output:?}"
        );
        session
            .submit_spar("import pkg { dismantler } from \"myTools\";")
            .unwrap();
        let result = session.submit_spar("dismantler()").unwrap();

        assert!(
            matches!(result, ShellResult::Value(spar::ConfigValue::Int(42))),
            "{result:?}"
        );
    }
}
