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
    if is_explicit_spar_construct(input) || is_explicit_call(input) {
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

fn is_explicit_spar_construct(input: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "var",
        "function",
        "functionGroup",
        "type",
        "enum",
        "import",
        "if",
        "for",
    ];
    if KEYWORDS
        .iter()
        .any(|keyword| begins_with_word(input, keyword))
    {
        return true;
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

fn is_explicit_call(input: &str) -> bool {
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
            "functionGroup Rust {}",
            "type Human = { name: str; };",
            "enum Mode { Fast; }",
            "import \"tools.spar\" as tools;",
            "export var public: int = 1;",
            "build()",
            "Rust::build();",
        ] {
            assert_eq!(classify(input, &session), Dispatch::SparFragment(input));
        }
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
