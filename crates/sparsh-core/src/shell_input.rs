//! Command-line continuations and here-documents for the interactive shell.
//! Here-document bodies stay in memory and are passed as exact stdin bytes.

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ShellInput {
    pub command: String,
    pub stdin: Option<Vec<u8>>,
}

pub(crate) fn prepare(source: &str) -> Result<ShellInput, String> {
    let Some(first_newline) = source.find('\n') else {
        let (_, delimiter) = take_here_document_header(source)?;
        return if delimiter.is_some() {
            Err("unterminated here-document".into())
        } else {
            Ok(ShellInput {
                command: join_continuations(source),
                stdin: None,
            })
        };
    };
    let header = &source[..first_newline];
    let (command, delimiter) = take_here_document_header(header)?;
    let Some((marker, strip_tabs)) = delimiter else {
        return Ok(ShellInput {
            command: join_continuations(source),
            stdin: None,
        });
    };
    let mut body = Vec::new();
    let mut found = false;
    let mut remainder = &source[first_newline + 1..];
    while !remainder.is_empty() {
        let (line, rest) = match remainder.split_once('\n') {
            Some((line, rest)) => (line, rest),
            None => (remainder, ""),
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        let comparison = if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line
        };
        if comparison == marker {
            found = true;
            if !rest.trim().is_empty() {
                return Err("text after here-document terminator is not supported".into());
            }
            break;
        }
        body.extend_from_slice(comparison.as_bytes());
        body.push(b'\n');
        remainder = rest;
    }
    if !found {
        return Err(format!("unterminated here-document: expected {marker}"));
    }
    Ok(ShellInput {
        command: join_continuations(&command),
        stdin: Some(body),
    })
}

pub(crate) fn needs_more_input(source: &str) -> bool {
    if source.trim_end_matches([' ', '\t', '\r']).ends_with('\\')
        || source.ends_with("\\\n")
        || source.ends_with("\\\r\n")
    {
        return true;
    }
    matches!(prepare(source), Err(message) if message.starts_with("unterminated here-document"))
}

fn join_continuations(source: &str) -> String {
    source.replace("\\\r\n", "").replace("\\\n", "")
}

fn take_here_document_header(header: &str) -> Result<(String, Option<(String, bool)>), String> {
    let bytes = header.as_bytes();
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index + 1 < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if byte == b'\\' && quote != Some(b'\'') {
            escaped = true;
            index += 1;
            continue;
        }
        if let Some(active) = quote {
            if byte == active {
                quote = None;
            }
            index += 1;
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if byte != b'<' || bytes[index + 1] != b'<' {
            index += 1;
            continue;
        }
        if bytes.get(index + 2) == Some(&b'<') {
            return Err("here-strings (<<<) are not supported; use a here-document".into());
        }
        let mut end = index + 2;
        let strip_tabs = bytes.get(end) == Some(&b'-');
        if strip_tabs {
            end += 1;
        }
        while bytes.get(end).is_some_and(u8::is_ascii_whitespace) {
            end += 1;
        }
        let quoted = bytes
            .get(end)
            .filter(|&&b| b == b'\'' || b == b'"')
            .copied();
        if let Some(q) = quoted {
            end += 1;
            let start = end;
            while bytes.get(end).is_some_and(|b| *b != q) {
                end += 1;
            }
            if bytes.get(end) != Some(&q) {
                return Err("unterminated here-document delimiter quote".into());
            }
            let marker = header[start..end].to_string();
            end += 1;
            if marker.is_empty() {
                return Err("missing here-document delimiter".into());
            }
            let command = format!("{}{}", &header[..index], &header[end..]);
            return Ok((command, Some((marker, strip_tabs))));
        }
        let start = end;
        while bytes.get(end).is_some_and(|b| {
            !b.is_ascii_whitespace() && !matches!(b, b'|' | b'&' | b';' | b'<' | b'>')
        }) {
            end += 1;
        }
        if start == end {
            return Err("missing here-document delimiter".into());
        }
        let marker = header[start..end].to_string();
        let command = format!("{}{}", &header[..index], &header[end..]);
        return Ok((command, Some((marker, strip_tabs))));
    }
    Ok((header.to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_python_document_is_exact_stdin() {
        let parsed = prepare("python - <<'PY' > result.txt\nprint('hello')\nPY\n").unwrap();
        assert_eq!(parsed.command, "python -  > result.txt");
        assert_eq!(parsed.stdin.unwrap(), b"print('hello')\n");
    }
    #[test]
    fn continuation_joins_command_words() {
        assert_eq!(prepare("echo foo\\\nbar").unwrap().command, "echo foobar");
        assert!(needs_more_input("echo foo\\"));
    }
    #[test]
    fn ignores_quoted_operator_and_rejects_unfinished_body() {
        assert_eq!(prepare("echo '<<PY'").unwrap().stdin, None);
        assert!(needs_more_input("python - <<PY\nprint(1)\n"));
    }
}
