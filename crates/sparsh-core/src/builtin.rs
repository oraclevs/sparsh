use std::path::{Path, PathBuf};

use crate::resolver::ResolutionMode;
use crate::services::ShellServices;

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
    pub stdout: Option<String>,
    pub status: i32,
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
    pub resolution_mode: ResolutionMode,
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
    ($name:literal, $description:literal, $usage:literal, $category:literal, $mutates:literal, $output:literal, $handler:ident) => {
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

fn wrapper(_: &[String], _: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    Err(error("internal wrapper dispatch failure"))
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

type BuiltinResult = Result<BuiltinOutput, BuiltinError>;

fn success(stdout: Option<String>) -> BuiltinOutput {
    BuiltinOutput { stdout, status: 0 }
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
        for name in [
            "cd", "pwd", "pushd", "popd", "dirs", "alias", "unalias", "export", "unset", "path",
            "hash", "type", "which", "command", "builtin", "exit",
        ] {
            assert!(registry.find(name).is_some(), "missing {name}");
        }
        assert!(registry.find("cd").unwrap().mutates_shell_state);
        assert!(!registry.find("which").unwrap().mutates_shell_state);
        assert_eq!(registry.metadata().count(), 16);
    }
}
