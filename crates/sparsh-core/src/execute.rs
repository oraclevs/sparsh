use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use spar_command::{CommandPlan, PipelinePlan, RedirectMode, Redirection, ShellPlan};

use crate::builtin::{BuiltinContext, BuiltinRegistry};
use crate::path::PathService;
use crate::resolver::ResolutionMode;
use crate::services::ShellServices;
use crate::session::{CommandDiagnostic, SessionMode, ShellError, ShellResult};

enum PreparedCommand {
    Builtin { name: String, plan: CommandPlan },
    SparScript(CommandPlan),
    External(CommandPlan),
    Missing {
        program: String,
        suggestions: Vec<String>,
    },
}

pub(crate) fn execute_plan(
    plan: &ShellPlan,
    registry: &BuiltinRegistry,
    services: &mut ShellServices,
    last_status: i32,
    mode: SessionMode,
) -> Result<ShellResult, ShellError> {
    let mut executor = SparshExecutor {
        registry,
        services,
        last_status,
        mode,
        requested_exit: None,
        requested_editor_mode: None,
        requested_reload_config: false,
        requested_exec: None,
        requested_source: None,
        last_result: None,
    };
    let outcome = spar_process::run_plan(plan, &mut executor)?;
    if let Some(status) = executor.requested_exit {
        return Ok(ShellResult::Exit(status));
    }
    if let Some(mode) = executor.requested_editor_mode {
        return Ok(ShellResult::EditorMode(mode));
    }
    if executor.requested_reload_config {
        return Ok(ShellResult::ReloadConfig);
    }
    if let Some((words, template)) = executor.requested_exec {
        return Ok(ShellResult::ExecRequest {
            words,
            template: Box::new(template),
        });
    }
    if let Some(request) = executor.requested_source {
        return Ok(ShellResult::SourceRequest(request));
    }
    Ok(executor.last_result.unwrap_or({
        ShellResult::Process(spar::ShellPlanOutcome {
            success: outcome.success,
            exit_code: outcome.exit_code,
            signal: None,
            pid: 0,
            pipeline: Vec::new(),
        })
    }))
}

struct SparshExecutor<'a> {
    registry: &'a BuiltinRegistry,
    services: &'a mut ShellServices,
    last_status: i32,
    mode: SessionMode,
    requested_exit: Option<i32>,
    requested_editor_mode: Option<crate::session::EditorMode>,
    requested_reload_config: bool,
    requested_exec: Option<(Vec<String>, CommandPlan)>,
    requested_source: Option<crate::builtin::SourceRequest>,
    last_result: Option<ShellResult>,
}

impl spar_process::StepExecutor for SparshExecutor<'_> {
    type Error = ShellError;

    fn run_command(
        &mut self,
        command: &CommandPlan,
    ) -> Result<spar_process::ExitStatus, Self::Error> {
        let command_text = render_command_for_job(command);
        match prepare_command(
            command,
            ResolutionMode::Normal,
            self.registry,
            self.services,
        )? {
            PreparedCommand::Builtin { name, plan } => self.run_builtin(&name, &plan),
            PreparedCommand::SparScript(plan) => self.run_spar_script(&plan),
            PreparedCommand::External(plan) => self.run_external(&plan, command_text),
            PreparedCommand::Missing {
                program,
                suggestions,
            } => self.run_missing(program, suggestions),
        }
    }

    fn run_pipeline(
        &mut self,
        pipeline: &PipelinePlan,
    ) -> Result<spar_process::ExitStatus, Self::Error> {
        let mut prepared = Vec::with_capacity(pipeline.commands.len());
        for command in &pipeline.commands {
            prepared.push(prepare_command(
                command,
                ResolutionMode::Normal,
                self.registry,
                self.services,
            )?);
        }

        if prepared
            .iter()
            .all(|stage| matches!(stage, PreparedCommand::External(_)))
        {
            let commands = prepared
                .into_iter()
                .map(|stage| match stage {
                    PreparedCommand::External(command) => command,
                    _ => unreachable!("all stages were checked as external"),
                })
                .collect::<Vec<_>>();
            let pipeline = PipelinePlan { commands };
            let command_text = render_pipeline_for_job(&pipeline);
            if pipeline
                .commands
                .last()
                .is_some_and(|command| command.background)
            {
                return self.run_background_pipeline(&pipeline, command_text);
            }
            if self.mode == SessionMode::InteractiveTty && spar_process::stdin_is_tty() {
                return self.run_foreground_pipeline(&pipeline, command_text);
            }
            let options = execution_options(self.services);
            let output = spar_process::run_pipeline(&pipeline, &options).map_err(process_error)?;
            let outcome = shell_outcome_from_pipeline(&output);
            self.last_status = outcome.exit_code;
            self.last_result = Some(ShellResult::Process(outcome));
            return Ok(output.status);
        }

        if pipeline
            .commands
            .last()
            .is_some_and(|command| command.background)
        {
            return Err(ShellError::Process {
                message: "background pipelines containing Sparsh builtins are not supported".into(),
                status: 2,
            });
        }

        self.run_mixed_pipeline(prepared)
    }

    fn should_stop(&self) -> bool {
        self.requested_exit.is_some()
            || self.requested_exec.is_some()
            || self.requested_source.is_some()
            || self.requested_reload_config
    }
}

