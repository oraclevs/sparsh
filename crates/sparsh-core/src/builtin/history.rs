use super::{error, success, usage_error, BuiltinContext, BuiltinRegistry, BuiltinResult};

pub(super) fn history(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let Some(history) = context.services.history.as_ref() else {
        return Ok(success(None));
    };
    if args == ["-c"] {
        history.clear().map_err(error)?;
        return Ok(success(None));
    }
    let limit = match args {
        [] => None,
        [value] => Some(value.parse::<usize>().map_err(|_| usage_error("history [N|-c]"))?),
        _ => return Err(usage_error("history [N|-c]")),
    };
    let entries = history.list(limit).map_err(error)?;
    let mut text = String::new();
    for (index, entry) in entries.iter().enumerate() {
        text.push_str(&format!("{:>5}  {}\n", index + 1, entry));
    }
    Ok(success(Some(text)))
}
