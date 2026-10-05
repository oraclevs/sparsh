use super::{error, success, usage_error, BuiltinContext, BuiltinRegistry, BuiltinResult};

pub(super) fn history(
    args: &[String],
    context: &mut BuiltinContext<'_>,
    _: &BuiltinRegistry,
) -> BuiltinResult {
    let Some(history) = context.services.history.as_ref() else {
        return Err(error("history is available in interactive sessions"));
    };
    const USAGE: &str = "history [N|--search TEXT|--delete LINE|--delete-matching TEXT|--delete-exact COMMAND|--clear]";
    match args {
        [flag] if flag == "--clear" || flag == "-c" => {
            history.clear().map_err(error)?;
            return Ok(success(Some("history cleared\n".into())));
        }
        [flag, number] if flag == "--delete" => {
            let line = number.parse::<usize>().map_err(|_| usage_error(USAGE))?;
            if line == 0 || history.delete_line(line).map_err(error)? == 0 {
                return Err(error("history line does not exist"));
            }
            return Ok(success(Some("history entry removed\n".into())));
        }
        [flag, query] if flag == "--delete-matching" || flag == "--delete-exact" => {
            if query.is_empty() {
                return Err(usage_error(USAGE));
            }
            let count = history
                .delete_matching(query, flag == "--delete-exact")
                .map_err(error)?;
            return Ok(success(Some(format!("removed {count} history entries\n"))));
        }
        [flag, query] if flag == "--search" => {
            if query.is_empty() {
                return Err(usage_error(USAGE));
            }
            let entries = history.list(None).map_err(error)?;
            return Ok(success(Some(format_entries(
                entries
                    .into_iter()
                    .filter(|(_, command)| command.contains(query)),
            ))));
        }
        _ => {}
    }
    let limit = match args {
        [] => None,
        [value] => Some(value.parse::<usize>().map_err(|_| usage_error(USAGE))?),
        _ => return Err(usage_error(USAGE)),
    };
    Ok(success(Some(format_entries(
        history.list(limit).map_err(error)?.into_iter(),
    ))))
}

fn format_entries(entries: impl Iterator<Item = (usize, String)>) -> String {
    let mut text = String::new();
    for (number, entry) in entries {
        text.push_str(&format!("{number:>5}  {entry}\n"));
    }
    text
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
    let state = if history.stealth_mode().map_err(error)? {
        "on"
    } else {
        "off"
    };
    Ok(success(Some(format!("stealth mode: {state}\n"))))
}