impl SparshExecutor<'_> {
    fn run_missing(
        &mut self,
        program: String,
        suggestions: Vec<String>,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        let status = spar_process::ExitStatus {
            success: false,
            code: Some(127),
        };
        self.last_status = 127;
        self.last_result = Some(ShellResult::CommandStatus {
            status: 127,
            diagnostic: Some(CommandDiagnostic::NotFound {
                program,
                suggestions,
            }),
        });
        Ok(status)
    }

    fn run_mixed_pipeline(
        &mut self,
        stages: Vec<PreparedCommand>,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        let mut statuses = Vec::new();
        let mut input = None;
        let mut index = 0usize;
        let mut final_in_process = None;

        while index < stages.len() {
            match &stages[index] {
                PreparedCommand::Builtin { name, plan } => {
                    let mut output = run_isolated_builtin_stage(
                        self.registry,
                        self.services,
                        name,
                        plan,
                        input.take().unwrap_or_default(),
                        self.last_status,
                    )?;
                    let status = spar_process::ExitStatus {
                        success: output.status == 0,
                        code: Some(output.status),
                    };
                    statuses.push(status);
                    if !output.stderr.is_empty() {
                        io::stderr()
                            .lock()
                            .write_all(&output.stderr)
                            .map_err(process_error)?;
                        output.stderr.clear();
                    }
                    if index + 1 == stages.len() {
                        final_in_process = Some(output);
                    } else {
                        input = Some(output.stdout);
                    }
                    index += 1;
                }
                PreparedCommand::SparScript(plan) => {
                    let mut output = run_spar_script_stage(
                        self.services,
                        plan,
                        input.take(),
                    )?;
                    let status = spar_process::ExitStatus {
                        success: output.status == 0,
                        code: Some(output.status),
                    };
                    statuses.push(status);
                    if !output.stderr.is_empty() {
                        io::stderr().lock().write_all(&output.stderr).map_err(process_error)?;
                        output.stderr.clear();
                    }
                    if index + 1 == stages.len() {
                        final_in_process = Some(output);
                    } else {
                        input = Some(output.stdout);
                    }
                    index += 1;
                }
                PreparedCommand::Missing {
                    program,
                    suggestions,
                } => {
                    let status = spar_process::ExitStatus {
                        success: false,
                        code: Some(127),
                    };
                    statuses.push(status);
                    if index + 1 == stages.len() {
                        let aggregate = aggregate_pipeline_status(&statuses);
                        self.last_status = aggregate.code.unwrap_or(127);
                        self.last_result = Some(ShellResult::CommandStatus {
                            status: self.last_status,
                            diagnostic: Some(CommandDiagnostic::NotFound {
                                program: program.clone(),
                                suggestions: suggestions.clone(),
                            }),
                        });
                        return Ok(aggregate);
                    }
                    input = Some(Vec::new());
                    index += 1;
                }
                PreparedCommand::External(_) => {
                    let start = index;
                    while index < stages.len()
                        && matches!(&stages[index], PreparedCommand::External(_))
                    {
                        index += 1;
                    }
                    let followed_by_in_process = index < stages.len();
                    let commands = stages[start..index]
                        .iter()
                        .map(|stage| match stage {
                            PreparedCommand::External(command) => command.clone(),
                            _ => unreachable!("external segment contains non-external stage"),
                        })
                        .collect::<Vec<_>>();
                    let mut options = execution_options(self.services);
                    options.capture_stdout = followed_by_in_process;
                    let output = spar_process::run_pipeline_with_input(
                        &PipelinePlan { commands },
                        &options,
                        input.take(),
                    )
                    .map_err(process_error)?;
                    statuses.push(output.status.clone());
                    if followed_by_in_process {
                        input = Some(output.stdout.unwrap_or_default());
                    }
                }
            }
        }

        let aggregate = aggregate_pipeline_status(&statuses);
        self.last_status = aggregate.code.unwrap_or(if aggregate.success { 0 } else { 1 });
        if let Some(mut output) = final_in_process {
            output.status = self.last_status;
            self.last_result = Some(ShellResult::Builtin(output));
        } else {
            self.last_result = Some(ShellResult::Process(shell_outcome(&aggregate)));
        }
        Ok(aggregate)
    }

    fn run_spar_script(&mut self, plan: &CommandPlan) -> Result<spar_process::ExitStatus, ShellError> {
        if plan.background {
            return Err(ShellError::Process {
                message: "background execution of in-process .spar scripts is not supported yet".into(),
                status: 2,
            });
        }
        let mut output = run_spar_script_stage(self.services, plan, None)?;
        if !output.stdout.is_empty() {
            io::stdout().lock().write_all(&output.stdout).map_err(process_error)?;
            output.stdout.clear();
        }
        if !output.stderr.is_empty() {
            io::stderr().lock().write_all(&output.stderr).map_err(process_error)?;
            output.stderr.clear();
        }
        let status = spar_process::ExitStatus { success: output.status == 0, code: Some(output.status) };
        self.last_status = output.status;
        self.last_result = Some(ShellResult::Process(shell_outcome(&status)));
        Ok(status)
    }

    fn run_external(
        &mut self,
        plan: &CommandPlan,
        command_text: String,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        if plan.background {
            return self.run_background_command(plan, command_text);
        }
        if self.mode == SessionMode::InteractiveTty && spar_process::stdin_is_tty() {
            return self.run_foreground_command(plan, command_text);
        }
        let options = execution_options(self.services);
        let output = spar_process::run_command(plan, &options).map_err(process_error)?;
        let outcome = shell_outcome_from_command(&output);
        self.last_status = outcome.exit_code;
        self.last_result = Some(ShellResult::Process(outcome));
        Ok(output.status)
    }

    fn run_background_command(
        &mut self,
        plan: &CommandPlan,
        command_text: String,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        let options = execution_options(self.services);
        let spawned = spar_process::spawn_job_command(plan, &options).map_err(process_error)?;
        let pgid = spawned.pgid.0;
        let id = self.services.jobs.insert(spawned, command_text);
        let status = spar_process::ExitStatus {
            success: true,
            code: Some(0),
        };
        self.last_status = 0;
        self.last_result = Some(ShellResult::BackgroundJob { id: id.0, pgid });
        Ok(status)
    }

    fn run_background_pipeline(
        &mut self,
        pipeline: &PipelinePlan,
        command_text: String,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        let options = execution_options(self.services);
        let spawned = spar_process::spawn_job_pipeline(pipeline, &options).map_err(process_error)?;
        let pgid = spawned.pgid.0;
        let id = self.services.jobs.insert(spawned, command_text);
        let status = spar_process::ExitStatus {
            success: true,
            code: Some(0),
        };
        self.last_status = 0;
        self.last_result = Some(ShellResult::BackgroundJob { id: id.0, pgid });
        Ok(status)
    }

    fn run_foreground_command(
        &mut self,
        plan: &CommandPlan,
        command_text: String,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        let options = execution_options(self.services);
        let spawned = spar_process::spawn_job_command(plan, &options).map_err(process_error)?;
        self.finish_foreground_job(spawned, command_text)
    }

    fn run_foreground_pipeline(
        &mut self,
        pipeline: &PipelinePlan,
        command_text: String,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        let options = execution_options(self.services);
        let spawned = spar_process::spawn_job_pipeline(pipeline, &options).map_err(process_error)?;
        self.finish_foreground_job(spawned, command_text)
    }

    fn finish_foreground_job(
        &mut self,
        spawned: spar_process::SpawnedJob,
        command_text: String,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        let lease = TerminalLease::acquire(spawned.pgid).map_err(process_error)?;
        let observed = observe_foreground_job(&spawned);
        let restore = lease.finish();
        let observed = observed.map_err(process_error)?;
        restore.map_err(process_error)?;

        if observed.stopped {
            let id = self.services.jobs.insert(spawned.clone(), command_text);
            observed.replay_into(&mut self.services.jobs);
            let _ = id;
        }

        let outcome = observed.shell_outcome(&spawned);
        let status = spar_process::ExitStatus {
            success: outcome.success,
            code: Some(outcome.exit_code),
        };
        self.last_status = outcome.exit_code;
        self.last_result = Some(ShellResult::Process(outcome));
        Ok(status)
    }

    fn run_builtin(
        &mut self,
        name: &str,
        plan: &CommandPlan,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        if plan.background {
            return Err(ShellError::Builtin(crate::builtin::BuiltinError {
                message: format!("{name}: Sparsh builtins cannot run in the background"),
                status: 2,
            }));
        }
        // Prepare every redirection before executing a stateful builtin.
        // A failed open therefore cannot leave cd/export/alias half-applied.
        let cwd = self.services.directories.current().to_path_buf();
        let mut streams = BuiltinStreams::prepare(plan, &cwd).map_err(process_error)?;
        let (stdin, stdin_available) = read_builtin_stdin(plan, &cwd).map_err(process_error)?;
        let login_shell = self.services.login_shell;
        let mut context = BuiltinContext {
            services: self.services,
            last_status: self.last_status,
            requested_exit: None,
            requested_editor_mode: None,
            requested_reload_config: false,
            requested_exec: None,
            requested_source: None,
            stdin,
            stdin_available,
            login_shell,
            resolution_mode: ResolutionMode::Normal,
            session_mode: self.mode,
        };
        let output = self
            .registry
            .execute(name, &plan.args, &mut context)
            .map_err(ShellError::Builtin)?;
        let visible = streams.route(output).map_err(process_error)?;
        self.requested_exit = context.requested_exit;
        self.requested_editor_mode = context.requested_editor_mode;
        self.requested_reload_config = context.requested_reload_config;
        self.requested_exec = context
            .requested_exec
            .map(|words| (words, plan.clone()));
        self.requested_source = context.requested_source;
        self.last_status = visible.status;
        let status = spar_process::ExitStatus {
            success: visible.status == 0,
            code: Some(visible.status),
        };
        self.last_result = Some(ShellResult::Builtin(visible));
        Ok(status)
    }
}


