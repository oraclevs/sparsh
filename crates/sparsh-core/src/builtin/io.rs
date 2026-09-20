use std::io::BufRead;

use super::{error, status_output, success, usage_error, BuiltinContext, BuiltinRegistry, BuiltinResult};

pub(super) fn echo(args: &[String], _: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let (newline, words) = if args.first().is_some_and(|arg| arg == "-n") {
        (false, &args[1..])
    } else {
        (true, args)
    };
    let mut bytes = words.join(" ").into_bytes();
    if newline { bytes.push(b'\n'); }
    Ok(status_output(0, bytes))
}

pub(super) fn printf(args: &[String], _: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let Some(format) = args.first() else { return Err(usage_error("printf format [argument ...]")); };
    let mut arguments = args[1..].iter();
    let mut output = Vec::new();
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('n') => output.push(b'\n'),
                Some('t') => output.push(b'\t'),
                Some('r') => output.push(b'\r'),
                Some('\\') => output.push(b'\\'),
                Some(other) => output.extend_from_slice(other.to_string().as_bytes()),
                None => output.push(b'\\'),
            },
            '%' => {
                let Some(spec) = chars.next() else { return Err(error("printf: trailing '%'")); };
                if spec == '%' { output.push(b'%'); continue; }
                let value = arguments.next().map(String::as_str).unwrap_or("");
                let rendered = match spec {
                    's' => value.to_string(),
                    'd' | 'i' => value.parse::<i64>().map_err(|_| error(format!("printf: expected integer: {value}")))?.to_string(),
                    'u' => value.parse::<u64>().map_err(|_| error(format!("printf: expected unsigned integer: {value}")))?.to_string(),
                    'x' => format!("{:x}", value.parse::<u64>().map_err(|_| error(format!("printf: expected integer: {value}")))?),
                    'X' => format!("{:X}", value.parse::<u64>().map_err(|_| error(format!("printf: expected integer: {value}")))?),
                    'o' => format!("{:o}", value.parse::<u64>().map_err(|_| error(format!("printf: expected integer: {value}")))?),
                    'c' => value.chars().next().unwrap_or('\0').to_string(),
                    other => return Err(super::BuiltinError { message: format!("printf: unsupported conversion %{other}"), status: 2 }),
                };
                output.extend_from_slice(rendered.as_bytes());
            }
            other => output.extend_from_slice(other.to_string().as_bytes()),
        }
    }
    Ok(status_output(0, output))
}

pub(super) fn read(args: &[String], context: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    let (raw, name) = match args {
        [name] => (false, name.as_str()),
        [flag, name] if flag == "-r" => (true, name.as_str()),
        _ => return Err(usage_error("read [-r] NAME")),
    };
    if !super::valid_environment_name(name) {
        return Err(error(format!("read: invalid environment name: {name}")));
    }
    let mut line = if context.stdin_available {
        let mut cursor = std::io::Cursor::new(&context.stdin);
        let mut line = String::new();
        cursor.read_line(&mut line).map_err(|e| error(format!("read: {e}")))?;
        line
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map_err(|e| error(format!("read: {e}")))?;
        line
    };
    while matches!(line.as_bytes().last(), Some(b'\n' | b'\r')) { line.pop(); }
    if !raw {
        line = unescape_backslashes(&line);
    }
    context.services.environment.set_os(name, line);
    if name == "PATH" { context.services.reload_path_from_environment(); }
    Ok(success(None))
}

fn unescape_backslashes(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() { out.push(next); }
        } else { out.push(ch); }
    }
    out
}
