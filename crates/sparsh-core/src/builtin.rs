use std::path::{Path, PathBuf};

use crate::resolver::ResolutionMode;
use crate::services::ShellServices;

mod history;
mod io;
mod pkg;
mod session_extra;
mod system;

#[derive(Debug)]
pub struct BuiltinMetadata {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub usage: &'static str,
    pub category: &'static str,
    pub mutates_shell_state: bool,
    pub reads_stdin: bool,
    pub produces_output: bool,
    pub completion_key: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BuiltinOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRequest {
    pub path: String,
    pub shell: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BuiltinError {
    pub message: String,
    pub status: i32,
}

pub(crate) struct BuiltinContext<'a> {
    pub services: &'a mut ShellServices,
    pub last_status: i32,
    pub requested_exit: Option<i32>,
    pub requested_editor_mode: Option<crate::session::EditorMode>,
    pub requested_reload_config: bool,
    pub requested_exec: Option<Vec<String>>,
    pub requested_source: Option<SourceRequest>,
    pub stdin: Vec<u8>,
    pub stdin_available: bool,
    pub login_shell: bool,
    pub resolution_mode: ResolutionMode,
    pub session_mode: crate::session::SessionMode,
}

type Handler =
    fn(&[String], &mut BuiltinContext<'_>, &BuiltinRegistry) -> Result<BuiltinOutput, BuiltinError>;

struct Builtin {
    metadata: BuiltinMetadata,
    handler: Handler,
}

pub struct BuiltinRegistry {
    entries: Vec<Builtin>,
}

macro_rules! builtin {
    ($name:literal, $description:literal, $usage:literal, $category:literal, $mutates:literal, $output:literal, $handler:path) => {
        Builtin {
            metadata: BuiltinMetadata {
                name: $name,
                aliases: &[],
                description: $description,
                usage: $usage,
                category: $category,
                mutates_shell_state: $mutates,
                reads_stdin: false,
                produces_output: $output,
                completion_key: $name,
            },
            handler: $handler,
        }
    };
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        Self {
            entries: vec![
                builtin!(
                    "cd",
                    "Change directory",
                    "cd [directory]",
                    "directory",
                    true,
                    false,
                    cd
                ),
                builtin!(
                    "pwd",
                    "Print current directory",
                    "pwd",
                    "directory",
                    false,
                    true,
                    pwd
                ),
                builtin!(
                    "pushd",
                    "Push a directory",
                    "pushd [directory]",
                    "directory",
                    true,
                    true,
                    pushd
                ),
                builtin!(
                    "popd",
                    "Pop a directory",
                    "popd",
                    "directory",
                    true,
                    true,
                    popd
                ),
                builtin!(
                    "dirs",
                    "Print directory stack",
                    "dirs",
                    "directory",
                    false,
                    true,
                    dirs
                ),
                builtin!(
                    "alias",
                    "Define or list aliases",
                    "alias [name = command ...]",
                    "resolution",
                    true,
                    true,
                    alias
                ),
                builtin!(
                    "unalias",
                    "Remove aliases",
                    "unalias name [...]",
                    "resolution",
                    true,
                    false,
                    unalias
                ),
                builtin!(
                    "export",
                    "Set or list environment",
                    "export [NAME[=value] ...]",
                    "environment",
                    true,
                    true,
                    export
                ),
                builtin!(
                    "unset",
                    "Remove environment entries",
                    "unset NAME [...]",
                    "environment",
                    true,
                    false,
                    unset
                ),
                builtin!(
                    "path",
                    "Inspect or edit PATH",
                    "path [prepend|append|remove directory]",
                    "environment",
                    true,
                    true,
                    path
                ),
                builtin!(
                    "hash",
                    "Inspect or clear command cache",
                    "hash [-r]",
                    "resolution",
                    true,
                    true,
                    hash
                ),
                builtin!(
                    "type",
                    "Describe command resolution",
                    "type name [...]",
                    "resolution",
                    false,
                    true,
                    command_type
                ),
                builtin!(
                    "which",
                    "Locate external commands",
                    "which name [...]",
                    "resolution",
                    false,
                    true,
                    which
                ),
                builtin!(
                    "command",
                    "Run with aliases bypassed",
                    "command name [argument ...]",
                    "resolution",
                    false,
                    false,
                    wrapper
                ),
                builtin!(
                    "builtin",
                    "Run only a builtin",
                    "builtin name [argument ...]",
                    "resolution",
                    false,
                    false,
                    wrapper
                ),
                builtin!(
                    "jobs",
                    "List shell-owned jobs",
                    "jobs",
                    "job",
                    false,
                    true,
                    jobs
                ),
                builtin!(
                    "fg",
                    "Move a job to the foreground",
                    "fg [%job]",
                    "job",
                    true,
                    false,
                    fg
                ),
                builtin!(
                    "bg",
                    "Resume a stopped job in the background",
                    "bg [%job]",
                    "job",
                    true,
                    true,
                    bg
                ),
                builtin!(
                    "wait",
                    "Wait for a shell-owned job",
                    "wait [%job]",
                    "job",
                    true,
                    false,
                    wait
                ),
                builtin!(
                    "disown",
                    "Remove a job from shell ownership",
                    "disown [%job]",
                    "job",
                    true,
                    false,
                    disown
                ),
                builtin!(
                    "kill",
                    "Send a signal to a job or process",
                    "kill [-SIGNAL] target [...]",
                    "job",
                    true,
                    false,
                    kill
                ),
                builtin!(
                    "history",
                    "List or clear command history",
                    "history [N|-c]",
                    "history",
                    true,
                    true,
                    history::history
                ),
                builtin!(
                    "echo",
                    "Write arguments separated by spaces",
                    "echo [-n] [argument ...]",
                    "io",
                    false,
                    true,
                    io::echo
                ),
                builtin!(
                    "printf",
                    "Format and write arguments",
                    "printf format [argument ...]",
                    "io",
                    false,
                    true,
                    io::printf
                ),
                builtin!(
                    "read",
                    "Read one line into a shell environment variable",
                    "read [-r] NAME",
                    "io",
                    true,
                    false,
                    io::read
                ),
                builtin!(
                    "umask",
                    "Show or set the process file-creation mask",
                    "umask [0000-0777]",
                    "system",
                    true,
                    true,
                    system::umask
                ),
                builtin!(
                    "ulimit",
                    "Show or set selected process resource limits",
                    "ulimit -a | -n|-c|-s|-u [value|unlimited]",
                    "system",
                    true,
                    true,
                    system::ulimit
                ),
                builtin!(
                    "help",
                    "Show Sparsh builtin help",
                    "help [builtin]",
                    "session",
                    false,
                    true,
                    session_extra::help
                ),
                builtin!(
                    "exec",
                    "Replace Sparsh with an external command",
                    "exec command [argument ...]",
                    "session",
                    true,
                    false,
                    session_extra::exec
                ),
                builtin!(
                    "logout",
                    "Exit a login shell",
                    "logout",
                    "session",
                    true,
                    false,
                    session_extra::logout
                ),
                Builtin {
                    metadata: BuiltinMetadata {
                        name: "source",
                        aliases: &["."],
                        description: "Load Spar code or import foreign shell environment changes",
                        usage: "source [--shell bash|zsh|sh] FILE",
                        category: "session",
                        mutates_shell_state: true,
                        reads_stdin: false,
                        produces_output: false,
                        completion_key: "source",
                    },
                    handler: session_extra::source,
                },
                builtin!(
                    "deactivate",
                    "Deactivate the active Python virtual environment",
                    "deactivate",
                    "environment",
                    true,
                    false,
                    deactivate
                ),
                builtin!(
                    "repl",
                    "Enter multiline Spar REPL mode",
                    "repl",
                    "session",
                    true,
                    false,
                    repl
                ),
                builtin!(
                    "pkg",
                    "Manage dependencies of the ~/.sparsh config package",
                    "pkg add <alias> <request> | remove <alias> | install [--offline] | update [alias] | tree",
                    "package",
                    true,
                    true,
                    pkg::pkg
                ),
                builtin!(
                    "reload",
                    "Reload ~/.sparsh/src/config.spar transactionally",
                    "reload",
                    "session",
                    true,
                    false,
                    reload
                ),
                builtin!(
                    "exit",
                    "Exit the shell",
                    "exit [status]",
                    "session",
                    true,
                    false,
                    exit
                ),
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
        context: &mut BuiltinContext<'_>,
    ) -> Result<BuiltinOutput, BuiltinError> {
        let _resolution_mode = context.resolution_mode;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.metadata.name == name || entry.metadata.aliases.contains(&name))
            .expect("execute is only called after builtin lookup");
        (entry.handler)(arguments, context, self)
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.metadata.name.to_string())
            .collect()
    }
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn cd(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.len() > 1 {
        return Err(usage_error("cd [directory]"));
    }
    let target = match args.first() {
        Some(target) => target.clone(),
        None => context
            .services
            .environment
            .get("HOME")
            .ok_or_else(|| error("cd: HOME is not set"))?
            .to_string_lossy()
            .into_owned(),
    };
    context
        .services
        .directories
        .change(&target, &mut context.services.environment)
        .map_err(error)?;
    Ok(success(None))
}

fn deactivate(
    args: &[String],
    context: &mut BuiltinContext<'_>,
    _: &BuiltinRegistry,
) -> BuiltinResult {
    require_empty(args, "deactivate")?;
    context
        .services
        .deactivate_python_environment()
        .map_err(error)?;
    Ok(success(None))
}

fn pwd(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    require_empty(args, "pwd")?;
    Ok(success(Some(format!(
        "{}\n",
        context.services.directories.current().display()
    ))))
}

fn pushd(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.len() > 1 {
        return Err(usage_error("pushd [directory]"));
    }
    context
        .services
        .directories
        .push(
            args.first().map(String::as_str),
            &mut context.services.environment,
        )
        .map_err(error)?;
    Ok(success(Some(context.services.directories.render())))
}

fn popd(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    require_empty(args, "popd")?;
    context
        .services
        .directories
        .pop(&mut context.services.environment)
        .map_err(error)?;
    Ok(success(Some(context.services.directories.render())))
}

fn dirs(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    require_empty(args, "dirs")?;
    Ok(success(Some(context.services.directories.render())))
}

fn alias(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.is_empty() {
        return Ok(success(Some(context.services.aliases.render())));
    }
    if args.len() < 3 || args[1] != "=" {
        return Err(usage_error("alias [name = command ...]"));
    }
    context
        .services
        .aliases
        .define(&args[0], args[2..].to_vec())
        .map_err(error)?;
    Ok(success(None))
}

fn unalias(
    args: &[String],
    context: &mut BuiltinContext<'_>,
    _: &BuiltinRegistry,
) -> BuiltinResult {
    if args.is_empty() {
        return Err(usage_error("unalias name [...]"));
    }
    context.services.aliases.remove_many(args).map_err(error)?;
    Ok(success(None))
}

fn export(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.is_empty() {
        return Ok(success(Some(context.services.environment.render())));
    }
    let mut assignments = Vec::new();
    let mut names = Vec::new();
    for argument in args {
        if let Some((name, value)) = argument.split_once('=') {
            assignments.push((name.to_string(), value.to_string()));
        } else {
            names.push(argument.clone());
        }
    }
    let mut candidate = context.services.environment.clone();
    candidate.set_many(&assignments).map_err(error)?;
    candidate.ensure_many(&names).map_err(error)?;
    let path_changed = args
        .iter()
        .any(|arg| arg == "PATH" || arg.starts_with("PATH="));
    context.services.environment = candidate;
    if path_changed {
        context.services.reload_path_from_environment();
        context.services.resolver.clear();
    }
    Ok(success(None))
}

fn unset(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.is_empty() {
        return Err(usage_error("unset NAME [...]"));
    }
    let mut candidate = context.services.environment.clone();
    candidate.unset_many(args).map_err(error)?;
    let path_changed = args.iter().any(|name| name == "PATH");
    context.services.environment = candidate;
    if path_changed {
        context.services.reload_path_from_environment();
        context.services.resolver.clear();
    }
    Ok(success(None))
}

fn path(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.is_empty() {
        return Ok(success(Some(context.services.path.render())));
    }
    if args.len() != 2 {
        return Err(usage_error("path [prepend|append|remove directory]"));
    }
    let mut candidate = context.services.path.clone();
    let cwd = context.services.directories.current();
    let home = context.services.environment.get("HOME").map(PathBuf::from);
    match args[0].as_str() {
        "prepend" => candidate.prepend(Path::new(&args[1]), cwd, home.as_deref()),
        "append" => candidate.append(Path::new(&args[1]), cwd, home.as_deref()),
        "remove" => candidate.remove(Path::new(&args[1]), cwd, home.as_deref()),
        _ => return Err(usage_error("path [prepend|append|remove directory]")),
    }
    .map_err(error)?;
    candidate.to_environment().map_err(error)?;
    context.services.path = candidate;
    context.services.sync_path_environment().map_err(error)?;
    context.services.resolver.clear();
    Ok(success(None))
}

fn hash(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args == ["-r"] {
        context.services.resolver.clear();
        context.services.path.invalidate_executable_cache();
        return Ok(success(None));
    }
    require_empty(args, "hash [-r]")?;
    let mut output = String::new();
    for (name, path) in context.services.resolver.entries() {
        output.push_str(&format!("{name}={}\n", path.display()));
    }
    Ok(success(Some(output)))
}

fn command_type(
    args: &[String],
    context: &mut BuiltinContext<'_>,
    registry: &BuiltinRegistry,
) -> BuiltinResult {
    if args.is_empty() {
        return Err(usage_error("type name [...]"));
    }
    let mut output = String::new();
    for name in args {
        let expanded = context.services.aliases.expand(name, &[]).map_err(error)?;
        if context.services.aliases.get(name).is_some() {
            output.push_str(&format!("{name} is an alias for {}\n", expanded.join(" ")));
        }
        let target = &expanded[0];
        if registry.find(target).is_some() {
            output.push_str(&format!("{target} is a Sparsh builtin\n"));
        } else {
            let resolved = context
                .services
                .resolver
                .external(
                    target,
                    &context.services.path,
                    context.services.directories.current(),
                )
                .map_err(error)?;
            output.push_str(&format!("{target} is {}\n", resolved.display()));
        }
    }
    Ok(success(Some(output)))
}

fn which(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.is_empty() {
        return Err(usage_error("which name [...]"));
    }
    let mut output = String::new();
    for name in args {
        let resolved = context
            .services
            .resolver
            .external(
                name,
                &context.services.path,
                context.services.directories.current(),
            )
            .map_err(error)?;
        output.push_str(&format!("{}\n", resolved.display()));
    }
    Ok(success(Some(output)))
}

fn jobs(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    require_empty(args, "jobs")?;
    context.services.jobs.poll().map_err(io_error)?;
    let mut output = String::new();
    for job in context.services.jobs.snapshots() {
        let state = match job.state {
            crate::job::JobState::Running => "Running".to_string(),
            crate::job::JobState::Stopped => "Stopped".to_string(),
            crate::job::JobState::Done(code) => format!("Done({code})"),
        };
        output.push_str(&format!("[{}] {state}  {}\n", job.id.0, job.command_text));
    }
    Ok(success(Some(output)))
}

fn fg(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let requested = parse_optional_job_target(args, "fg [%job]")?;
    context.services.jobs.poll().map_err(io_error)?;
    let id = resolve_job_target(&context.services.jobs, requested)?;
    let state = context
        .services
        .jobs
        .foreground(
            id,
            context.session_mode == crate::session::SessionMode::InteractiveTty,
        )
        .map_err(io_error)?
        .ok_or_else(|| error(format!("fg: no such job: %{}", id.0)))?;
    let status = match state {
        crate::job::JobState::Done(code) => {
            context.services.jobs.remove(id);
            code
        }
        crate::job::JobState::Stopped => 128,
        crate::job::JobState::Running => 0,
    };
    Ok(status_output(status, Vec::new()))
}

fn bg(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let requested = parse_optional_job_target(args, "bg [%job]")?;
    context.services.jobs.poll().map_err(io_error)?;
    let id = resolve_job_target(&context.services.jobs, requested)?;
    let snapshot = context
        .services
        .jobs
        .get(id)
        .ok_or_else(|| error(format!("bg: no such job: %{}", id.0)))?;
    if matches!(snapshot.state, crate::job::JobState::Done(_)) {
        return Err(error(format!("bg: job %{} is already done", id.0)));
    }
    context
        .services
        .jobs
        .resume(id)
        .map_err(io_error)?
        .ok_or_else(|| error(format!("bg: no such job: %{}", id.0)))?;
    Ok(success(Some(format!(
        "[{}] {}\n",
        id.0, snapshot.command_text
    ))))
}

fn wait(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let requested = parse_optional_job_target(args, "wait [%job]")?;
    context.services.jobs.poll().map_err(io_error)?;
    let id = resolve_job_target(&context.services.jobs, requested)?;
    let state = context
        .services
        .jobs
        .wait_until_stable(id)
        .map_err(io_error)?
        .ok_or_else(|| error(format!("wait: no such job: %{}", id.0)))?;
    let status = match state {
        crate::job::JobState::Done(code) => {
            context.services.jobs.remove(id);
            code
        }
        crate::job::JobState::Stopped => 128,
        crate::job::JobState::Running => 0,
    };
    Ok(status_output(status, Vec::new()))
}

fn disown(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let requested = parse_optional_job_target(args, "disown [%job]")?;
    context.services.jobs.poll().map_err(io_error)?;
    let id = resolve_job_target(&context.services.jobs, requested)?;
    context
        .services
        .jobs
        .remove(id)
        .ok_or_else(|| error(format!("disown: no such job: %{}", id.0)))?;
    Ok(success(None))
}

fn kill(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.is_empty() {
        return Err(usage_error("kill [-SIGNAL] target [...]"));
    }
    context.services.jobs.poll().map_err(io_error)?;

    let mut signal = spar_process::signal_number("TERM").unwrap_or(15);
    let mut first_target = 0usize;
    if let Some(option) = args
        .first()
        .filter(|value| value.starts_with('-') && value.len() > 1)
    {
        signal = spar_process::signal_number(&option[1..])
            .ok_or_else(|| error(format!("kill: unknown signal: {}", &option[1..])))?;
        first_target = 1;
    }
    if first_target == args.len() {
        return Err(usage_error("kill [-SIGNAL] target [...]"));
    }

    for target in &args[first_target..] {
        if target.starts_with('%') {
            let id = parse_job_id(target)?;
            let pgid = context
                .services
                .jobs
                .pgid(id)
                .ok_or_else(|| error(format!("kill: no such job: {target}")))?;
            spar_process::signal_process_group(pgid, signal).map_err(io_error)?;
            if Some(signal) == spar_process::signal_number("CONT") {
                context.services.jobs.mark_running(id);
            }
        } else {
            let pid = target
                .parse::<u32>()
                .map_err(|_| error(format!("kill: invalid process id: {target}")))?;
            spar_process::signal_process(pid, signal).map_err(io_error)?;
        }
    }
    Ok(success(None))
}

fn parse_optional_job_target(
    args: &[String],
    usage: &str,
) -> Result<Option<crate::job::JobId>, BuiltinError> {
    if args.len() > 1 {
        return Err(usage_error(usage));
    }
    args.first().map(|value| parse_job_id(value)).transpose()
}

fn parse_job_id(value: &str) -> Result<crate::job::JobId, BuiltinError> {
    let value = value.strip_prefix('%').unwrap_or(value);
    let id = value
        .parse::<u64>()
        .map_err(|_| error(format!("invalid job id: {value}")))?;
    if id == 0 {
        return Err(error("job id must be greater than zero"));
    }
    Ok(crate::job::JobId(id))
}

fn resolve_job_target(
    jobs: &crate::job::JobTable,
    requested: Option<crate::job::JobId>,
) -> Result<crate::job::JobId, BuiltinError> {
    jobs.resolve_id(requested)
        .ok_or_else(|| error("no current job"))
}

fn wrapper(_: &[String], _: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    Err(error("internal wrapper dispatch failure"))
}

fn repl(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    require_empty(args, "repl")?;
    context.requested_editor_mode = Some(crate::session::EditorMode::Repl);
    Ok(success(None))
}

fn reload(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    require_empty(args, "reload")?;
    context.requested_reload_config = true;
    Ok(success(None))
}

fn exit(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.len() > 1 {
        return Err(usage_error("exit [status]"));
    }
    let status = match args.first() {
        None => context.last_status,
        Some(value) => value
            .parse::<u8>()
            .map(i32::from)
            .map_err(|_| usage_error("exit [status]"))?,
    };
    context.requested_exit = Some(status);
    Ok(success(None))
}

pub(super) fn valid_environment_name(name: &str) -> bool {
    crate::environment::valid_name(name)
}

type BuiltinResult = Result<BuiltinOutput, BuiltinError>;

fn success(stdout: Option<String>) -> BuiltinOutput {
    BuiltinOutput {
        stdout: stdout.unwrap_or_default().into_bytes(),
        stderr: Vec::new(),
        status: 0,
    }
}

fn status_output(status: i32, stdout: Vec<u8>) -> BuiltinOutput {
    BuiltinOutput {
        stdout,
        stderr: Vec::new(),
        status,
    }
}

fn io_error(error: std::io::Error) -> BuiltinError {
    BuiltinError {
        message: error.to_string(),
        status: 1,
    }
}

fn error(message: impl Into<String>) -> BuiltinError {
    BuiltinError {
        message: message.into(),
        status: 1,
    }
}

fn usage_error(usage: &str) -> BuiltinError {
    BuiltinError {
        message: format!("usage: {usage}"),
        status: 2,
    }
}

fn require_empty(args: &[String], usage: &str) -> Result<(), BuiltinError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(usage_error(usage))
    }
}