pub(crate) fn replace_with_external_command(
    words: &[String],
    mut template: CommandPlan,
    services: &mut ShellServices,
) -> Result<(), ShellError> {
    let Some(program) = words.first() else {
        return Err(ShellError::Builtin(crate::builtin::BuiltinError {
            message: "exec: missing command".into(),
            status: 2,
        }));
    };
    // Preserve the exec builtin's already-parsed cwd/environment/redirections
    // while replacing only its command words. Redirections were preflighted
    // before the stateful builtin ran and are reopened by exec(2) setup.
    template.program = program.clone();
    template.args = words[1..].to_vec();
    template.background = false;
    let registry = BuiltinRegistry::new();
    match prepare_command(&template, ResolutionMode::Normal, &registry, services)? {
        PreparedCommand::External(plan) => {
            let error = spar_process::replace_process(&plan, &execution_options(services));
            Err(process_error(error))
        }
        PreparedCommand::SparScript(_) => Err(ShellError::Process {
            message: "exec: in-process .spar execution cannot replace Sparsh; use 'spar file.spar' for process replacement".into(),
            status: 2,
        }),
        PreparedCommand::Builtin { name, .. } => Err(ShellError::Builtin(crate::builtin::BuiltinError {
            message: format!("exec: cannot replace Sparsh with builtin '{name}'"),
            status: 2,
        })),
        PreparedCommand::Missing { program, suggestions } => Err(ShellError::CommandNotFound {
            program,
            suggestions,
        }),
    }
}

