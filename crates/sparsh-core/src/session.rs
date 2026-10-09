use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::builtin::{BuiltinError, BuiltinOutput, BuiltinRegistry};
use crate::dispatch::{classify, Dispatch};
use spar::repl_split::ReplKind;
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
    #[doc(hidden)]
    ThemeRequest(Vec<String>),
    Builtin(BuiltinOutput),
    /// `help NAME` at a terminal: the UI draws it with colors and tables.
    Help(Box<crate::HelpPage>),
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
    /// A submission that held several statements: one outcome per statement,
    /// in order. A failing statement does not stop the ones after it.
    Sequence(Vec<StatementOutcome>),
}

/// What one statement of a multi-statement submission produced.
#[derive(Debug)]
pub struct StatementOutcome {
    /// The statement as the user typed it.
    pub source: String,
    pub result: Result<ShellResult, ShellError>,
}

impl ShellResult {
    /// The result that decides the submission's exit status and editor
    /// effects: the result itself, or the last successful statement of a
    /// sequence.
    pub fn leaf(&self) -> Option<&ShellResult> {
        match self {
            ShellResult::Sequence(outcomes) => match outcomes.last()?.result.as_ref() {
                Ok(result) => result.leaf(),
                Err(_) => None,
            },
            other => Some(other),
        }
    }
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
    theme_state: crate::ThemeFileState,
    interactive_source: String,
    /// `repo` is open: the buffer redeclares the session, so the editor must not
    /// flag those names as duplicates.
    repo_editing: bool,
    /// The config-only session `repl` checks its buffer against (no interactive declarations).
    repo_base: Option<spar::Session>,
    last_interactive_value: Option<spar::Value>,
    config_notices: Vec<String>,
}

/// Builtins that change shell state are not programs on PATH, so a Spar
/// function body that calls one cannot spawn it. They are queued for the
/// session to run once the function returns. `cd` is handled by Spar itself;
/// the rest are left out because deferring them would change their meaning.
fn register_deferred_builtins() {
    const NOT_DEFERRED: &[&str] = &[
        "cd", "exit", "logout", "source", ".", "exec", "srepl", "reload", "pkg",
    ];
    spar_process::add_deferred_programs(
        BuiltinRegistry::new()
            .metadata()
            .filter(|metadata| metadata.mutates_shell_state)
            .map(|metadata| metadata.name)
            .filter(|name| !NOT_DEFERRED.contains(name))
            .map(str::to_string),
    );
}

impl ShellSession {
    pub fn new() -> Self {
        Self::try_new().expect("failed to initialize Sparsh session services")
    }

    pub fn try_new() -> Result<Self, ShellError> {
        register_deferred_builtins();
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
            theme_state: crate::ThemeFileState::default(),
            interactive_source: String::new(),
            repo_editing: false,
            repo_base: None,
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
        // A `Result` is shown as what it carries: `Ok(x)` as `x`, `Err(e)` as an
        // error with its exit code, never wrapped in `Ok(...)`.
        let result = match result {
            spar::InteractivePreviewResult::RuntimeValue(mut preview)
                if matches!(preview.value, spar::Value::Result(_)) =>
            {
                let spar::Value::Result(inner) = preview.value.clone() else { unreachable!() };
                match inner {
                    Ok(ok) if ok.quiet => spar::InteractivePreviewResult::Empty,
                    Ok(ok) => {
                        preview.value = *ok.value;
                        spar::InteractivePreviewResult::RuntimeValue(preview)
                    }
                    Err(error) => {
                        let message = format!("{}\n", error.value.render_display());
                        eprint!("{message}");
                        self.last_status = error.exit_code;
                        return Ok(ShellResult::Builtin(crate::BuiltinOutput {
                            stdout: Vec::new(),
                            stderr: message.into_bytes(),
                            status: error.exit_code,
                        }));
                    }
                }
            }
            spar::InteractivePreviewResult::Value(spar::ConfigValue::Result(inner)) => match inner {
                Ok(value) => spar::InteractivePreviewResult::Value(*value),
                Err(error) => {
                    let message = format!("{}\n", spar::Value::from_config(*error).render_display());
                    eprint!("{message}");
                    self.last_status = 1;
                    return Ok(ShellResult::Builtin(crate::BuiltinOutput {
                        stdout: Vec::new(),
                        stderr: message.into_bytes(),
                        status: 1,
                    }));
                }
            },
            other => other,
        };
        let result = match result {
            spar::InteractivePreviewResult::Empty => ShellResult::Empty,
            spar::InteractivePreviewResult::Value(value) => self.handle_interactive_value(value)?,
            spar::InteractivePreviewResult::RuntimeValue(value) => ShellResult::Structured(value),
            spar::InteractivePreviewResult::Process(outcome) => ShellResult::Process(outcome),
        };
        self.remember_interactive_value(&result);
        Ok(result)
    }

    /// Check a draft with Spar's compiler and the declarations already in
    /// this shell. External commands are left to the shell parser on submit.
    pub fn editor_diagnostics(&self, source: &str) -> Vec<spar::SparError> {
        if !matches!(classify(source, &self.spar), Dispatch::SparFragment(_))
            || spar::input_completeness(source) == spar::InputCompleteness::Incomplete
        {
            return Vec::new();
        }
        let mut errors = self
            .spar
            .check_interactive_fragment(source, self.last_interactive_value.as_ref())
            .err()
            .unwrap_or_default();
        if self.repo_editing {
            errors.retain(|error| !is_redeclaration(error));
        }
        errors
    }

