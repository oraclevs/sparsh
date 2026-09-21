#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Dispatch<'a> {
    Empty,
    SparFragment(&'a str),
    SparValue(&'a str),
    Command(&'a str),
}

pub(crate) fn classify<'a>(input: &'a str, session: &spar::Session) -> Dispatch<'a> {
    let input = input.trim();
    if input.is_empty() {
        return Dispatch::Empty;
    }
    if input.contains("|>")
        || is_explicit_spar_construct(input)
        || is_explicit_call(input)
        || is_previous_value_access(input)
        || input.starts_with("(await")
    {
        return Dispatch::SparFragment(input);
    }
    if let Some((left, right)) = input.split_once('=') {
        let name = left.trim();
        if !right.starts_with('=') && is_identifier(name) && session.value(name).is_some() {
            return Dispatch::SparFragment(input);
        }
    }
    if is_identifier(input) && session.value(input).is_some() {
        return Dispatch::SparValue(input);
    }
    Dispatch::Command(input)
}

/// `_.field`, `_[0]`, `_.method()`: member access on the previous result.
fn is_previous_value_access(input: &str) -> bool {
    input
        .strip_prefix('_')
        .is_some_and(|rest| rest.starts_with(['.', '[']))
}

fn is_explicit_spar_construct(input: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "var",
        "function",
        "functionGroup",
        "struct",
        "type",
        "enum",
        "import",
        "dynamic",
        "async",
        "if",
        "for",
        "shell",
        "await",
    ];
    if KEYWORDS
        .iter()
        .any(|keyword| begins_with_word(input, keyword))
    {
        return true;
    }
    // `exec { ... }` / `exec shell { ... }` is Spar; `exec printf ok` is the
    // process-replacing builtin and stays a command.
    if begins_with_word(input, "exec") {
        let rest = input["exec".len()..].trim_start();
        if rest.starts_with('{') || begins_with_word(rest, "shell") {
            return true;
        }
    }
    if let Some(rest) = input.strip_prefix("private") {
        if rest.starts_with(char::is_whitespace) {
            let rest = rest.trim_start();
            if ["function", "functionGroup", "struct"]
                .iter()
                .any(|keyword| begins_with_word(rest, keyword))
                || rest.starts_with('[')
            {
                return true;
            }
        }
    }

    let Some(rest) = input.strip_prefix("export") else {
        return false;
    };
    if rest.is_empty() || !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let rest = rest.trim_start();
    ["var", "function", "type", "enum"]
        .iter()
        .any(|keyword| begins_with_word(rest, keyword))
}

fn begins_with_word(input: &str, word: &str) -> bool {
    input == word
        || input
            .strip_prefix(word)
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

pub(crate) fn is_explicit_call(input: &str) -> bool {
    let input = input.strip_suffix(';').unwrap_or(input).trim_end();
    let Some(open) = input.find('(') else {
        return false;
    };
    if !input.ends_with(')') {
        return false;
    }
    let name = input[..open].trim();
    !name.is_empty() && name.split("::").all(is_identifier)
}

/// A real shell byte pipe feeding Spar's explicit decoder (`... | from FORMAT`).
/// Quoted `| from` text is ignored so ordinary command arguments cannot be
/// misclassified as mixed pipelines.
pub(crate) fn is_mixed_byte_pipeline(input: &str) -> bool {
    let input = input.trim();
    if begins_with_word(input, "shell") {
        return false;
    }

    let bytes = input.as_bytes();
    let mut quote = None::<u8>;
    let mut escaped = false;
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }

        if let Some(active_quote) = quote {
            if active_quote == b'"' && byte == b'\\' {
                escaped = true;
            } else if byte == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }

        match byte {
            b'\'' | b'"' => {
                quote = Some(byte);
                index += 1;
            }
            b'\\' => {
                escaped = true;
                index += 1;
            }
            b'|' => {
                let previous_is_pipe = index > 0 && bytes[index - 1] == b'|';
                let next = bytes.get(index + 1).copied();
                if previous_is_pipe || matches!(next, Some(b'|' | b'>')) {
                    index += 1;
                    continue;
                }

                let mut cursor = index + 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                let from_end = cursor.saturating_add(4);
                if bytes.get(cursor..from_end) == Some(&b"from"[..])
                    && bytes.get(from_end).is_none_or(u8::is_ascii_whitespace)
                {
                    return true;
                }
                index += 1;
            }
            _ => index += 1,
        }
    }

    false
}