fn should_execute_as_spar(path: &Path) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) != Some("spar") {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else { return false; };
    let first_line = bytes.split(|byte| *byte == b'\n').next().unwrap_or_default();
    if !first_line.starts_with(b"#!") {
        return true;
    }
    let line = String::from_utf8_lossy(first_line);
    line.split_whitespace().any(|word| word == "spar" || word.ends_with("/spar"))
}

fn run_spar_script_stage(
    services: &ShellServices,
    plan: &CommandPlan,
    pipeline_input: Option<Vec<u8>>,
) -> Result<crate::builtin::BuiltinOutput, ShellError> {
    use std::sync::{Arc, Mutex};

    let path = Path::new(&plan.program);
    let source = std::fs::read_to_string(path).map_err(|error| ShellError::Process {
        message: format!("failed to read Spar script {}: {error}", path.display()),
        status: 1,
    })?;
    let filename = path.display().to_string();
    let engine = spar::Engine::default();
    let compiled = engine.compile_path(path).map_err(|errors| ShellError::SparSource {
        errors,
        source: source.clone(),
        filename: filename.clone(),
    })?;
    let cwd = match &plan.cwd {
        Some(spar_command::WorkingDirectory::Path(path)) => PathBuf::from(path),
        None => services.directories.current().to_path_buf(),
    };
    let mut context = spar::RuntimeContext::new(cwd.clone());
    let mut environment = services.environment.snapshot();
    for entry in &plan.env {
        environment.retain(|(key, _)| key != OsStr::new(&entry.key));
        environment.push((entry.key.clone().into(), entry.value.clone().into()));
    }
    context.replace_environment(environment.iter().filter_map(|(key, value)| {
        Some((key.to_str()?.to_string(), value.to_str()?.to_string()))
    }));
    context.set_args(plan.args.clone());

    if let Some(input) = pipeline_input {
        context.set_stdin(spar::RuntimeInput::from_bytes(input));
    } else if let Some((input, true)) = read_builtin_stdin(plan, &cwd).ok() {
        context.set_stdin(spar::RuntimeInput::from_bytes(input));
    }

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    context.set_stdout(spar::RuntimeOutput::Buffer(stdout.clone()));
    context.set_stderr(spar::RuntimeOutput::Buffer(stderr.clone()));
    let outcome = engine
        .execute_compiled_with_context(&compiled, context)
        .map_err(|errors| ShellError::SparSource {
            errors,
            source: source.clone(),
            filename: filename.clone(),
        })?;
    let output = crate::builtin::BuiltinOutput {
        stdout: stdout.lock().map_err(|_| ShellError::Process { message: "Spar stdout buffer lock poisoned".into(), status: 1 })?.clone(),
        stderr: stderr.lock().map_err(|_| ShellError::Process { message: "Spar stderr buffer lock poisoned".into(), status: 1 })?.clone(),
        status: outcome.exit_status,
    };
    let mut streams = BuiltinStreams::prepare(plan, &cwd).map_err(process_error)?;
    streams.route(output).map_err(process_error)
}

