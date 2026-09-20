use super::{error, success, usage_error, BuiltinContext, BuiltinRegistry, BuiltinResult, SourceRequest};

pub(super) fn help(args: &[String], _: &mut BuiltinContext<'_>, registry: &BuiltinRegistry) -> BuiltinResult {
    match args {
        [] => {
            let mut entries = registry.metadata().collect::<Vec<_>>();
            entries.sort_by_key(|entry| (entry.category, entry.name));
            let mut out = String::new();
            let mut category = "";
            for entry in entries {
                if entry.category != category {
                    category = entry.category;
                    if !out.is_empty() { out.push('\n'); }
                    out.push_str(category);
                    out.push_str(":\n");
                }
                out.push_str(&format!("  {:<10} {}\n", entry.name, entry.description));
            }
            Ok(success(Some(out)))
        }
        [name] => {
            let entry = registry.find(name).ok_or_else(|| error(format!("help: no such builtin: {name}")))?;
            Ok(success(Some(format!("{} — {}\nusage: {}\n", entry.name, entry.description, entry.usage))))
        }
        _ => Err(usage_error("help [builtin]")),
    }
}

pub(super) fn logout(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if !args.is_empty() { return Err(usage_error("logout")); }
    if !context.login_shell { return Err(error("logout: not a login shell")); }
    context.requested_exit = Some(context.last_status);
    Ok(success(None))
}

pub(super) fn exec(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    if args.is_empty() { return Err(usage_error("exec command [argument ...]")); }
    context.requested_exec = Some(args.to_vec());
    Ok(success(None))
}

pub(super) fn source(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let (shell, path) = match args {
        [path] => (None, path.clone()),
        [flag, shell, path] if flag == "--shell" => (Some(shell.clone()), path.clone()),
        _ => return Err(usage_error("source [--shell bash|zsh|sh] FILE")),
    };
    context.requested_source = Some(SourceRequest { path, shell });
    Ok(success(None))
}