    /// Runs one statement. `forced` is the kind the statement splitter chose;
    /// `None` falls back to the whole-input classifier.
    fn submit_statement(
        &mut self,
        input: &str,
        forced: Option<ReplKind>,
    ) -> Result<ShellResult, ShellError> {
        self.poll_jobs()?;
        let prepared = if input.contains("<<")
            || (matches!(classify(input, &self.spar), Dispatch::Command(_))
                && input.contains("\\\n"))
        {
            Some(
                crate::shell_input::prepare(input)
                    .map_err(|message| ShellError::Process { message, status: 2 })?,
            )
        } else {
            None
        };
        let input = prepared
            .as_ref()
            .map_or(input, |prepared| prepared.command.as_str());
        let heredoc_input = prepared
            .as_ref()
            .and_then(|prepared| prepared.stdin.clone());
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
            let words: Vec<&str> = input.trim().trim_end_matches(';').split_whitespace().collect();
            if let ["help", name] = words.as_slice() {
                if self.services.aliases.get("help").is_none() {
                    if let Some(page) = crate::help_page(name, &self.builtins) {
                        return self.finish_submission(Ok(ShellResult::Help(Box::new(page))));
                    }
                }
            }
        }
        if self.mode == SessionMode::InteractiveTty {
            if let Some(table) = self.builtin_table(input) {
                let result = match table {
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
        // `z --list` is a table like `ls`; non-interactive runs get the plain text.
        if self.mode == SessionMode::InteractiveTty
            && input.trim().trim_end_matches(';').split_whitespace().collect::<Vec<_>>() == ["z", "--list"]
        {
            if let Some(history) = self.services.history.clone() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs());
                let visits = history.directories().map_err(|message| ShellError::Process { message, status: 1 })?;
                let aliases = history.dir_aliases().map_err(|message| ShellError::Process { message, status: 1 })?;
                let result = match crate::zdir::listing_table(&visits, &aliases, now) {
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
        if self.mode == SessionMode::InteractiveTty && self.allows_native_listing(input) {
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
        if let Some((call, disown)) = crate::function_pipeline::split_background_call(input) {
            let value = self
                .spar
                .eval_transient_with_context(call, &cwd, &environment)
                .map_err(|errors| ShellError::from_spar(errors, call))?;
            let spar::InteractiveEvalResult::Value(spar::ConfigValue::Shell(mut plan)) = value
            else {
                return Err(ShellError::Process {
                    message: "only a function returning shell can run in the background".into(),
                    status: 2,
                });
            };
            crate::function_pipeline::mark_background(&mut plan);
            let result = execute_plan(
                &plan,
                &self.builtins,
                &mut self.services,
                self.last_status,
                self.mode,
            );
            if disown && result.is_ok() {
                self.submit("disown")?;
            }
            return self.finish_submission(result);
        }
        let call = input.trim().trim_end_matches(';').trim_end();
        if crate::dispatch::is_explicit_call(call) {
            let name = call.split_once('(').map_or(call, |(name, _)| name).trim();
            if matches!(self.spar.function_return_type(name),
                Some(spar::ast::SparType::Applied { name, arguments }) if name == "ShellResult" && arguments.len() == 2)
            {
                let result = self.run_shell_result_call(call)?;
                return self.finish_submission(Ok(result));
            }
            // A plain function runs for its effects; echoing what it returns
            // would print values (and secrets) the caller never asked to see.
            // Use `println` to show something. `_` still holds the value.
            if crate::function_pipeline::split_top_level_pipeline(call).len() == 1
                && self.spar.function_return_type(name).is_some_and(|ty| {
                    !matches!(ty, spar::ast::SparType::Shell)
                })
            {
                let result = match self.submit_spar(input.trim())? {
                    ShellResult::Value(_) | ShellResult::Structured(_) => ShellResult::Empty,
                    other => other,
                };
                return self.finish_submission(Ok(result));
            }
        }
        if let Some(composed) = crate::function_pipeline::compose_function_pipeline(
            input,
            &self.spar,
            &cwd,
            &environment,
            self.last_status,
        )? {
            let result = crate::execute::execute_plan_with_captured(
                &composed.plan,
                &self.builtins,
                &mut self.services,
                self.last_status,
                self.mode,
                None,
                composed.captured_outputs,
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

        let mut dispatch = classify(input, &self.spar);
        if matches!(&dispatch, Dispatch::Empty) {
            return Ok(ShellResult::Empty);
        }
        match forced {
            // The splitter decided this is Spar. A bare existing variable
            // still prints its value directly.
            Some(ReplKind::Spar) => {
                let text = input.trim();
                dispatch = if crate::dispatch::is_bare_identifier(text)
                    && self.spar.value(text).is_some()
                {
                    Dispatch::SparValue(text)
                } else {
                    Dispatch::SparFragment(text)
                };
            }
            // A command-shaped statement stays a command unless the old
            // classifier knows better: `env.HOME`, `env["X"]`, `_` access,
            // `|>` pipelines, existing variables, `(await ...)`.
            Some(ReplKind::Command) | None => {}
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
                .eval_shell_plan_with_context(command, &cwd, &environment, Some(self.last_status))
                .map_err(|errors| ShellError::from_spar(errors, command))
                .and_then(|plan| {
                    crate::execute::execute_plan_with_input(
                        &plan,
                        &self.builtins,
                        &mut self.services,
                        self.last_status,
                        self.mode,
                        heredoc_input,
                    )
                }),
        };

        self.finish_submission(result)
    }

    pub fn submit(&mut self, input: &str) -> Result<ShellResult, ShellError> {
        self.submit_streaming(input, &mut |_| {})
    }

    /// Like `submit`, but hands each statement outcome of a multi-statement
    /// submission to `sink` as soon as it completes, so captured command
    /// output appears in order relative to output that Spar blocks stream.
    /// The returned `Sequence` still lists every outcome.
    pub fn submit_streaming(
        &mut self,
        input: &str,
        sink: &mut dyn FnMut(&StatementOutcome),
    ) -> Result<ShellResult, ShellError> {
        let before = self.services.directories.current().to_path_buf();
        let result = self.submit_inner(input, sink, false);
        if self.services.directories.current() != before {
            self.record_current_dir();
        }
        result
    }

    fn submit_inner(
        &mut self,
        input: &str,
        sink: &mut dyn FnMut(&StatementOutcome),
        script: bool,
    ) -> Result<ShellResult, ShellError> {
        self.poll_jobs()?;
        // Heredocs and line continuations span lines on purpose; keep them
        // as one statement.
        if input.trim().is_empty() || input.contains("<<") || input.contains("\\\n") {
            return self.submit_statement(input, None);
        }
        let names = self.split_names(input);
        let statements = spar::repl_split::split_statements(input, &names);
        if statements.is_empty() {
            return self.submit_statement(input, None);
        }

        let mut outcomes = Vec::with_capacity(statements.len());
        let mut cursor = 0;
        let total = statements.len();
        for statement in statements {
            let original = original_text(&statement);
            let start = input[cursor..]
                .find(&original)
                .map_or(cursor, |offset| cursor + offset);
            cursor = (start + original.len()).min(input.len());

            // Each statement is its own fragment, so expressions and
            // declarations need no `;` (and a control block rejects one).
            // A top-level `~ command` marker runs through the command path.
            let (text, kind, start, inserted) = match statement.text.strip_prefix('~') {
                Some(rest)
                    if statement.kind == ReplKind::Spar
                        && rest.starts_with(char::is_whitespace) =>
                {
                    let trimmed = rest.trim_start();
                    let skipped = statement.text.len() - trimmed.len();
                    (
                        trimmed.trim_end_matches(';').to_string(),
                        ReplKind::Command,
                        start + skipped,
                        Vec::new(),
                    )
                }
                _ => (
                    statement.text.clone(),
                    statement.kind,
                    start,
                    statement.inserted.clone(),
                ),
            };
            let result = self
                .submit_statement(&text, Some(kind))
                .map_err(|error| remap_error(error, &text, &inserted, start, input));
            let stop = matches!(
                &result,
                Ok(ShellResult::Exit(_)) | Ok(ShellResult::EditorMode(_))
            );
            let result = if script && total > 1 {
                result.map(|result| script_result(result, &original))
            } else {
                result
            };
            let outcome = StatementOutcome {
                source: original,
                result,
            };
            if total > 1 {
                sink(&outcome);
            }
            outcomes.push(outcome);
            if stop {
                break;
            }
        }
        if total == 1 {
            return outcomes.pop().expect("one statement").result;
        }
        Ok(ShellResult::Sequence(outcomes))
    }

    /// Names the splitter treats as Spar: the user's declarations plus any
    /// existing value used as `name.field` / `name[index]` at a statement
    /// start (`env.HOME`), without hijacking a bare command of that name.
    fn split_names(&self, input: &str) -> std::collections::HashSet<String> {
        let mut names = self.spar.scope_names();
        for piece in input.split([';', '\n']) {
            let piece = piece.trim_start();
            if let Some(index) = piece.find(['.', '[']) {
                let head = &piece[..index];
                if crate::dispatch::is_bare_identifier(head) && self.spar.value(head).is_some() {
                    names.insert(head.to_string());
                }
            }
        }
        names
    }

    /// Submission for scripts (`-c`, piped stdin). A string returned by a Spar
    /// expression is data, so it is written as-is; a bare variable name still
    /// shows the quoted, Spar-literal form, as does the prompt.
    pub fn submit_script(&mut self, input: &str) -> Result<ShellResult, ShellError> {
        let result = self.submit(input)?;
        Ok(script_result(result, input))
    }

    /// `submit_script` with per-statement streaming; see `submit_streaming`.
    pub fn submit_script_streaming(
        &mut self,
        input: &str,
        sink: &mut dyn FnMut(&StatementOutcome),
    ) -> Result<ShellResult, ShellError> {
        let result = self.submit_inner(input, sink, true)?;
        Ok(match result {
            // Already converted per statement as each one completed.
            sequence @ ShellResult::Sequence(_) => sequence,
            other => script_result(other, input),
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
            Ok(ShellResult::ThemeRequest(args)) => {
                let output = self.run_theme_command(&args)?;
                self.last_status = output.status;
                Ok(ShellResult::Builtin(output))
            },
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
                    | ShellResult::Help(_)
                    | ShellResult::EditorMode(_)
                    | ShellResult::ReloadConfig
                    | ShellResult::ReloadConfigWith(_)
                    | ShellResult::ExecRequest { .. }
                    | ShellResult::SourceRequest(_)
                    | ShellResult::ThemeRequest(_) => 0,
                    ShellResult::Builtin(output) => output.status,
                    ShellResult::Process(outcome) => outcome.exit_code,
                    ShellResult::BackgroundJob { .. } => 0,
                    ShellResult::CommandStatus { status, .. } => *status,
                    ShellResult::Exit(status) => *status,
                    ShellResult::Sequence(_) => self.last_status,
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
        self.record_current_dir();
    }

    fn record_current_dir(&self) {
        if let Some(history) = &self.services.history {
            // History is best effort; a failed write must not break the prompt.
            let _ = history.record_dir(self.services.directories.current());
        }
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
                spar::ConfigValue::Object(_) | spar::ConfigValue::List(_)
            )
        };
        let structured = self.mode == SessionMode::InteractiveTty
            && match &value {
                spar::ConfigValue::Object(fields) => !fields.is_empty(),
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

    /// True unless the user has overridden `ls`/`ll` with their own alias,
    /// from config or the `alias` builtin, rather than Sparsh's own
    /// `--color=auto`/`-la` fallbacks (or no alias at all). A user alias
    /// always wins over the native table. Anything other than a bare `ls` or
    /// `ll` invocation (pipes, other commands, ...) is left to fall through.
    fn allows_native_listing(&self, input: &str) -> bool {
        // `_ |> ...` re-pipelines the last listing rather than naming `ls`/`ll`
        // directly; its gate has always tracked the `ls` alias.
        let (name, builtin_default) = match input.split_whitespace().next() {
            Some("ls") | Some("_") => ("ls", crate::alias::BUILTIN_LS_ALIAS),
            Some("ll") => ("ll", crate::alias::BUILTIN_LL_ALIAS),
            _ => return false,
        };
        !self
            .services
            .aliases
            .get(name)
            .is_some_and(|expansion| expansion != builtin_default)
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
        let interactive = self.interactive_source.clone();
        self.rebuild_spar(&interactive, true)?;
        if let Some(notice) = self.refresh_theme(true).notice {
            return Err(ShellError::Config(crate::ConfigLoadError::Invalid(format!("theme: {notice}"))));
        }
        Ok(())
    }

    /// The declarations typed or sourced in this session (imports, variables,
    /// structs, functions), formatted, as `repo` shows them.
    /// The first compile problems in a `repl` buffer, checked as a whole file.
    pub fn repo_diagnostics(&self, source: &str) -> Vec<spar::SparError> {
        if source.trim().is_empty() {
            return Vec::new();
        }
        // Against the config-only base, the buffer's own declarations cannot
        // collide with themselves; without it, drop the redeclaration messages.
        let checker = self.repo_base.as_ref().unwrap_or(&self.spar);
        let mut errors = checker.check_interactive_fragment(source, None).err().unwrap_or_default();
        if self.repo_base.is_none() {
            errors.retain(|error| !is_redeclaration(error));
        }
        errors
    }

    /// Turns private (stealth) history mode on or off.
    pub fn set_stealth_mode(&self, enabled: bool) {
        if let Some(history) = &self.services.history {
            let _ = history.set_stealth_mode(enabled);
        }
    }

    /// The declarations typed or sourced so far, exactly as stored; pass it to
    /// `apply_repo_source` later to put the session back.
    pub fn declarations_snapshot(&self) -> String {
        self.interactive_source.clone()
    }

    pub fn set_repo_editing(&mut self, editing: bool) {
        self.repo_editing = editing;
        self.repo_base = if editing {
            // A failure here only means diagnostics fall back to filtering.
            self.base_session().ok().map(|(session, _)| session)
        } else {
            None
        };
    }

    pub fn repo_source(&self) -> String {
        let source = self.interactive_source.trim();
        if source.is_empty() {
            return String::new();
        }
        spar::formatter::format_source(source).unwrap_or_else(|_| format!("{source}\n"))
    }

    /// Replaces the session's declarations with `source` if the whole thing
    /// compiles together with the config; otherwise nothing changes and the
    /// compiler's diagnostics come back. Returns the names (added, removed).
    pub fn apply_repo_source(&mut self, source: &str) -> Result<(Vec<String>, Vec<String>), ShellError> {
        let names = |session: &Self| -> std::collections::BTreeSet<String> {
            session
                .spar
                .identifiers()
                .filter(|name| !name.starts_with("Sparsh") && !is_internal_name(name))
                .map(str::to_string)
                .collect()
        };
        let before = names(self);
        self.rebuild_spar(source, false)?;
        self.interactive_source = source.trim().to_string();
        let after = names(self);
        Ok((
            after.difference(&before).cloned().collect(),
            before.difference(&after).cloned().collect(),
        ))
    }

    /// A fresh session with only the config evaluated: what the declarations in
    /// `repl` are compiled against.
    fn base_session(&mut self) -> Result<(spar::Session, crate::SparshConfig), ShellError> {
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
        Ok((candidate, config))
    }

    fn rebuild_spar(&mut self, interactive_source: &str, apply_config: bool) -> Result<(), ShellError> {
        let (mut candidate, config) = self.base_session()?;
        if !interactive_source.trim().is_empty() {
            candidate.eval(interactive_source).map_err(ShellError::Spar)?;
        }
        if apply_config {
            self.services
                .apply_config(&config)
                .map_err(|message| ShellError::Config(crate::ConfigLoadError::Invalid(message)))?;
        }
        // The replacement session must keep the terminal features, or a
        // config reload at startup silently turns them off.
        if self.mode == SessionMode::InteractiveTty {
            configure_terminal_session(&mut candidate);
        }
        self.spar = candidate;
        if apply_config {
            self.config = config;
            self.config_generation = self.config_generation.wrapping_add(1);
        }
        Ok(())
    }

    /// Calls a `ShellResult` function through the compiled runtime. The runtime
    /// keeps `err` exit codes and `ok(quiet:)`, and runs structured mixed
    /// pipelines (`cmd | from fmt`) the tree evaluator cannot. A pipeline that
    /// ends the body on a structured terminal is shown as a table.
    /// Runs, in order, the sparsh builtins that a Spar function body called
    /// while it was evaluating. Spar cannot reach this session's state, so
    /// `spar-process` queues them instead of spawning a program.
    fn run_deferred_builtins(&mut self) -> Result<(), ShellError> {
        for command in spar_process::take_deferred_commands() {
            let line = std::iter::once(command.program.as_str())
                .chain(command.args.iter().map(String::as_str))
                .map(crate::execute::shell_quote)
                .collect::<Vec<_>>()
                .join(" ");
            self.submit(&line)?;
        }
        Ok(())
    }

    fn run_shell_result_call(&mut self, call: &str) -> Result<ShellResult, ShellError> {
        let cwd = self.services.directories.current().to_path_buf();
        let environment = self.services.environment.snapshot();
        let evaluated = self
            .spar
            .eval_call_runtime_with_context(call, &cwd, &environment)
            .map_err(|errors| ShellError::from_spar(errors, call));
        // Builtins the function called (`path append`, `export`, ...) ran as
        // no-ops inside Spar; apply them now, in order, in this session.
        let deferred = self.run_deferred_builtins();
        let (value, capture) = evaluated?;
        deferred?;
        // `ok(pretty: true)`: show the value as a table you can query with `_`.
        if let spar::Value::Result(Ok(ok)) = &value {
            if ok.pretty && !ok.quiet {
                return Ok(ShellResult::Structured(spar::InteractiveRuntimeValue {
                    value: (*ok.value).clone(),
                    stream_preview: false,
                    truncated: false,
                    presentation: spar::InteractivePresentation::Pipeline,
                }));
            }
        }
        let output = crate::function_pipeline::shell_result_runtime_output(value)?;
        if !output.stderr.is_empty() {
            use std::io::Write;
            std::io::stderr().lock().write_all(&output.stderr).map_err(|error| ShellError::Process { message: error.to_string(), status: 1 })?;
        }
        if let (Some(capture), 0) = (capture, output.status) {
            return Ok(ShellResult::Structured(capture));
        }
        Ok(ShellResult::Builtin(crate::BuiltinOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            status: output.status,
        }))
    }

    /// The table for a plain `dirs`, `alias`, `jobs`, `history`, `path`, `hash`,
    /// `export` or `help` typed at a terminal; `None` for anything else (an
    /// argument that changes state, pipes, redirects, or a user alias that
    /// shadows the name), which runs the ordinary text builtin.
    fn builtin_table(&mut self, input: &str) -> Option<Result<spar::Value, String>> {
        use crate::builtin_tables::{number, optional_text, table, text, when, Row};
        let line = input.trim().trim_end_matches(';').trim();
        if line.split_whitespace().any(crate::listing::word_has_shell_syntax) {
            return None;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let (name, args) = words.split_first()?;
        if self.services.aliases.get(name).is_some() {
            return None;
        }
        let rows: (Vec<Row>, &[&str]) = match (*name, args) {
            ("dirs", []) => (
                self.services
                    .directories
                    .all()
                    .into_iter()
                    .enumerate()
                    .map(|(index, path)| {
                        vec![
                            ("path", text(path.to_string_lossy())),
                            ("role", text(if index == 0 { "current" } else { "stack" })),
                        ]
                    })
                    .collect(),
                &["path", "role"],
            ),
            ("alias", []) => (
                self.services
                    .aliases
                    .iter()
                    .map(|(alias, words)| vec![("name", text(alias)), ("command", text(words.join(" ")))])
                    .collect(),
                &["name", "command"],
            ),
            ("jobs", []) => {
                self.services.jobs.poll().ok()?;
                (
                    self.services
                        .jobs
                        .snapshots()
                        .into_iter()
                        .map(|job| {
                            let state = match job.state {
                                crate::job::JobState::Running => "Running".to_string(),
                                crate::job::JobState::Stopped => "Stopped".to_string(),
                                crate::job::JobState::Done(code) => format!("Done({code})"),
                            };
                            vec![("id", number(job.id.0 as usize)), ("state", text(state)), ("command", text(job.command_text))]
                        })
                        .collect(),
                    &["id", "state", "command"],
                )
            }
            ("history", rest) => {
                let history = self.services.history.clone()?;
                let records = match rest {
                    [] => history.records(None),
                    [count] => history.records(Some(count.parse::<usize>().ok()?)),
                    ["--search", query] => history
                        .records(None)
                        .map(|all| all.into_iter().filter(|record| record.command.contains(query)).collect()),
                    _ => return None,
                };
                (
                    match records {
                        Ok(records) => records
                            .into_iter()
                            .map(|record| {
                                vec![
                                    ("line", number(record.line)),
                                    ("when", when(record.time)),
                                    ("command", text(record.command)),
                                    ("directory", optional_text(&record.directory)),
                                    ("kind", text(record.kind)),
                                ]
                            })
                            .collect(),
                        Err(message) => return Some(Err(message)),
                    },
                    &["line", "when", "command", "directory", "kind"],
                )
            }
            ("path", []) => (
                self.services
                    .path
                    .directories()
                    .iter()
                    .map(|directory| {
                        vec![
                            ("directory", text(directory.to_string_lossy())),
                            ("exists", spar::Value::Bool(directory.is_dir())),
                        ]
                    })
                    .collect(),
                &["directory", "exists"],
            ),
            ("theme", []) => {
                self.refresh_theme(false);
                (
                    crate::theme_command::status_rows(&self.theme_state, &self.config)
                        .into_iter()
                        .map(|(property, value)| vec![("property", text(property)), ("value", text(value))])
                        .collect(),
                    &["property", "value"],
                )
            }
            ("theme", ["list"]) => {
                self.refresh_theme(false);
                (
                    crate::theme_command::list_rows(&self.theme_state, &self.config)
                        .into_iter()
                        .map(|(name, description, active)| {
                            vec![
                                ("name", text(name)),
                                ("description", description.map_or(spar::Value::Void, text)),
                                ("active", spar::Value::Bool(active)),
                            ]
                        })
                        .collect(),
                    &["name", "description", "active"],
                )
            }
            ("hash", []) => (
                self.services
                    .resolver
                    .entries()
                    .map(|(command, path)| vec![("command", text(command)), ("path", text(path.to_string_lossy()))])
                    .collect(),
                &["command", "path"],
            ),
            ("export", []) => {
                let mut variables: Vec<_> = self
                    .services
                    .environment
                    .snapshot()
                    .into_iter()
                    .map(|(name, value)| (name.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
                    .collect();
                variables.sort();
                (
                    variables.into_iter().map(|(name, value)| vec![("name", text(name)), ("value", text(value))]).collect(),
                    &["name", "value"],
                )
            }
            ("help", []) => (
                self.builtins
                    .metadata()
                    .map(|metadata| {
                        vec![
                            ("category", text(metadata.category)),
                            ("name", text(metadata.name)),
                            ("description", text(metadata.description)),
                            ("usage", text(metadata.usage)),
                        ]
                    })
                    .collect(),
                &["category", "name", "description", "usage"],
            ),
            _ => return None,
        };
        Some(table(rows.0, rows.1))
    }

    pub fn run_startup_hook(&mut self) -> Result<ShellResult, ShellError> {
        if !self.spar.has_function("startup") {
            return Ok(ShellResult::Empty);
        }
        let cwd = self.services.directories.current().to_path_buf();
        let environment = self.services.environment.snapshot();
        if matches!(self.spar.function_return_type("startup"),
            Some(spar::ast::SparType::Applied { name, arguments }) if name == "ShellResult" && arguments.len() == 2)
        {
            return self.run_shell_result_call("startup()");
        }
        // A `__shell` startup returns a plan the session must run (e.g. `cd`).
        match self
            .spar
            .eval_transient_with_context("startup()", &cwd, &environment)
            .map_err(ShellError::Spar)?
        {
            spar::InteractiveEvalResult::Value(spar::ConfigValue::Shell(plan)) => {
                self.handle_interactive_value(spar::ConfigValue::Shell(plan))
            }
            _ => Ok(ShellResult::Empty),
        }
    }

    pub fn stealth_mode(&self) -> bool {
        self.services
            .history
            .as_ref()
            .and_then(|history| history.stealth_mode().ok())
            .unwrap_or(false)
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

    pub fn refresh_theme(&mut self, force: bool) -> crate::ThemeRefresh {
        let Some(home) = self.services.environment.get("HOME").map(std::path::PathBuf::from) else {
            return crate::ThemeRefresh::default();
        };
        self.theme_state.refresh(&home, force)
    }

    pub fn run_theme_command(&mut self, args: &[String]) -> Result<BuiltinOutput, ShellError> {
        let home = self.services.environment.get("HOME").map(PathBuf::from);
        crate::theme_command::run(args, &mut self.theme_state, &self.config, home.as_deref())
    }

    pub fn theme_generation(&self) -> u64 {
        self.theme_state.generation()
    }

    pub fn theme_file_layer(&self) -> &crate::ThemeLayer {
        self.theme_state.layer()
    }

    pub fn config_theme_layer(&self) -> &crate::ThemeLayer {
        &self.config.theme
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
            .filter(|name| !name.starts_with("Sparsh") && !is_internal_name(name))
            .map(str::to_string)
            .collect();
        let spar_functions = self
            .spar
            .function_names()
            .filter(|name| !name.starts_with("Sparsh") && !is_internal_name(name))
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
        let async_functions: BTreeSet<String> = self
            .spar
            .function_names()
            .filter(|name| {
                self.spar.function_is_async(name)
                    && !name.starts_with("Sparsh")
                    && !is_internal_name(name)
            })
            .map(str::to_string)
            .collect();
        let dir_aliases = self
            .services
            .history
            .as_ref()
            .and_then(|history| history.dir_aliases().ok())
            .unwrap_or_default();
        let visited_dirs = self
            .services
            .history
            .as_ref()
            .and_then(|history| history.directories().ok())
            .unwrap_or_default();
        crate::CompletionSnapshot {
            visited_dirs,
            dir_aliases,
            async_functions,
            cwd,
            home: self.services.environment.get("HOME").map(PathBuf::from),
            builtins,
            aliases,
            executables,
            spar_identifiers,
            spar_functions,
            session_source: self.spar.committed_source().to_string(),
            theme_names: self
                .config
                .themes
                .iter()
                .map(|theme| (theme.name.clone(), theme.description.clone()))
                .collect(),
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
            builtins: self
                .builtins
                .names()
                .into_iter()
                .collect(),
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

/// Names the compiler invents when it isolates an imported module
/// (`SparModule<hash>Name`); they are never the user's to see or pick.
fn is_internal_name(name: &str) -> bool {
    spar::naming::demangle(name) != name
}

/// A message about a name that is already declared. While `repl` is open the
/// buffer redeclares the whole session, so these are expected until save.
fn is_redeclaration(error: &spar::SparError) -> bool {
    let text = error.to_string();
    text.contains("duplicate declaration")
        || text.contains("collides with a declaration")
        || text.contains("is already defined")
        || text.contains("already declared")
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
            .define(
                crate::alias::BUILTIN_LS_ALIAS[0],
                crate::alias::BUILTIN_LS_ALIAS
                    .iter()
                    .map(|word| (*word).to_string())
                    .collect(),
            )
            .expect("the built-in ls alias is valid");
    }
}

/// A string returned by a Spar expression in a script is data, so it is
/// written as-is; a bare variable name still shows the quoted form.
fn script_result(result: ShellResult, source: &str) -> ShellResult {
    match result {
        ShellResult::Value(spar::ConfigValue::Str(text))
            if !crate::dispatch::is_bare_identifier(source.trim()) =>
        {
            ShellResult::Builtin(crate::BuiltinOutput {
                stdout: format!("{text}\n").into_bytes(),
                stderr: Vec::new(),
                status: 0,
            })
        }
        ShellResult::Sequence(outcomes) => ShellResult::Sequence(
            outcomes
                .into_iter()
                .map(|outcome| StatementOutcome {
                    result: outcome
                        .result
                        .map(|result| script_result(result, &outcome.source)),
                    source: outcome.source,
                })
                .collect(),
        ),
        other => other,
    }
}

/// The statement as the user typed it: its rewritten text without the
/// inserted `~ ` markers and `;` terminators.
fn original_text(statement: &spar::repl_split::ReplStatement) -> String {
    let mut out = String::with_capacity(statement.text.len());
    for (index, ch) in statement.text.char_indices() {
        if !statement
            .inserted
            .iter()
            .any(|&(start, end)| index >= start && index < end)
        {
            out.push(ch);
        }
    }
    out
}

/// Maps a byte offset in rewritten statement text back to the submission the
/// user typed: drop the inserted bytes before it, add the statement's start.
fn map_offset(offset: usize, inserted: &[(usize, usize)], start: usize, limit: usize) -> usize {
    let removed: usize = inserted
        .iter()
        .map(|&(a, b)| offset.min(b).saturating_sub(a.min(offset)))
        .sum();
    (start + offset - removed.min(offset)).min(limit)
}

/// Re-points Spar error spans from the text handed to Spar (rewritten, or a
/// trimmed slice of it) to the user's typed submission, and renders against
/// that submission.
fn remap_error(
    error: ShellError,
    passed: &str,
    inserted: &[(usize, usize)],
    start: usize,
    submission: &str,
) -> ShellError {
    let ShellError::SparSource { mut errors, source, filename } = error else {
        return error;
    };
    let Some(base) = passed.find(&source) else {
        return ShellError::SparSource { errors, source, filename };
    };
    for error in &mut errors {
        let span = error.span_mut();
        if span.file != 0 {
            continue;
        }
        let first = map_offset(base + span.start, inserted, start, submission.len());
        let last = map_offset(base + span.end, inserted, start, submission.len()).max(first);
        span.start = first;
        span.end = last;
        let before = &submission[..first];
        span.line = before.matches('\n').count() as u32 + 1;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        span.col = submission[line_start..first].chars().count() as u32 + 1;
    }
    ShellError::SparSource {
        errors,
        source: submission.to_string(),
        filename,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use spar::ConfigValue;

    use super::{
        install_color_ls_alias, CommandDiagnostic, CommandKind, ShellResult, ShellSession,
    };
    use crate::alias::AliasService;
    use crate::PROCESS_STATE;

    #[test]
    fn session_refreshes_theme_without_touching_config_generation() {
        let home = tempfile::tempdir().unwrap();
        let source_dir = home.path().join(".sparsh/src");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("sparsh-types.spar"), include_str!("../../../examples/sparsh-types.spar")).unwrap();
        std::fs::write(source_dir.join("theme.generated.spar"), r##"var themeState: Record = { source: { kind: "external"; }; theme: { palette: { accent: "#112233"; }; }; };"##).unwrap();
        let mut session = ShellSession::new();
        session.services.environment = crate::environment::EnvironmentService::from_pairs([
            ("HOME".to_string(), home.path().display().to_string()),
        ]);
        let config_generation = session.config_generation();
        assert!(session.refresh_theme(false).changed);
        assert_eq!(session.theme_generation(), 1);
        assert_eq!(session.config_generation(), config_generation);
        assert_eq!(session.theme_file_layer().palette[&crate::PaletteKey::Accent], crate::ColorSpec::Rgb(0x11, 0x22, 0x33));
        assert!(session.config_theme_layer().palette.is_empty());
    }

    #[test]
    fn reload_forces_an_invalid_theme_notice() {
        let home = tempfile::tempdir().unwrap();
        let source_dir = home.path().join(".sparsh/src");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("sparsh-types.spar"), include_str!("../../../examples/sparsh-types.spar")).unwrap();
        std::fs::write(source_dir.join("theme.generated.spar"), "struct Theme {").unwrap();
        let mut session = ShellSession::new();
        session.services.environment = crate::environment::EnvironmentService::from_pairs([
            ("HOME".to_string(), home.path().display().to_string()),
        ]);
        assert!(session.refresh_theme(false).notice.is_some());
        assert!(session.refresh_theme(false).notice.is_none());
        let error = session.reload_config().unwrap_err().to_string();
        assert!(error.contains("theme.generated.spar"), "{error}");
    }

    #[derive(Default)]
    struct MemoryHistory {
        entries: Mutex<Vec<String>>,
        stealth: AtomicBool,
        dirs: Mutex<Vec<crate::DirVisit>>,
        aliases: Mutex<Vec<crate::DirAlias>>,
    }

    impl crate::HistoryAccess for MemoryHistory {
        fn stealth_mode(&self) -> Result<bool, String> {
            Ok(self.stealth.load(Ordering::Relaxed))
        }

        fn set_stealth_mode(&self, enabled: bool) -> Result<(), String> {
            self.stealth.store(enabled, Ordering::Relaxed);
            Ok(())
        }

        fn records(&self, limit: Option<usize>) -> Result<Vec<crate::HistoryRecord>, String> {
            Ok(self
                .list(limit)?
                .into_iter()
                .map(|(line, command)| crate::HistoryRecord {
                    line,
                    time: 0,
                    command,
                    directory: String::new(),
                    kind: "command".into(),
                })
                .collect())
        }

        fn list(&self, limit: Option<usize>) -> Result<Vec<(usize, String)>, String> {
            let entries = self.entries.lock().map_err(|_| "history lock poisoned")?;
            let start = limit
                .map(|limit| entries.len().saturating_sub(limit))
                .unwrap_or(0);
            Ok(entries
                .iter()
                .enumerate()
                .skip(start)
                .map(|(index, entry)| (index + 1, entry.clone()))
                .collect())
        }

        fn delete_line(&self, line: usize) -> Result<usize, String> {
            let mut entries = self.entries.lock().map_err(|_| "history lock poisoned")?;
            if line == 0 || line > entries.len() {
                return Ok(0);
            }
            entries.remove(line - 1);
            Ok(1)
        }

        fn delete_matching(&self, text: &str, exact: bool) -> Result<usize, String> {
            let mut entries = self.entries.lock().map_err(|_| "history lock poisoned")?;
            let before = entries.len();
            entries.retain(|entry| {
                if exact {
                    entry != text
                } else {
                    !entry.contains(text)
                }
            });
            Ok(before - entries.len())
        }

        fn clear(&self) -> Result<(), String> {
            self.entries
                .lock()
                .map_err(|_| "history lock poisoned".to_string())?
                .clear();
            Ok(())
        }

        fn record_dir(&self, path: &Path) -> Result<(), String> {
            let mut dirs = self.dirs.lock().map_err(|_| "history lock poisoned")?;
            match dirs.iter_mut().find(|visit| visit.path == path) {
                Some(visit) => visit.count += 1,
                None => dirs.push(crate::DirVisit { path: path.to_path_buf(), count: 1, last: 0 }),
            }
            Ok(())
        }

        fn directories(&self) -> Result<Vec<crate::DirVisit>, String> {
            Ok(self.dirs.lock().map_err(|_| "history lock poisoned")?.clone())
        }

        fn dir_aliases(&self) -> Result<Vec<crate::DirAlias>, String> {
            Ok(self.aliases.lock().map_err(|_| "history lock poisoned")?.clone())
        }

        fn set_dir_alias(&self, name: &str, path: &Path) -> Result<(), String> {
            let mut aliases = self.aliases.lock().map_err(|_| "history lock poisoned")?;
            aliases.retain(|(existing, _)| existing != name);
            aliases.push((name.to_string(), path.to_path_buf()));
            Ok(())
        }

        fn remove_dir_alias(&self, name: &str) -> Result<bool, String> {
            let mut aliases = self.aliases.lock().map_err(|_| "history lock poisoned")?;
            let before = aliases.len();
            aliases.retain(|(existing, _)| existing != name);
            Ok(aliases.len() != before)
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
            "function startup() -> __shell {{ return __shell {{ cd {}; }}; }};",
            directory.path().display()
        );

        session.submit_spar(&source).unwrap();
        session.run_startup_hook().unwrap();

        assert_eq!(session.ui_snapshot().cwd(), directory.path());
    }

    #[test]
    fn startup_runs_state_changing_builtins_after_the_function_returns() {
        let mut session = ShellSession::new();
        session
            .submit_spar(
                r#"function startup() -> ShellResult<int, int> {
    path append "/tmp/sparsh-deferred-test-dir";
    export SPARSH_DEFERRED_TEST=yes;
    return ok(value: 0, quiet: true);
};"#,
            )
            .unwrap();
        session.run_startup_hook().unwrap();
        let ShellResult::Builtin(path) = session.submit("path").unwrap() else {
            panic!("expected path output");
        };
        assert!(
            String::from_utf8_lossy(&path.stdout).contains("/tmp/sparsh-deferred-test-dir"),
            "{}",
            String::from_utf8_lossy(&path.stdout)
        );
        assert_eq!(
            session.services.environment.get("SPARSH_DEFERRED_TEST"),
            Some(std::ffi::OsStr::new("yes"))
        );
    }

    #[test]
    fn a_failing_deferred_builtin_reports_its_error() {
        let mut session = ShellSession::new();
        session
            .submit_spar(
                r#"function startup() -> ShellResult<int, int> {
    path frobnicate;
    return ok(value: 0, quiet: true);
};"#,
            )
            .unwrap();
        let error = session.run_startup_hook().unwrap_err().to_string();
        assert!(error.contains("path"), "{error}");
    }

    #[test]
    fn interactive_commands_survive_invalid_config_and_language_input() {
        let home = tempfile::tempdir().unwrap();
        let rc = home.path().join(".sparsh");
        std::fs::create_dir_all(&rc).unwrap();
        std::fs::write(rc.join("sparsh.spar"), "this is invalid config {").unwrap();
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .services
            .environment
            .set_os("HOME", home.path().as_os_str());
        assert!(session.reload_config().is_err());
        assert!(session.submit("var broken: int = ;").is_err());
        let marker = home.path().join("command-output");
        session
            .submit(&format!("printf recovered > {}", marker.display()))
            .unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "recovered");
        session.submit("false").unwrap();
        assert_eq!(session.last_status(), 1);
        session.submit("true").unwrap();
        assert_eq!(session.last_status(), 0);
        session.submit("exit").unwrap();
        assert!(session.should_exit());
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
    fn dollar_question_reports_the_previous_command_status() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("status.txt");
        let mut session = ShellSession::new();
        session.submit("false").unwrap();
        assert_eq!(session.last_status(), 1);

        let result = session
            .submit(&format!("printf '%s' $? | cat > {}", output.display()))
            .unwrap();
        assert!(
            matches!(
                result,
                ShellResult::Process(spar::ShellPlanOutcome { success: true, .. })
            ),
            "{result:?}"
        );
        assert_eq!(std::fs::read_to_string(output).unwrap(), "1");
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
                "{}\nstruct Config {{\n    aliases: Option<List<SparshAlias>> = some(value: [SparshAlias(name: \"gs\", command: [\"git\", \"status\"])]);\n    prompt: Option<SparshPrompt> = some(value: SparshPrompt(right: some(value: SparshPromptRight(slot1: some(value: SparshPromptSlot(text: \"{{cpu}}\")), slot2: some(value: SparshPromptSlot(text: \"{{cpuu}}\"))))));\n}};\n",
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
                "{}\nstruct Config {{\n    keybindings: Option<List<SparshKeybinding>> = some(value: [SparshKeybinding(key: \"ctrl+l\", action: \"clearScreen\")]);\n}};\n",
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
    fn editor_diagnostics_use_committed_declarations_without_running_draft() {
        let mut session = ShellSession::new();
        session.submit("var base: int = 4;").unwrap();
        assert!(session.editor_diagnostics("base + 1").is_empty());
        let errors = session.editor_diagnostics("var result: int = missing; ");
        assert!(!errors.is_empty());
        assert!(matches!(
            session.submit("base").unwrap(),
            ShellResult::Value(spar::ConfigValue::Int(4))
        ));
        assert!(session.editor_diagnostics("echo hello").is_empty());
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
    fn chained_builtins_keep_every_output() {
        for (input, expected) in [
            ("echo a; echo b", "a\nb\n"),
            ("echo a && echo b", "a\nb\n"),
            ("echo a && echo b && echo c", "a\nb\nc\n"),
            ("false || echo b", "b\n"),
            ("echo a || echo b", "a\n"),
            ("echo a; cd .; echo c", "a\nc\n"),
        ] {
            let mut session = ShellSession::new();
            let output = builtin_stdout(session.submit(input).unwrap());
            assert_eq!(output, expected, "input: {input}");
        }
    }

    #[test]
    fn ls_in_a_chain_is_the_native_listing() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("zdir")).unwrap();
        std::fs::write(directory.path().join("a.txt"), "x").unwrap();
        let dir = directory.path().display();
        for input in [
            format!("cd {dir}; ls && echo done"),
            format!("cd {dir}; ls --color=auto; echo done"),
            format!("cd {dir}; ls -la && echo done"),
        ] {
            let mut session = ShellSession::new();
            let output = builtin_stdout(session.submit(&input).unwrap());
            assert!(output.contains("a.txt"), "input: {input}\n{output}");
            assert!(output.contains("zdir"), "input: {input}\n{output}");
            assert!(output.ends_with("done\n"), "input: {input}\n{output}");
            // directories sort first, like the native listing
            assert!(
                output.find("zdir") < output.find("a.txt"),
                "input: {input}\n{output}"
            );
        }
    }

    #[test]
    fn ls_with_unsupported_flags_still_reaches_the_binary() {
        let mut session = ShellSession::new();
        let result = session.submit("ls --definitely-not-a-flag").unwrap();
        assert!(!matches!(result, ShellResult::Builtin(_)));
    }

    #[test]
    fn failed_builtin_does_not_abort_the_chain() {
        let missing = "/definitely/not/a/dir/zzz";
        for (input, expected) in [
            (format!("cd {missing} || echo recovered"), "recovered\n"),
            (format!("cd {missing}; echo after"), "after\n"),
            (format!("cd {missing} && echo no; echo after"), "after\n"),
        ] {
            let mut session = ShellSession::new();
            let output = builtin_stdout(session.submit(&input).unwrap());
            assert_eq!(output, expected, "input: {input}");
        }
        // A builtin error in last position is still reported as an error.
        let mut session = ShellSession::new();
        assert!(session.submit(&format!("cd {missing}")).is_err());
        assert!(session.submit(&format!("echo a; cd {missing}")).is_err());
    }

    #[test]
    fn unquoted_globs_expand_against_the_session_directory() {
        let directory = tempfile::tempdir().unwrap();
        for file in ["a.txt", "b.txt", "c.md", ".hidden.txt"] {
            std::fs::write(directory.path().join(file), "").unwrap();
        }
        let dir = directory.path().display();
        for (input, expected) in [
            (format!("cd {dir}; echo *.txt"), "a.txt b.txt\n"),
            (format!("cd {dir}; echo ?.md"), "c.md\n"),
            (format!("cd {dir}; echo [ab].txt"), "a.txt b.txt\n"),
            (format!("cd {dir}; echo .*.txt"), ".hidden.txt\n"),
            // quoted, escaped and unmatched patterns stay literal
            (format!("cd {dir}; echo \"*.txt\""), "*.txt\n"),
            (format!("cd {dir}; echo '*.txt'"), "*.txt\n"),
            (format!("cd {dir}; echo \\*.txt"), "*.txt\n"),
            (format!("cd {dir}; echo *.zzz"), "*.zzz\n"),
            // expansion happens for each command in a chain
            (format!("cd {dir}; echo *.md && echo *.txt"), "c.md\na.txt b.txt\n"),
        ] {
            let mut session = ShellSession::new();
            let output = builtin_stdout(session.submit(&input).unwrap());
            assert_eq!(output, expected, "input: {input}");
        }
    }

    #[test]
    fn errors_in_imported_functions_name_the_imported_file_and_trace() {
        let directory = tempfile::tempdir().unwrap();
        let lib = directory.path().join("lib.spar");
        std::fs::write(&lib, "fn boom(x: int) -> int {\n    return 10 / x;\n};\n").unwrap();
        let mut session = ShellSession::new();
        session
            .submit(&format!("import {{ boom }} from \"{}\";", lib.display()))
            .unwrap();
        let error = session.submit("boom(x: 0)").unwrap_err();
        let errors = match error {
            super::ShellError::Spar(errors) => errors,
            super::ShellError::SparSource { errors, .. } => errors,
            other => panic!("expected a Spar error, got {other:?}"),
        };
        let text = spar::ErrorRenderer::new("boom(x: 0)", "<sparsh>").render_all(&errors);
        assert!(text.contains("lib.spar:2:"), "{text}");
        assert!(text.contains("return 10 / x;"), "{text}");
        assert!(text.contains("trace (most recent call first):"), "{text}");
        assert!(text.contains("in boom"), "{text}");
        assert!(text.contains("called from top level"), "{text}");
        assert!(text.contains("<sparsh>:1:"), "{text}");
    }

    #[test]
    fn missing_program_in_an_imported_function_names_the_program_and_its_line() {
        let directory = tempfile::tempdir().unwrap();
        let lib = directory.path().join("lib.spar");
        std::fs::write(
            &lib,
            "fn badcmd() -> ShellResult<str, str> {\n    nonexistent_cmd_zzz --flag;\n    return ok(value: \"x\");\n};\n",
        )
        .unwrap();
        let mut session = ShellSession::new();
        session
            .submit(&format!("import {{ badcmd }} from \"{}\";", lib.display()))
            .unwrap();
        let error = session.submit("badcmd()").unwrap_err();
        let errors = match error {
            super::ShellError::Spar(errors) => errors,
            super::ShellError::SparSource { errors, .. } => errors,
            other => panic!("expected a Spar error, got {other:?}"),
        };
        let text = spar::ErrorRenderer::new("badcmd()", "<sparsh>").render_all(&errors);
        assert!(
            text.contains("could not run 'nonexistent_cmd_zzz': not found"),
            "{text}"
        );
        assert!(text.contains("lib.spar:2:"), "{text}");
        assert!(text.contains("nonexistent_cmd_zzz --flag;"), "{text}");
    }

    fn rendered_spar_error(error: super::ShellError, input: &str) -> String {
        let errors = match error {
            super::ShellError::Spar(errors) => errors,
            super::ShellError::SparSource { errors, .. } => errors,
            other => panic!("expected a Spar error, got {other:?}"),
        };
        spar::ErrorRenderer::new(input, "<sparsh>").render_all(&errors)
    }

    #[test]
    fn a_plain_top_level_error_prints_no_trace_section() {
        let mut session = ShellSession::new();
        let input = "var z: int = 0; 10 / z";
        // Two statements: the failing one is reported inside the sequence.
        let ShellResult::Sequence(outcomes) = session.submit(input).unwrap() else {
            panic!("expected a sequence");
        };
        let error = outcomes
            .into_iter()
            .find_map(|outcome| outcome.result.err())
            .expect("the division fails");
        let text = rendered_spar_error(error, input);
        assert!(text.contains("division by zero"), "{text}");
        assert!(!text.contains("trace (most recent call first)"), "{text}");
    }

    #[test]
    fn session_defined_functions_do_not_print_wrong_frame_locations() {
        let mut session = ShellSession::new();
        session
            .submit("fn b(x: int) -> int {\n    return 10 / x;\n};")
            .unwrap();
        session
            .submit("fn a(x: int) -> int {\n    return b(x: x);\n};")
            .unwrap();
        let input = "a(x: 0)";
        let error = session.submit(input).unwrap_err();
        let text = rendered_spar_error(error, input);
        let line = |prefix: &str| {
            text.lines()
                .find(|line| line.trim_start().starts_with(prefix))
                .unwrap_or_else(|| panic!("no `{prefix}` line in\n{text}"))
                .to_string()
        };
        assert!(!line("in b").contains("<sparsh>:1:1"), "{text}");
        assert!(!line("called from a").contains("<sparsh>:1:1"), "{text}");
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
    fn spar_variable_can_capture_a_grouped_command_substitution() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("image.jpg");
        let output = directory.path().join("out.txt");
        std::fs::write(&image, b"image").unwrap();
        let mut session = ShellSession::new();
        session.submit(&format!(
            "var img: str = $(find {} -type f \\( -iname '*.jpg' -o -iname '*.png' \\) | head -n1);",
            directory.path().display()
        )).unwrap();
        session
            .submit(&format!(r#"echo "${{img}}" > {}"#, output.display()))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            format!("{}\n", image.display())
        );
    }

    #[test]
    fn shell_result_function_returns_typed_data_to_pipeline() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("profile.txt");
        let mut session = ShellSession::try_new_interactive().unwrap();
        session.submit("struct Data { name: str = \"OCC\"; age: int = 33; };").unwrap();
        session.submit("fn getProfile() -> ShellResult<Data, str> {\n    echo preparing;\n    echo ready;\n    return ok(value: Data());\n};").unwrap();
        let result = session.submit("getProfile() | from json").unwrap();
        let ShellResult::Structured(value) = result else { panic!("expected structured result"); };
        assert!(value.value.render_display().contains("OCC"));
        session.submit(&format!("getProfile() | grep -i age > {}", output.display())).unwrap();
        assert!(std::fs::read_to_string(output).unwrap().contains("age"));
    }

    #[test]
    fn shell_result_direct_call_runs_commands_and_prints_only_returned_value() {
        let directory = tempfile::tempdir().unwrap();
        let side_effect = directory.path().join("side.txt");
        let mut session = ShellSession::new();
        session.submit("fn emit(path: str) -> ShellResult<str, str> {\n echo side > \"${path}\";\n return ok(value: \"payload\");\n};").unwrap();
        let result = session.submit(&format!("emit(path: \"{}\")", side_effect.display())).unwrap();
        assert_eq!(builtin_stdout(result), "payload\n");
        assert_eq!(std::fs::read_to_string(side_effect).unwrap(), "side\n");
        assert_eq!(session.last_status(), 0);
    }

    #[test]
    fn shell_result_quiet_ok_prints_nothing_and_err_keeps_exit_code() {
        let mut session = ShellSession::new();
        session.submit("fn loud() -> ShellResult<int, str> { return ok(value: 5); };").unwrap();
        session.submit("fn quiet() -> ShellResult<int, str> { return ok(value: 5, quiet: true); };").unwrap();
        session.submit("fn bad() -> ShellResult<int, str> { return err(error: \"boom\", exitCode: 7); };").unwrap();
        assert_eq!(builtin_stdout(session.submit("loud()").unwrap()), "5\n");
        assert_eq!(builtin_stdout(session.submit("quiet()").unwrap()), "");
        session.submit("bad()").unwrap();
        assert_eq!(session.last_status(), 7);
    }

    #[test]
    fn z_jumps_to_a_visited_directory_by_name_and_rejects_unknown_ones() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("projects").join("occ");
        std::fs::create_dir_all(&target).unwrap();
        let history = Arc::new(MemoryHistory::default());
        let mut session = ShellSession::new();
        session.set_history_access(history.clone());
        session.submit(&format!("cd {}", target.display())).unwrap();
        session.submit(&format!("cd {}", root.path().display())).unwrap();

        session.submit("z occ").unwrap();
        assert_eq!(session.ui_snapshot().cwd(), target.as_path());
        assert!(session.submit("z never-visited").is_err() || session.last_status() != 0);
        assert_eq!(session.ui_snapshot().cwd(), target.as_path());
    }

    #[test]
    fn listing_builtins_return_tables_in_an_interactive_session_and_text_otherwise() {
        let history = Arc::new(MemoryHistory::default());
        history.entries.lock().unwrap().push("echo one".to_string());
        let mut session = ShellSession::try_new_interactive().unwrap();
        session.set_history_access(history.clone());
        session.submit("alias zz = echo hi").unwrap();
        for command in ["alias", "dirs", "jobs", "history", "history 1", "path", "hash", "export", "help", "z --list"] {
            let result = session.submit(command).unwrap();
            assert!(matches!(result, ShellResult::Structured(_)), "{command}");
        }
        // Arguments, pipes and redirects keep the text builtin.
        assert!(matches!(session.submit("history --clear").unwrap(), ShellResult::Builtin(_)));
        let mut plain = ShellSession::new();
        plain.submit("alias zz = echo hi").unwrap();
        assert!(builtin_stdout(plain.submit("alias").unwrap()).contains("zz"));
    }

    #[test]
    fn theme_list_and_status_are_tables_in_an_interactive_session() {
        // Never read the developer's real theme state.
        let home = tempfile::tempdir().unwrap();
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .services
            .environment
            .set_os("HOME", home.path().as_os_str());
        session.config.themes.push(crate::ThemeKindConfig {
            name: "gruvbox".into(),
            description: Some("warm".into()),
            layer: crate::ThemeLayer::default(),
        });
        let ShellResult::Structured(list) = session.submit("theme list").unwrap() else {
            panic!("theme list should be a table");
        };
        let spar::Value::Table(table) = &list.value else {
            panic!("expected a table, got {:?}", list.value);
        };
        let rows = table.rows();
        assert_eq!(rows.len(), 2);
        let spar::Value::Object(first) = &rows[0] else { panic!("row") };
        assert_eq!(first.get("name"), Some(&spar::Value::String("default".into())));
        assert_eq!(first.get("description"), Some(&spar::Value::String("built-in colors".into())));
        assert_eq!(first.get("active"), Some(&spar::Value::Bool(true)));
        let spar::Value::Object(second) = &rows[1] else { panic!("row") };
        assert_eq!(second.get("description"), Some(&spar::Value::String("warm".into())));
        assert_eq!(second.get("active"), Some(&spar::Value::Bool(false)));
        assert!(matches!(session.submit("theme").unwrap(), ShellResult::Structured(_)));
        // Arguments that change state keep the text builtin.
        assert!(matches!(session.submit("theme set nope"), Err(_) | Ok(ShellResult::Builtin(_))));
        let mut plain = ShellSession::new();
        assert!(builtin_stdout(plain.submit("theme list").unwrap()).contains("default"));
    }

    #[test]
    fn repo_applies_a_compiling_source_and_rejects_a_broken_one_without_changes() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        let mut session = session_with_home(home.path());
        session.submit_spar("var a: int = 1;").unwrap();
        assert!(session.repo_source().contains("var a"));

        let (added, removed) = session
            .apply_repo_source("var a: int = 1;\nvar b: int = 2;\n")
            .unwrap();
        assert_eq!((added, removed), (vec!["b".to_string()], Vec::<String>::new()));
        assert!(session.repo_source().contains("var b"));

        // `missing` does not exist: nothing may change.
        assert!(session.apply_repo_source("var c: int = missing;").is_err());
        assert!(session.repo_source().contains("var b"));
        assert!(!session.repo_source().contains("var c"));

        let (_, removed) = session.apply_repo_source("var a: int = 1;").unwrap();
        assert_eq!(removed, vec!["b".to_string()]);
    }

    #[test]
    fn repl_buffer_reopened_with_an_import_has_no_duplicate_errors_and_no_internal_names() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        let mut session = session_with_home(home.path());
        session.submit_spar("import pkg { get as gFetch } from \"std/http\";").unwrap();
        let source = session.repo_source();
        assert!(session.repo_diagnostics(&source).is_empty(), "{:?}", session.repo_diagnostics(&source));
        let (added, removed) = session.apply_repo_source(&source).unwrap();
        assert!(added.iter().chain(removed.iter()).all(|name| !name.contains("arModule")), "{added:?} {removed:?}");
        let snapshot = session.completion_snapshot();
        assert!(!format!("{snapshot:?}").contains("arModule"));
    }

    #[test]
    fn repl_checks_its_buffer_against_the_config_not_the_live_session() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        let mut session = session_with_home(home.path());
        session.submit_spar("struct DadJoke { id: str = \"\"; };").unwrap();
        let source = session.repo_source();
        session.set_repo_editing(true);
        assert!(session.repo_diagnostics(&source).is_empty(), "{:?}", session.repo_diagnostics(&source));
        // A real mistake in the buffer is still reported.
        assert!(!session.repo_diagnostics("var x: int = missing;").is_empty());
        session.set_repo_editing(false);
    }

    #[test]
    fn result_values_show_their_content_and_ok_pretty_is_a_table() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        session.submit_spar("fn wrapped() -> Result<int, str> { return ok(value: 7); };").unwrap();
        let shown = session.submit("wrapped()").unwrap();
        // No `Ok(7)` wrapper: the value itself.
        assert!(!format!("{shown:?}").contains("Ok("), "{shown:?}");
        session
            .submit_spar("struct Row { id: int = 1; };")
            .unwrap();
        session
            .submit_spar("fn rows() -> ShellResult<List<Row>, str> { return ok(value: [Row(), Row(id: 2)], pretty: true); };")
            .unwrap();
        assert!(matches!(session.submit("rows()").unwrap(), ShellResult::Structured(_)));
    }

    #[test]
    fn repl_keeps_async_functions_and_their_imports() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        let mut session = session_with_home(home.path());
        let source = "import pkg { get } from \"std/http\";\nstruct J { id: str = \"\"; };\nasync fn jokes(term: str = \"\") -> List<J> {\n    var response = await get(url: \"http://x.invalid/\");\n    return [];\n};\n";
        session.apply_repo_source(source).unwrap();
        assert!(session.repo_source().contains("async fn jokes"), "{}", session.repo_source());
        assert!(session.ui_snapshot().has_function("jokes"));
    }

    #[test]
    fn echo_prints_any_value_the_way_print_does() {
        let mut session = ShellSession::new();
        session.submit_spar("struct P { name: str = \"a\"; age: int = 3; };").unwrap();
        session.submit_spar("var l: List<int> = [1, 2];").unwrap();
        session.submit_spar("var p: P = P();").unwrap();
        session.submit_spar("var o: Option<int> = some(value: 4);").unwrap();
        assert_eq!(builtin_stdout(session.submit("echo ${l}").unwrap()), "[1, 2]\n");
        assert_eq!(builtin_stdout(session.submit("echo ${p}").unwrap()), "{name: \"a\", age: 3}\n");
        assert_eq!(builtin_stdout(session.submit("echo list=${l} done").unwrap()), "list=[1, 2] done\n");
        assert!(!builtin_stdout(session.submit("echo ${o}").unwrap()).is_empty());
        // Other commands still insist on text: no silent stringification.
        assert!(session.submit("printf %s ${l}").is_err());
    }

    #[test]
    fn plain_function_call_does_not_echo_its_return_value() {
        let mut session = ShellSession::new();
        session.submit("fn plus(a: int, b: int) -> int { return a + b; };").unwrap();
        assert!(matches!(session.submit("plus(a: 1, b: 2)").unwrap(), ShellResult::Empty));
    }

    #[test]
    fn shell_result_error_goes_to_stderr_and_sets_status() {
        let mut session = ShellSession::new();
        session.submit("fn fail() -> ShellResult<str, str> { return err(error: \"no profile\"); };").unwrap();
        let ShellResult::Builtin(direct) = session.submit("fail()").unwrap() else { panic!("expected builtin output"); };
        assert_eq!(direct.stderr, b"no profile\n");
        assert!(direct.stdout.is_empty());
        assert_eq!(direct.status, 1);
        let result = session.submit("fail() | cat").unwrap();
        assert!(matches!(result, ShellResult::CommandStatus { status: 1, .. }));
        assert_eq!(session.last_status(), 1);
        let decoded = session.submit("fail() | from json").unwrap();
        assert!(matches!(decoded, ShellResult::CommandStatus { status: 1, .. }));
    }

    #[test]
    fn shell_function_output_decodes_with_json_bridge() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("import pkg { stringify } from \"std/json\";")
            .unwrap();
        session
            .submit("export struct Data { name: str = \"OCC\"; age: int = 33; };")
            .unwrap();
        session.submit("fn getProfileJson() -> __shell {\n return __shell {\n echo;\n println(value: stringify(value: Data()));\n };\n};").unwrap();
        let result = session.submit("getProfileJson() | from json").unwrap();
        let ShellResult::Structured(value) = result else {
            panic!("expected structured data");
        };
        assert!(value.value.render_display().contains("OCC"));
        let recalled = session.submit("_ |> to json").unwrap();
        let ShellResult::Structured(recalled) = recalled else {
            panic!("expected decoded record to remain available for structured pipelines");
        };
        assert!(recalled.value.render_display().contains("OCC"));
        let error = session
            .submit("getProfileJson() | from record")
            .unwrap_err();
        assert!(error.to_string().contains("cannot decode record"));
    }

    #[test]
    fn shell_function_println_reaches_downstream_grep() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("result.txt");
        let mut session = ShellSession::new();
        session
            .submit("export struct Data { name: str = \"OCC\"; age: int = 33; };")
            .unwrap();
        session.submit("fn getProfile() -> __shell {\n return __shell {\n echo;\n println(value: Data());\n };\n};").unwrap();
        session
            .submit(&format!("getProfile() | grep OCC > {}", output.display()))
            .unwrap();
        assert!(std::fs::read_to_string(output).unwrap().contains("OCC"));
    }

    #[test]
    fn shell_function_output_pipes_to_another_shell_function_without_status_text() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("result.txt");
        let mut session = ShellSession::new();
        session
            .submit("fn emit() -> __shell { return __shell { printf 'OCC\\n'; }; };")
            .unwrap();
        session
            .submit("fn select(path: str) -> __shell { return __shell { grep OCC > \"${path}\"; }; };")
            .unwrap();
        session
            .submit(&format!("emit() | select(path: \"{}\")", output.display()))
            .unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "OCC\n");
        assert_eq!(session.last_status(), 0);
    }