fn prepare_command(
    original: &CommandPlan,
    mode: ResolutionMode,
    registry: &BuiltinRegistry,
    services: &mut ShellServices,
) -> Result<PreparedCommand, ShellError> {
    let mut plan = original.clone();
    if plan.cwd.is_none() {
        plan.cwd = Some(spar_command::WorkingDirectory::Path(
            services.directories.current().to_string_lossy().into_owned(),
        ));
    }
    if mode == ResolutionMode::Normal {
        let words = services
            .aliases
            .expand(&plan.program, &plan.args)
            .map_err(service_error)?;
        plan.program = words[0].clone();
        plan.args = words[1..].to_vec();
    }

    let home = services.environment.get("HOME").map(PathBuf::from);
    expand_tilde_in_plan(&mut plan, home.as_deref())?;

    if mode != ResolutionMode::BuiltinOnly && matches!(plan.program.as_str(), "command" | "builtin")
    {
        let wrapper = plan.program.clone();
        if plan.args.is_empty() {
            return Err(ShellError::Builtin(crate::builtin::BuiltinError {
                message: format!("usage: {wrapper} name [argument ...]"),
                status: 2,
            }));
        }
        plan.program = plan.args.remove(0);
        let next_mode = if wrapper == "command" {
            ResolutionMode::BypassAlias
        } else {
            ResolutionMode::BuiltinOnly
        };
        return prepare_command(&plan, next_mode, registry, services);
    }

    if mode != ResolutionMode::ExternalOnly && registry.find(&plan.program).is_some() {
        return Ok(PreparedCommand::Builtin {
            name: plan.program.clone(),
            plan,
        });
    }
    if mode == ResolutionMode::BuiltinOnly {
        return Err(ShellError::Process {
            status: 1,
            message: format!("{}: not a Sparsh builtin", plan.program),
        });
    }

    let resolved = if let Some(value) = plan
        .env
        .iter()
        .rev()
        .find(|entry| entry.key == "PATH")
        .map(|entry| entry.value.as_str())
    {
        let temporary = PathService::from_environment(Some(OsStr::new(value)));
        services.resolver.external_uncached(
            &plan.program,
            temporary.directories(),
            services.directories.current(),
        )
    } else {
        services.resolver.external(
            &plan.program,
            &services.path,
            services.directories.current(),
        )
    };
    match resolved {
        Ok(resolved) => {
            plan.program = resolved.to_string_lossy().into_owned();
            if should_execute_as_spar(&resolved) {
                Ok(PreparedCommand::SparScript(plan))
            } else {
                Ok(PreparedCommand::External(plan))
            }
        }
        Err(message) if message == format!("command not found: `{}`", plan.program) => {
            let mut additional = registry.names();
            additional.extend(services.aliases.names().map(str::to_string));
            let suggestions = services
                .resolver
                .suggestions(&plan.program, &services.path, &additional);
            Ok(PreparedCommand::Missing {
                program: plan.program,
                suggestions,
            })
        }
        Err(message) => Err(ShellError::Process {
            message,
            status: 127,
        }),
    }
}

fn expand_tilde_in_plan(plan: &mut CommandPlan, home: Option<&Path>) -> Result<(), ShellError> {
    plan.program = expand_tilde_word(&plan.program, home)?;
    for argument in &mut plan.args {
        *argument = expand_tilde_word(argument, home)?;
    }
    if let Some(redirection) = &mut plan.stdin {
        expand_tilde_redirection(redirection, home)?;
    }
    if let Some(redirection) = &mut plan.stdout {
        expand_tilde_redirection(redirection, home)?;
    }
    if let Some(redirection) = &mut plan.stderr {
        expand_tilde_redirection(redirection, home)?;
    }
    for redirection in &mut plan.redirections {
        expand_tilde_redirection(&mut redirection.target, home)?;
    }
    Ok(())
}

fn expand_tilde_redirection(
    redirection: &mut Redirection,
    home: Option<&Path>,
) -> Result<(), ShellError> {
    if let Redirection::File { path, .. } = redirection {
        *path = expand_tilde_word(path, home)?;
    }
    Ok(())
}