pub(crate) fn is_bare_identifier(value: &str) -> bool {
    is_identifier(value)
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_alphabetic())
        && characters.all(|character| character == '_' || character.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::{classify, Dispatch};

    #[test]
    fn known_bare_variable_is_a_value_but_unknown_bare_word_is_a_command() {
        let mut session = spar::Engine::default().session();
        session.eval("var project: str = \"spar\";").unwrap();

        assert_eq!(
            classify("project", &session),
            Dispatch::SparValue("project")
        );
        assert_eq!(classify("build", &session), Dispatch::Command("build"));
    }

    #[test]
    fn declarations_and_explicit_calls_are_spar() {
        let session = spar::Engine::default().session();

        for input in [
            "var project: str = \"spar\";",
            "function build() -> int { return 0; };",
            "private function hidden() -> int { return 0; };",
            "functionGroup Rust {}",
            "struct Human { name: str = \"OCC\"; };",
            "type Human = { name: str; };",
            "enum Mode { Fast; }",
            "import \"tools.spar\" as tools;",
            "export var public: int = 1;",
            "build()",
            "Rust::build();",
            "shell { echo hello; }",
            "exec { printf hello; }",
            "await get(url: \"http://localhost\")",
            "(await get(url: \"http://localhost\")).json()",
            "_.status",
            "_[0]",
        ] {
            assert_eq!(classify(input, &session), Dispatch::SparFragment(input));
        }
    }

    #[test]
    fn byte_pipe_into_from_is_a_mixed_shell_pipeline() {
        assert!(super::is_mixed_byte_pipeline("printf x | from jsonl"));
        assert!(super::is_mixed_byte_pipeline(
            "printf x | from jsonl |> take(1) |> to jsonl"
        ));
        assert!(!super::is_mixed_byte_pipeline("users |> take(2)"));
        assert!(!super::is_mixed_byte_pipeline("printf x | cat"));
        assert!(!super::is_mixed_byte_pipeline("printf '%s\n' '| from csv'"));
        assert!(!super::is_mixed_byte_pipeline("printf \"| from csv\""));
        assert!(!super::is_mixed_byte_pipeline("printf x || from csv"));
        assert!(super::is_mixed_byte_pipeline("printf 'a|b' | from csv"));
        assert!(super::is_mixed_byte_pipeline(
            "shellcheck report.txt | from lines"
        ));
        assert!(!super::is_mixed_byte_pipeline(
            "shell { printf x | from jsonl |> to jsonl; }"
        ));
    }

    #[test]
    fn structured_value_pipelines_are_dispatched_to_spar() {
        let session = spar::Engine::default().session();

        assert_eq!(
            classify("users |> take(2)", &session),
            Dispatch::SparFragment("users |> take(2)")
        );
        assert_eq!(
            classify("_ |> inspect()", &session),
            Dispatch::SparFragment("_ |> inspect()")
        );
    }

    #[test]
    fn assignment_is_spar_only_when_the_left_identifier_already_exists() {
        let mut session = spar::Engine::default().session();
        session.eval("var mut count: int = 0;").unwrap();

        assert_eq!(
            classify("count = count + 1;", &session),
            Dispatch::SparFragment("count = count + 1;")
        );
        assert_eq!(
            classify("MISSING=value", &session),
            Dispatch::Command("MISSING=value")
        );
        assert_eq!(
            classify("count == 1", &session),
            Dispatch::Command("count == 1")
        );
    }

    #[test]
    fn ordinary_command_lines_remain_commands() {
        let session = spar::Engine::default().session();

        assert_eq!(
            classify("git status", &session),
            Dispatch::Command("git status")
        );
        assert_eq!(
            classify(" echo project ", &session),
            Dispatch::Command("echo project")
        );
        assert_eq!(classify("   ", &session), Dispatch::Empty);
    }
}
