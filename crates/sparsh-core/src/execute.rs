use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::Write;

use spar_command::{CommandPlan, PipelinePlan, RedirectMode, Redirection, ShellPlan};

use crate::builtin::{BuiltinContext, BuiltinRegistry};
use crate::path::PathService;
use crate::resolver::ResolutionMode;
use crate::services::ShellServices;
use crate::session::{ShellError, ShellResult};

enum PreparedCommand {
    Builtin { name: String, plan: CommandPlan },
    External(CommandPlan),
}

pub(crate) fn execute_plan(
    plan: &ShellPlan,
    registry: &BuiltinRegistry,
    services: &mut ShellServices,
    last_status: i32,
) -> Result<ShellResult, ShellError> {
    let mut executor = SparshExecutor {
        registry,
        services,
        last_status,
        requested_exit: None,
        last_result: None,
    };
    let outcome = spar_process::run_plan(plan, &mut executor)?;
    if let Some(status) = executor.requested_exit {
        return Ok(ShellResult::Exit(status));
    }
    Ok(executor.last_result.unwrap_or({
        ShellResult::Process(spar::ShellPlanOutcome {
            success: outcome.success,
            exit_code: outcome.exit_code,
        })
    }))
}

struct SparshExecutor<'a> {
    registry: &'a BuiltinRegistry,
    services: &'a mut ShellServices,
    last_status: i32,
    requested_exit: Option<i32>,
    last_result: Option<ShellResult>,
}

impl spar_process::StepExecutor for SparshExecutor<'_> {
    type Error = ShellError;

    fn run_command(
        &mut self,
        command: &CommandPlan,
    ) -> Result<spar_process::ExitStatus, Self::Error> {
        match prepare_command(
            command,
            ResolutionMode::Normal,
            self.registry,
            self.services,
        )? {
            PreparedCommand::Builtin { name, plan } => self.run_builtin(&name, &plan),
            PreparedCommand::External(plan) => self.run_external(&plan),
        }
    }

    fn run_pipeline(
        &mut self,
        pipeline: &PipelinePlan,
    ) -> Result<spar_process::ExitStatus, Self::Error> {
        let mut commands = Vec::with_capacity(pipeline.commands.len());
        for command in &pipeline.commands {
            match prepare_command(
                command,
                ResolutionMode::Normal,
                self.registry,
                self.services,
            )? {
                PreparedCommand::External(command) => commands.push(command),
                PreparedCommand::Builtin { name, .. } => {
                    return Err(ShellError::Process {
                        status: 1,
                        message: format!(
                            "{name}: Sparsh builtins in pipelines are not supported yet"
                        ),
                    });
                }
            }
        }
        let options = execution_options(self.services);
        let output = spar_process::run_pipeline(&PipelinePlan { commands }, &options)
            .map_err(process_error)?;
        let outcome = shell_outcome(&output.status);
        self.last_status = outcome.exit_code;
        self.last_result = Some(ShellResult::Process(outcome));
        Ok(output.status)
    }

    fn should_stop(&self) -> bool {
        self.requested_exit.is_some()
    }
}

impl SparshExecutor<'_> {
    fn run_external(&mut self, plan: &CommandPlan) -> Result<spar_process::ExitStatus, ShellError> {
        let options = execution_options(self.services);
        let output = spar_process::run_command(plan, &options).map_err(process_error)?;
        let outcome = shell_outcome(&output.status);
        self.last_status = outcome.exit_code;
        self.last_result = Some(ShellResult::Process(outcome));
        Ok(output.status)
    }

    fn run_builtin(
        &mut self,
        name: &str,
        plan: &CommandPlan,
    ) -> Result<spar_process::ExitStatus, ShellError> {
        let mut redirect = prepare_builtin_output(&plan.stdout).map_err(process_error)?;
        let mut context = BuiltinContext {
            services: self.services,
            last_status: self.last_status,
            requested_exit: None,
            resolution_mode: ResolutionMode::Normal,
        };
        let mut output = self
            .registry
            .execute(name, &plan.args, &mut context)
            .map_err(ShellError::Builtin)?;
        if let (Some(file), Some(text)) = (&mut redirect, &output.stdout) {
            file.write_all(text.as_bytes()).map_err(process_error)?;
            output.stdout = None;
        }
        self.requested_exit = context.requested_exit;
        self.last_status = output.status;
        let status = spar_process::ExitStatus {
            success: output.status == 0,
            code: Some(output.status),
        };
        self.last_result = Some(ShellResult::Builtin(output));
        Ok(status)
    }
}

fn prepare_command(
    original: &CommandPlan,
    mode: ResolutionMode,
    registry: &BuiltinRegistry,
    services: &mut ShellServices,
) -> Result<PreparedCommand, ShellError> {
    let mut plan = original.clone();
    if mode == ResolutionMode::Normal {
        let words = services
            .aliases
            .expand(&plan.program, &plan.args)
            .map_err(service_error)?;
        plan.program = words[0].clone();
        plan.args = words[1..].to_vec();
    }

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
    plan.program = resolved
        .map_err(|message| resolution_error(message, &plan.program, registry, services))?
        .to_string_lossy()
        .into_owned();
    Ok(PreparedCommand::External(plan))
}

fn execution_options(services: &ShellServices) -> spar_process::ExecutionOptions {
    spar_process::ExecutionOptions {
        environment: Some(services.environment.snapshot()),
        ..Default::default()
    }
}

fn prepare_builtin_output(redirection: &Option<Redirection>) -> std::io::Result<Option<File>> {
    match redirection {
        None => Ok(None),
        Some(Redirection::File { path, mode }) => {
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
            options.open(path).map(Some)
        }
        Some(Redirection::DuplicateFd(1)) => Ok(None),
        Some(Redirection::DuplicateFd(fd)) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unsupported builtin stdout duplicate fd: {fd}"),
        )),
    }
}

fn shell_outcome(status: &spar_process::ExitStatus) -> spar::ShellPlanOutcome {
    spar::ShellPlanOutcome {
        success: status.success,
        exit_code: status.code.unwrap_or(if status.success { 0 } else { 1 }),
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

fn resolution_error(
    message: String,
    missing: &str,
    registry: &BuiltinRegistry,
    services: &ShellServices,
) -> ShellError {
    if message != format!("command not found: `{missing}`") {
        return ShellError::Process {
            message,
            status: 127,
        };
    }
    let mut additional = registry.names();
    additional.extend(services.aliases.names().map(str::to_string));
    let suggestions = services
        .resolver
        .suggestions(missing, &services.path, &additional);
    ShellError::CommandNotFound {
        program: missing.to_string(),
        suggestions,
    }
}