fn expand_tilde_word(value: &str, home: Option<&Path>) -> Result<String, ShellError> {
    let rest = if value == "~" {
        Some("")
    } else {
        value.strip_prefix("~/")
    };
    let Some(rest) = rest else {
        return Ok(value.to_string());
    };
    let home = home.ok_or_else(|| ShellError::Process {
        message: "HOME is not set; cannot expand '~'".into(),
        status: 1,
    })?;
    let expanded = if rest.is_empty() {
        home.to_path_buf()
    } else {
        home.join(rest)
    };
    Ok(expanded.to_string_lossy().into_owned())
}

fn run_isolated_builtin_stage(
    registry: &BuiltinRegistry,
    services: &ShellServices,
    name: &str,
    plan: &CommandPlan,
    stdin: Vec<u8>,
    last_status: i32,
) -> Result<crate::builtin::BuiltinOutput, ShellError> {
    let mut isolated = services.snapshot_for_isolated_stage();
    let cwd = isolated.directories.current().to_path_buf();
    let mut streams = BuiltinStreams::prepare(plan, &cwd).map_err(process_error)?;
    let login_shell = isolated.login_shell;
    let mut context = BuiltinContext {
        services: &mut isolated,
        last_status,
        requested_exit: None,
        requested_editor_mode: None,
        requested_reload_config: false,
        requested_exec: None,
        requested_source: None,
        stdin,
        stdin_available: true,
        login_shell,
        resolution_mode: ResolutionMode::Normal,
        session_mode: SessionMode::NonInteractive,
    };
    let output = registry
        .execute(name, &plan.args, &mut context)
        .map_err(ShellError::Builtin)?;
    streams.route(output).map_err(process_error)
}

fn aggregate_pipeline_status(statuses: &[spar_process::ExitStatus]) -> spar_process::ExitStatus {
    let success = statuses.iter().all(|status| status.success);
    let code = if success {
        statuses.last().and_then(|status| status.code).unwrap_or(0)
    } else {
        statuses
            .iter()
            .rev()
            .find(|status| !status.success)
            .and_then(|status| status.code)
            .unwrap_or(1)
    };
    spar_process::ExitStatus {
        success,
        code: Some(code),
    }
}

fn execution_options(services: &ShellServices) -> spar_process::ExecutionOptions {
    spar_process::ExecutionOptions {
        environment: Some(services.environment.snapshot()),
        ..Default::default()
    }
}

fn read_builtin_stdin(plan: &CommandPlan, cwd: &Path) -> io::Result<(Vec<u8>, bool)> {
    let mut selected = plan.stdin.as_ref();
    for ordered in &plan.redirections {
        if ordered.fd == 0 {
            selected = Some(&ordered.target);
        }
    }
    let Some(redirection) = selected else {
        return Ok((Vec::new(), false));
    };
    match redirection {
        Redirection::File { path, .. } => {
            let path = resolve_builtin_path(path, cwd);
            std::fs::read(path).map(|bytes| (bytes, true))
        }
        Redirection::DuplicateFd(source) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("builtin stdin fd duplication from {source} is not supported"),
        )),
    }
}

#[derive(Debug)]
enum BuiltinSink {
    CaptureStdout,
    CaptureStderr,
    File(File),
}

impl BuiltinSink {
    fn duplicate(&self) -> io::Result<Self> {
        match self {
            Self::CaptureStdout => Ok(Self::CaptureStdout),
            Self::CaptureStderr => Ok(Self::CaptureStderr),
            Self::File(file) => file.try_clone().map(Self::File),
        }
    }
}

struct BuiltinStreams {
    stdout: BuiltinSink,
    stderr: BuiltinSink,
}

impl BuiltinStreams {
    fn prepare(plan: &CommandPlan, cwd: &Path) -> io::Result<Self> {
        let mut streams = Self {
            stdout: BuiltinSink::CaptureStdout,
            stderr: BuiltinSink::CaptureStderr,
        };

        if let Some(redirection) = &plan.stdout {
            streams.apply(1, redirection, cwd)?;
        }
        if let Some(redirection) = &plan.stderr {
            streams.apply(2, redirection, cwd)?;
        }
        for redirection in &plan.redirections {
            streams.apply(redirection.fd, &redirection.target, cwd)?;
        }
        Ok(streams)
    }

    fn apply(&mut self, fd: u32, redirection: &Redirection, cwd: &Path) -> io::Result<()> {
        let sink = match redirection {
            Redirection::File { path, mode } => {
                let path = resolve_builtin_path(path, cwd);
                let mut options = OpenOptions::new();
                options.create(true).write(true);
                match mode {
                    RedirectMode::Truncate => {
                        options.truncate(true);
                    }
                    RedirectMode::Append => {
                        options.append(true);
                    }
                }
                BuiltinSink::File(options.open(path)?)
            }
            Redirection::DuplicateFd(source) => self.sink(*source)?.duplicate()?,
        };
        *self.sink_mut(fd)? = sink;
        Ok(())
    }

