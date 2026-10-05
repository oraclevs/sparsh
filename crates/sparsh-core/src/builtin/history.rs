use super::{error, success, usage_error, BuiltinContext, BuiltinRegistry, BuiltinResult};

pub(super) fn history(
    args: &[String],
    context: &mut BuiltinContext<'_>,
    _: &BuiltinRegistry,
) -> BuiltinResult {
    let Some(history) = context.services.history.as_ref() else {
        return Ok(success(None));
    };
    if args == ["-c"] {
        history.clear().map_err(error)?;
        return Ok(success(None));
    }
    let limit = match args {
        [] => None,
        [value] => Some(
            value
                .parse::<usize>()
                .map_err(|_| usage_error("history [N|-c]"))?,
        ),
        _ => return Err(usage_error("history [N|-c]")),
    };
    let entries = history.list(limit).map_err(error)?;
    let mut text = String::new();
    for (index, entry) in entries.iter().enumerate() {
        text.push_str(&format!("{:>5}  {}\n", index + 1, entry));
    }
    Ok(success(Some(text)))
}

pub(super) fn stealth(
    args: &[String],
    context: &mut BuiltinContext<'_>,
    _: &BuiltinRegistry,
) -> BuiltinResult {
    let Some(history) = context.services.history.as_ref() else {
        return Err(error("stealth is available in interactive sessions"));
    };
    match args {
        [value] if value == "on" => history.set_stealth_mode(true).map_err(error)?,
        [value] if value == "off" => history.set_stealth_mode(false).map_err(error)?,
        [] => {}
        [value] if value == "status" => {}
        _ => return Err(usage_error("stealth [on|off|status]")),
    }
    let state = if history.stealth_mode().map_err(error)? { "on" } else { "off" };
    Ok(success(Some(format!("stealth mode: {state}\n"))))
}