    #[test]
    fn shell_function_pipeline_does_not_add_exit_status_to_stdout() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("result.txt");
        let mut session = ShellSession::new();
        session
            .submit("fn emit() -> __shell { println(value: \"OCC\"); return __shell { echo; }; };")
            .unwrap();
        session
            .submit(&format!("emit() | grep OCC > {}", output.display()))
            .unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "OCC\n");
        assert_eq!(session.last_status(), 0);
    }

    #[test]
    fn here_document_feeds_external_stdin_and_preserves_output_redirection() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("out.txt");
        let mut session = ShellSession::new();
        session
            .submit(&format!(
                "cat <<'BODY' > {}\nline one\nline two\nBODY",
                output.display()
            ))
            .unwrap();
        assert_eq!(std::fs::read(output).unwrap(), b"line one\nline two\n");
    }

    #[test]
    fn here_document_body_is_never_classified_as_spar() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("out.txt");
        let mut session = ShellSession::new();
        session
            .submit(&format!(
                "cat <<BODY > {}\nvalue |> untouched\nBODY",
                output.display()
            ))
            .unwrap();
        assert_eq!(std::fs::read(output).unwrap(), b"value |> untouched\n");
    }

    #[test]
    fn here_document_missing_terminator_does_not_execute_command() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("out.txt");
        let mut session = ShellSession::new();
        let error = session
            .submit(&format!("cat <<BODY > {}\nunfinished", output.display()))
            .unwrap_err();
        assert_eq!(error.status(), 2);
        assert!(!output.exists());
    }

    #[test]
    fn shell_line_continuation_joins_words_before_execution() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("out.txt");
        let mut session = ShellSession::new();
        session
            .submit(&format!("printf '%s' foo\\\nbar > {}", output.display()))
            .unwrap();
        assert_eq!(std::fs::read(output).unwrap(), b"foobar");
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
                "function build() -> __shell {{ return __shell {{ touch {}; }}; }};",
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
                r#"function writeMarker(output: str) -> __shell {
    return __shell {
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
                r#"function writeMarkers(output: str) -> __shell {
    return __shell {
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
                "function build() -> __shell {{ return __shell {{ touch {}; }}; }};",
                marker.display()
            ))
            .unwrap();
        session.submit("var plan: __shell = build();").unwrap();
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
                "function greet(name: str, output: str) -> __shell { return __shell { printf \"%s %s\" $SPARSH_GREETING ${name} > ${output}; }; }",
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

        let found = builtin_stdout(session.submit("history --search two").unwrap());
        assert!(found.contains("2  echo two"), "{found}");
        session.submit("history --delete 2").unwrap();
        assert_eq!(
            history.entries.lock().unwrap().as_slice(),
            ["echo one", "echo three"]
        );
        session
            .submit("history --delete-exact 'echo three'")
            .unwrap();
        assert_eq!(history.entries.lock().unwrap().as_slice(), ["echo one"]);
        session.submit("history --clear").unwrap();
        assert!(history.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn stealth_builtin_changes_history_mode_and_reports_status() {
        let history = Arc::new(MemoryHistory::default());
        let mut session = ShellSession::new();
        session.set_history_access(history);

        assert_eq!(
            builtin_stdout(session.submit("stealth status").unwrap()),
            "stealth mode: off\n"
        );
        assert_eq!(
            builtin_stdout(session.submit("stealth on").unwrap()),
            "stealth mode: on\n"
        );
        assert!(session.stealth_mode());
        assert_eq!(
            builtin_stdout(session.submit("stealth off").unwrap()),
            "stealth mode: off\n"
        );
        assert!(!session.stealth_mode());
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

        // A plain function call runs silently; `_` keeps the returned value.
        assert!(matches!(
            session.submit(r#"greet(name: "OCC")"#).unwrap(),
            ShellResult::Empty
        ));
        let ShellResult::Value(ConfigValue::Str(value)) = session.submit("_").unwrap() else {
            panic!("expected `_` to hold the sourced function result");
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
            r#"function main() -> int { println(value: "spar-ok"); return 0; };"#,
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
            r#"function main() -> int { println(value: "alpha"); println(value: "beta"); return 0; };"#,
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
                "printf 'name,age,team\\nObi,24,core\\nAda,31,ops\\n' | from csv |> where(predicate: fn(value) => value.age > 20)",
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
    fn a_user_ls_alias_always_wins_over_the_native_table() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit(&format!("cd {}", dir.path().display()))
            .unwrap();
        // Sparsh's own `--color=auto` fallback never blocks the native table.
        assert!(matches!(
            session.submit("ls").unwrap(),
            ShellResult::Structured(_)
        ));

        session
            .services
            .aliases
            .define("ls", vec!["true".into()])
            .unwrap();
        let result = session.submit("ls").unwrap();
        assert!(
            !matches!(result, ShellResult::Structured(_)),
            "user alias should run instead of the native table: {result:?}"
        );
        // With a user alias in place, `ls` is an ordinary external command
        // again: `|> to yaml` on raw bytes is the same parse error Spar gives
        // without any of this feature, not a native listing shortcut.
        assert!(
            session.submit("ls |> to yaml").is_err(),
            "the native `ls |> to FORMAT` shortcut must not survive a user alias"
        );
    }

    #[test]
    fn ll_is_a_native_long_listing_and_a_user_alias_still_wins_over_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit(&format!("cd {}", dir.path().display()))
            .unwrap();

        let ShellResult::Structured(preview) = session.submit("ll").unwrap() else {
            panic!("expected structured listing");
        };
        let spar::Value::Table(table) = preview.value else {
            panic!("expected table");
        };
        assert!(
            table
                .schema()
                .fields
                .iter()
                .any(|field| field.name == "mode"),
            "ll should request the long form (mode/user/group columns)"
        );

        session
            .services
            .aliases
            .define("ll", vec!["true".into()])
            .unwrap();
        let result = session.submit("ll").unwrap();
        assert!(
            !matches!(result, ShellResult::Structured(_)),
            "user alias should run instead of the native table: {result:?}"
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

        let ShellResult::Structured(taken) = session.submit("ls |> take(count: 1)").unwrap() else {
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
    fn json_array_pipes_as_rows_into_data_stages() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("import pkg { where, select, take } from \"std/data\";")
            .unwrap();
        let json = "printf '[{\"name\":\"a\",\"age\":3},{\"name\":\"b\",\"age\":40}]'";
        for (stages, rows) in [
            ("where(predicate: |value| value.age > 5)", 1),
            ("select(fields: [\"name\"])", 2),
            ("take(count: 1)", 1),
        ] {
            let result = session.submit(&format!("{json} | from json |> {stages}"));
            let ShellResult::Structured(value) = result.unwrap_or_else(|error| {
                panic!("{stages}: {error:?}");
            }) else {
                panic!("{stages}: expected structured result");
            };
            let count = match &value.value {
                spar::Value::Table(table) => table.len(),
                spar::Value::List(items) => items.len(),
                _ => 1,
            };
            assert_eq!(count, rows, "{stages}");
        }
        // A one-element array stays a one-row list, not a bare object.
        let one = session.submit("printf '[{\"a\":1}]' | from json").unwrap();
        let ShellResult::Structured(one) = one else {
            panic!("expected structured result");
        };
        assert!(
            matches!(one.value, spar::Value::Table(_) | spar::Value::List(_)),
            "{:?}",
            one.value
        );
        // A plain object is still one value.
        let object = session.submit("printf '{\"a\":1}' | from json").unwrap();
        let ShellResult::Structured(object) = object else {
            panic!("expected structured result");
        };
        assert!(
            matches!(object.value, spar::Value::Object(_)),
            "{:?}",
            object.value
        );
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
            "printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> where(predicate: fn(value) => value.age > 24)";
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
        let result = session
            .submit("printf 'a\\n1\\n' | from csv |> where(predicate: fn(value) => value.a > 0)");
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
    fn awaited_http_declaration_persists_without_refetching() {
        let url = serve_once("text/plain", "hello ${world}");
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("import pkg { get } from \"std/http\";")
            .unwrap();
        session
            .submit(&format!(
                "var res: HttpResponse = await get(url: \"{url}\");"
            ))
            .unwrap();
        let status = session.submit("res.status").unwrap();
        assert!(
            matches!(status, ShellResult::Value(spar::ConfigValue::Int(200))),
            "{status:?}"
        );
        assert!(matches!(
            session.submit("res.body").unwrap(),
            ShellResult::Value(spar::ConfigValue::Str(body)) if body == "hello ${world}"
        ));
        assert!(matches!(
            session.submit("res.headers.get(key: \"content-type\").unwrap()").unwrap(),
            ShellResult::Value(spar::ConfigValue::Str(value)) if value == "text/plain"
        ));
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
            Some(&spar::Value::Option(Some(Box::new(spar::Value::String(
                "application/json".into()
            )))))
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

    #[test]
    fn typed_http_json_can_be_filtered_in_one_prompt_expression() {
        let url = serve_once(
            "application/json",
            r#"{"stats":[{"stat":"hp","base":48},{"stat":"speed","base":12}]}"#,
        );
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("import pkg { get } from \"std/http\";")
            .unwrap();
        session
            .submit("struct Stat { stat: str; base: int; };")
            .unwrap();
        session
            .submit("struct Payload { stats: List<Stat>; };")
            .unwrap();
        let result = session.submit(&format!(
            "(await get(url: \"{url}\")).json<Payload>().stats |> where(predicate: fn(value) => value.base > 40) |> count()"
        )).unwrap();
        assert!(
            matches!(result, ShellResult::Value(spar::ConfigValue::Int(1))),
            "{result:?}"
        );
    }

    const STATS: &str = r#"{"name":"ditto","stats":[{"stat":"hp","base":48},{"stat":"attack","base":48},{"stat":"speed","base":48},{"stat":"special","base":10}],"meta":{"id":1}}"#;

    #[test]
    #[ignore = "structured-pipe type inference can't determine `.json().stats`'s element \
                type through a generic `HttpResponse.json<T>()` call chained straight into \
                `|>` with no explicit type argument or intermediate var declaration to anchor \
                T — fails with \"cannot determine structured pipe input type\". Not traced to \
                a specific fix; needs a dedicated look at structured-pipe input-type inference \
                for generic method calls. See spar_real_world_dx_handoff.md item 5."]
    fn a_fetched_json_list_can_be_chained_through_where_select_and_take_in_one_line() {
        let url = serve(3, "application/json", STATS);
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("import pkg { get } from \"std/http\";")
            .unwrap();

        let ShellResult::Structured(rows) = session
            .submit(&format!(
                "(await get(url: \"{url}\")).json().stats |> where(predicate: fn(value) => value.base > 40) |> select(fields: [\"stat\"])"
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
                "(await get(url: \"{url}\")).json().stats |> where(predicate: fn(value) => value.stat == \"hp\") |> count()"
            ))
            .unwrap();
        assert!(
            matches!(&count, ShellResult::Value(spar::ConfigValue::Int(1))),
            "{count:?}"
        );

        // A record where a list is needed fails with a clear message.
        let error = session
            .submit(&format!(
                "(await get(url: \"{url}\")).json().meta |> take(count: 1)"
            ))
            .unwrap_err();
        assert!(error.to_string().contains("list"), "unclear error: {error}");
    }

    #[test]
    fn srepl_builtin_requests_repl_editor_mode() {
        let mut session = ShellSession::new();
        assert!(matches!(
            session.submit("srepl").unwrap(),
            ShellResult::EditorMode(super::EditorMode::Repl)
        ));
    }

    fn write_tools_package(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("spar.package.spar"),
            "struct Package {\n    name: str = \"my-tools\";\n    version: str = \"1.0.0\";\n    kind: str = \"library\";\n};\n",
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
                "{}\nstruct Config {{\n    keybindings: Option<List<SparshKeybinding>> = some(value: [SparshKeybinding(key: \"ctrl+l\", action: \"clearScreen\")]);\n}};\n",
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

    fn typed_error_column(result: Result<ShellResult, super::ShellError>, input: &str) {
        let error = match result {
            Err(error) => error,
            Ok(ShellResult::Sequence(outcomes)) => outcomes
                .into_iter()
                .find_map(|outcome| outcome.result.err())
                .expect("an outcome failed"),
            other => panic!("expected an error, got {other:?}"),
        };
        let super::ShellError::SparSource { errors, source, .. } = error else {
            panic!("expected a Spar error");
        };
        assert_eq!(source, input, "errors render against the typed submission");
        let span = errors[0].span();
        assert!(input[span.start..].starts_with("nope"), "{span:?}");
        assert_eq!(span.col as usize, input.find("nope").unwrap() + 1);
    }

    #[test]
    fn block_rewrite_errors_point_at_the_typed_text() {
        let input = "if true { echo ok; nope() }";
        typed_error_column(ShellSession::new().submit(input), input);
    }

    #[test]
    fn rewritten_block_errors_in_a_multi_statement_line_point_at_the_typed_text() {
        let input = "var a: int = 1; if true { echo ok; nope() }";
        typed_error_column(ShellSession::new().submit(input), input);
    }

    #[test]
    fn statements_run_in_order_and_continue_after_a_failure() {
        let mut session = ShellSession::new();
        let result = session
            .submit("var a: int = 2; nope(1); a + 1")
            .unwrap();
        let ShellResult::Sequence(outcomes) = result else {
            panic!("expected a sequence")
        };
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes[0].result.is_ok());
        assert!(outcomes[1].result.is_err());
        assert!(matches!(
            outcomes[2].result,
            Ok(ShellResult::Value(spar::ConfigValue::Int(3)))
        ));
    }
}