    fn sink(&self, fd: u32) -> io::Result<&BuiltinSink> {
        match fd {
            1 => Ok(&self.stdout),
            2 => Ok(&self.stderr),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported builtin fd: {fd}"),
            )),
        }
    }

    fn sink_mut(&mut self, fd: u32) -> io::Result<&mut BuiltinSink> {
        match fd {
            1 => Ok(&mut self.stdout),
            2 => Ok(&mut self.stderr),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported builtin fd: {fd}"),
            )),
        }
    }

    fn route(&mut self, output: crate::builtin::BuiltinOutput) -> io::Result<crate::builtin::BuiltinOutput> {
        let mut visible_stdout = Vec::new();
        let mut visible_stderr = Vec::new();
        write_builtin_bytes(
            &mut self.stdout,
            &output.stdout,
            &mut visible_stdout,
            &mut visible_stderr,
        )?;
        write_builtin_bytes(
            &mut self.stderr,
            &output.stderr,
            &mut visible_stdout,
            &mut visible_stderr,
        )?;
        Ok(crate::builtin::BuiltinOutput {
            stdout: visible_stdout,
            stderr: visible_stderr,
            status: output.status,
        })
    }
}

fn resolve_builtin_path(path: &str, cwd: &Path) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    }
}

fn write_builtin_bytes(
    sink: &mut BuiltinSink,
    bytes: &[u8],
    visible_stdout: &mut Vec<u8>,
    visible_stderr: &mut Vec<u8>,
) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    match sink {
        BuiltinSink::CaptureStdout => visible_stdout.extend_from_slice(bytes),
        BuiltinSink::CaptureStderr => visible_stderr.extend_from_slice(bytes),
        BuiltinSink::File(file) => file.write_all(bytes)?,
    }
    Ok(())
}

fn render_command_for_job(command: &CommandPlan) -> String {
    let mut words = Vec::with_capacity(command.args.len() + 1);
    words.push(command.program.clone());
    words.extend(command.args.iter().map(|argument| shell_quote(argument)));
    if command.background {
        words.push("&".into());
    }
    words.join(" ")
}

fn render_pipeline_for_job(pipeline: &PipelinePlan) -> String {
    pipeline
        .commands
        .iter()
        .map(render_command_for_job)
        .collect::<Vec<_>>()
        .join(" | ")
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._/:".contains(character))
    {
        value.to_string()
    } else {
        format!("'{value}'", value = value.replace('\'', "'\\''"))
    }
}

struct TerminalLease {
    shell_pgid: spar_process::ProcessGroupId,
    terminal_state: spar_process::TerminalState,
    active: bool,
}

impl TerminalLease {
    fn acquire(job_pgid: spar_process::ProcessGroupId) -> io::Result<Self> {
        let shell_pgid = spar_process::current_process_group_id()?;
        let terminal_state = spar_process::capture_terminal_state(0)?;
        spar_process::set_terminal_foreground_pgid(0, job_pgid)?;
        Ok(Self {
            shell_pgid,
            terminal_state,
            active: true,
        })
    }

    fn finish(mut self) -> io::Result<()> {
        let ownership = spar_process::set_terminal_foreground_pgid(0, self.shell_pgid);
        let terminal = spar_process::restore_terminal_state(0, &self.terminal_state);
        self.active = false;
        ownership?;
        terminal
    }
}

impl Drop for TerminalLease {
    fn drop(&mut self) {
        if self.active {
            let _ = spar_process::set_terminal_foreground_pgid(0, self.shell_pgid);
            let _ = spar_process::restore_terminal_state(0, &self.terminal_state);
        }
    }
}

#[derive(Clone, Debug)]
enum ObservedProcessState {
    Running,
    Stopped(i32),
    Exited(i32),
    Signaled(i32),
}

impl ObservedProcessState {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Exited(_) | Self::Signaled(_))
    }

    fn is_stopped(&self) -> bool {
        matches!(self, Self::Stopped(_))
    }

    fn status_code(&self) -> Option<i32> {
        match self {
            Self::Exited(code) => Some(*code),
            Self::Signaled(signal) | Self::Stopped(signal) => Some(128 + *signal),
            Self::Running => None,
        }
    }

    fn signal(&self) -> Option<i32> {
        match self {
            Self::Signaled(signal) | Self::Stopped(signal) => Some(*signal),
            Self::Running | Self::Exited(_) => None,
        }
    }
}

struct ObservedJob {
    states: Vec<(u32, ObservedProcessState)>,
    stopped: bool,
}