#[cfg(test)]
mod tests {
    use super::BuiltinRegistry;

    #[test]
    fn registry_contains_service_builtins_with_execution_metadata() {
        let registry = BuiltinRegistry::new();
        let expected = [
            "cd",
            "pwd",
            "pushd",
            "popd",
            "dirs",
            "alias",
            "unalias",
            "export",
            "unset",
            "path",
            "hash",
            "type",
            "which",
            "command",
            "builtin",
            "jobs",
            "fg",
            "bg",
            "wait",
            "disown",
            "kill",
            "history",
            "echo",
            "printf",
            "read",
            "umask",
            "ulimit",
            "help",
            "exec",
            "logout",
            "source",
            "deactivate",
            "repl",
            "reload",
            "pkg",
            "exit",
        ];
        for name in expected {
            assert!(registry.find(name).is_some(), "missing {name}");
        }
        assert!(registry.find(".").is_some(), "missing source shorthand");
        assert!(registry.find("cd").unwrap().mutates_shell_state);
        assert!(registry.find("source").unwrap().mutates_shell_state);
        assert!(registry.find("deactivate").unwrap().mutates_shell_state);
        assert!(!registry.find("which").unwrap().mutates_shell_state);
        assert_eq!(registry.metadata().count(), expected.len());
    }
}