impl ObservedJob {
    fn replay_into(&self, jobs: &mut crate::job::JobTable) {
        for (pid, state) in &self.states {
            let wait_state = match state {
                ObservedProcessState::Running => None,
                ObservedProcessState::Stopped(signal) => Some(spar_process::WaitState::Stopped {
                    pid: *pid,
                    signal: *signal,
                }),
                ObservedProcessState::Exited(code) => Some(spar_process::WaitState::Exited {
                    pid: *pid,
                    code: *code,
                }),
                ObservedProcessState::Signaled(signal) => Some(spar_process::WaitState::Signaled {
                    pid: *pid,
                    signal: *signal,
                }),
            };
            if let Some(wait_state) = wait_state {
                jobs.apply_wait_state(wait_state);
            }
        }
    }

    fn shell_outcome(&self, spawned: &spar_process::SpawnedJob) -> spar::ShellPlanOutcome {
        let pipeline = self
            .states
            .iter()
            .map(|(pid, state)| {
                let code = state.status_code().unwrap_or(1);
                spar_process::ProcessStatus {
                    code,
                    success: code == 0,
                    signal: state.signal(),
                    pid: *pid,
                }
            })
            .collect::<Vec<_>>();
        let success = !self.stopped && pipeline.iter().all(|process| process.success);
        let exit_code = if success {
            pipeline.last().map_or(0, |process| process.code)
        } else {
            pipeline
                .iter()
                .rev()
                .find(|process| !process.success)
                .map_or(1, |process| process.code)
        };
        let last = pipeline.last();
        spar::ShellPlanOutcome {
            success,
            exit_code,
            signal: last.and_then(|process| process.signal),
            pid: spawned.last_pid(),
            pipeline,
        }
    }
}

fn observe_foreground_job(spawned: &spar_process::SpawnedJob) -> io::Result<ObservedJob> {
    let mut states = spawned
        .processes
        .iter()
        .map(|process| (process.pid, ObservedProcessState::Running))
        .collect::<Vec<_>>();

    loop {
        if states.iter().all(|(_, state)| state.is_terminal()) {
            return Ok(ObservedJob {
                states,
                stopped: false,
            });
        }
        let all_live_stopped = states
            .iter()
            .filter(|(_, state)| !state.is_terminal())
            .all(|(_, state)| state.is_stopped());
        if all_live_stopped {
            return Ok(ObservedJob {
                states,
                stopped: true,
            });
        }

        for (pid, state) in &mut states {
            if !matches!(&*state, ObservedProcessState::Running) {
                continue;
            }
            let Some(wait_state) = spar_process::wait_pid(*pid, false)? else {
                return Err(io::Error::other(format!(
                    "lost wait state for foreground child {pid}"
                )));
            };
            *state = match wait_state {
                spar_process::WaitState::Continued { .. } => ObservedProcessState::Running,
                spar_process::WaitState::Stopped { signal, .. } => {
                    ObservedProcessState::Stopped(signal)
                }
                spar_process::WaitState::Exited { code, .. } => ObservedProcessState::Exited(code),
                spar_process::WaitState::Signaled { signal, .. } => {
                    ObservedProcessState::Signaled(signal)
                }
            };
        }
    }
}

fn shell_outcome_from_command(output: &spar_process::CommandOutput) -> spar::ShellPlanOutcome {
    shell_outcome_from_output(output)
}

fn shell_outcome_from_pipeline(output: &spar_process::CommandOutput) -> spar::ShellPlanOutcome {
    shell_outcome_from_output(output)
}

fn shell_outcome_from_output(output: &spar_process::CommandOutput) -> spar::ShellPlanOutcome {
    let pipeline = output
        .pipeline_status
        .as_ref()
        .map(|status| status.processes.clone())
        .unwrap_or_default();
    let last = pipeline.last();
    spar::ShellPlanOutcome {
        success: output.status.success,
        exit_code: output
            .status
            .code
            .unwrap_or(if output.status.success { 0 } else { 1 }),
        signal: last.and_then(|process| process.signal),
        pid: last.map_or(0, |process| process.pid),
        pipeline,
    }
}

fn shell_outcome(status: &spar_process::ExitStatus) -> spar::ShellPlanOutcome {
    spar::ShellPlanOutcome {
        success: status.success,
        exit_code: status.code.unwrap_or(if status.success { 0 } else { 1 }),
        signal: None,
        pid: 0,
        pipeline: Vec::new(),
    }
}

fn process_error(error: std::io::Error) -> ShellError {
    ShellError::Process {
        status: if error.kind() == std::io::ErrorKind::NotFound {
            127
        } else {
            1
        },
        message: error.to_string(),
    }
}

fn service_error(message: String) -> ShellError {
    ShellError::Process { message, status: 1 }
}
