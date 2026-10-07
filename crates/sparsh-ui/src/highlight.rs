use std::ops::Range;
use std::sync::{Arc, RwLock};

use nu_ansi_term::Style;
use reedline::{Highlighter, StyledText};
use sparsh_core::{CommandKind, ShellUiSnapshot};

use crate::theme::{SemanticRole, Theme};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub role: SemanticRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperatorKind {
    StructuredPipe,
    ShellPipe,
    CommandSeparator,
    Expression,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MixedHighlightState {
    Shell,
    AfterBytePipe,
    DecoderFormat,
    Structured,
    AfterStructuredPipe,
    EncoderFormat,
    ByteOutput,
}

pub fn scan(line: &str, snapshot: &ShellUiSnapshot) -> Vec<HighlightSpan> {
    // `~ cmd` is a Spar statement marker: color the `~` as syntax and the
    // rest of the line as a shell command line.
    let indent = line.len() - line.trim_start().len();
    let trimmed = &line[indent..];
    if let Some(rest) = trimmed.strip_prefix('~') {
        if rest.starts_with(char::is_whitespace) {
            let offset = indent + 1;
            let mut spans = vec![HighlightSpan {
                range: indent..offset,
                role: SemanticRole::SparSyntax,
            }];
            for mut span in shell_scan(&line[offset..], snapshot) {
                span.range = span.range.start + offset..span.range.end + offset;
                spans.push(span);
            }
            return spans;
        }
    }
    if looks_like_spar(line) {
        if let Some(spans) = spar_scan(line, snapshot) {
            return spans;
        }
    }
    shell_scan(line, snapshot)
}

/// Spar source typed at the prompt: a declaration or control-flow keyword, a
/// call like `name(`, an assignment, or a comment. Shell command lines and
/// mixed `|>` pipelines stay on the shell scanner.
fn looks_like_spar(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return true;
    }
    let first: String = trimmed
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    if first.is_empty() {
        return false;
    }
    if is_spar_keyword(&first) {
        return true;
    }
    let rest = &trimmed[first.len()..];
    if rest.starts_with('(') {
        return true;
    }
    let rest = rest.trim_start();
    rest.starts_with('=') && !rest.starts_with("==")
}

/// Highlights a line from the real Spar token stream. `None` means the lexer
/// could not read the line (a half-typed string, say) and the caller should
/// fall back to the word scanner.
fn spar_scan(line: &str, snapshot: &ShellUiSnapshot) -> Option<Vec<HighlightSpan>> {
    use spar::token::Token;

    let (tokens, comments) = spar::Lexer::new(line).tokenize_with_comments().ok()?;
    let mut spans: Vec<HighlightSpan> = Vec::new();
    let mut paren_depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        let (start, end) = (token.span.start, token.span.end);
        if end <= start || end > line.len() || !line.is_char_boundary(start) || !line.is_char_boundary(end) {
            continue;
        }
        let text = &line[start..end];
        let next = tokens.get(index + 1).map(|next| &next.token);
        let role = match &token.token {
            Token::IntLit(_) | Token::FloatLit(_) => SemanticRole::DataNumber,
            Token::True | Token::False => SemanticRole::DataBool,
            // The lexer's spans for string pieces are not reliable; strings
            // are painted from their regions below.
            Token::StringStart
            | Token::StringFragment(_)
            | Token::StringEnd
            | Token::InterpolStart
            | Token::InterpolEnd
            | Token::CommandSubStart
            | Token::CommandSubEnd => continue,
            Token::TypeStr
            | Token::TypeInt
            | Token::TypeFloat
            | Token::TypeBool
            | Token::TypeVoid
            | Token::TypeShell => SemanticRole::TypeName,
            Token::Ident(name) => {
                let called = matches!(next, Some(Token::LParen));
                let named = paren_depth > 0 && matches!(next, Some(Token::Colon));
                if matches!(name.as_str(), "null" | "None") {
                    SemanticRole::DataNull
                } else if called && snapshot.has_function(name) {
                    SemanticRole::Function
                } else if named {
                    SemanticRole::Parameter
                } else if name.chars().next().is_some_and(char::is_uppercase) {
                    SemanticRole::TypeName
                } else {
                    SemanticRole::Argument
                }
            }
            _ if text.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') => {
                if matches!(
                    spar::token::keyword_or_ident(text.to_string()),
                    Token::Ident(_)
                ) {
                    SemanticRole::Argument
                } else {
                    SemanticRole::SparSyntax
                }
            }
            _ => SemanticRole::Operator,
        };
        match text {
            "(" => paren_depth += 1,
            ")" => paren_depth = paren_depth.saturating_sub(1),
            _ => {}
        }
        spans.push(HighlightSpan {
            range: start..end,
            role,
        });
    }
    for comment in comments {
        let end = comment.start + comment.text.len();
        if end <= line.len() && line.is_char_boundary(comment.start) && line.is_char_boundary(end)
        {
            spans.push(HighlightSpan {
                range: comment.start..end,
                role: SemanticRole::Comment,
            });
        }
    }
    // Strings: paint the quotes and text, keep only the expressions inside
    // `${...}` from the lexer.
    let regions = string_regions(line);
    spans.retain(|span| {
        regions.iter().all(|region| {
            let inside = span.range.start >= region.range.start && span.range.end <= region.range.end;
            !inside
                || region
                    .inner
                    .iter()
                    .any(|inner| span.range.start >= inner.start && span.range.end <= inner.end)
        })
    });
    for region in regions {
        spans.extend(region.pieces);
    }
    spans.sort_by_key(|span| span.range.start);
    // A lexer token must never overlap the next: keep the first of any pair.
    let mut last_end = 0;
    spans.retain(|span| {
        let keep = span.range.start >= last_end;
        if keep {
            last_end = span.range.end;
        }
        keep
    });
    Some(spans)
}

struct StringRegion {
    range: Range<usize>,
    /// Byte ranges of the expressions inside `${...}`.
    inner: Vec<Range<usize>>,
    /// Quotes, text and the `${` / `}` delimiters, already in order.
    pieces: Vec<HighlightSpan>,
}

/// The `"..."` literals of a Spar line, split around `${...}` interpolation.
fn string_regions(line: &str) -> Vec<StringRegion> {
    let bytes = line.as_bytes();
    let mut regions = Vec::new();
    let mut index = 0;
    while index < line.len() {
        if line[index..].starts_with("//") {
            break;
        }
        if bytes[index] != b'"' {
            index += line[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let end = quoted_end(line, index, '"');
        let mut pieces = Vec::new();
        let mut inner = Vec::new();
        let mut text_start = index;
        let mut cursor = index + 1;
        while cursor < end {
            if line[cursor..end].starts_with("${") {
                if text_start < cursor {
                    pieces.push(HighlightSpan {
                        range: text_start..cursor,
                        role: SemanticRole::QuotedString,
                    });
                }
                pieces.push(HighlightSpan {
                    range: cursor..cursor + 2,
                    role: SemanticRole::Operator,
                });
                let inner_start = cursor + 2;
                let mut depth = 1;
                let mut close = inner_start;
                while close < end {
                    match bytes[close] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    close += 1;
                }
                inner.push(inner_start..close.min(end));
                if close < end {
                    pieces.push(HighlightSpan {
                        range: close..close + 1,
                        role: SemanticRole::Operator,
                    });
                    cursor = close + 1;
                } else {
                    cursor = end;
                }
                text_start = cursor;
            } else {
                cursor += line[cursor..].chars().next().map_or(1, char::len_utf8);
            }
        }
        if text_start < end {
            pieces.push(HighlightSpan {
                range: text_start..end,
                role: SemanticRole::QuotedString,
            });
        }
        regions.push(StringRegion {
            range: index..end,
            inner,
            pieces,
        });
        index = end.max(index + 1);
    }
    regions
}

fn shell_scan(line: &str, snapshot: &ShellUiSnapshot) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let mut index = 0;
    let mut command_position = true;
    let mut active_command: Option<String> = None;
    let mut paren_depth = 0usize;
    let mut mixed_state = MixedHighlightState::Shell;

    while index < line.len() {
        let character = line[index..]
            .chars()
            .next()
            .expect("index is inside the input");
        if character.is_whitespace() {
            index += character.len_utf8();
            continue;
        }

        if matches!(character, '\'' | '"') {
            let end = quoted_end(line, index, character);
            let role = if mixed_state == MixedHighlightState::Shell
                && active_command.as_deref().is_some_and(command_takes_path)
            {
                SemanticRole::Path
            } else {
                SemanticRole::QuotedString
            };
            spans.push(HighlightSpan {
                range: index..end,
                role,
            });
            command_position = false;
            index = end;
            continue;
        }

        if let Some((length, kind)) = operator_at(line, index) {
            spans.push(HighlightSpan {
                range: index..index + length,
                role: SemanticRole::Operator,
            });
            match kind {
                OperatorKind::StructuredPipe => {
                    mixed_state = MixedHighlightState::AfterStructuredPipe;
                    active_command = None;
                }
                OperatorKind::ShellPipe => {
                    mixed_state = MixedHighlightState::AfterBytePipe;
                    active_command = None;
                }
                OperatorKind::CommandSeparator => {
                    mixed_state = MixedHighlightState::Shell;
                    command_position = true;
                    active_command = None;
                }
                OperatorKind::Expression => {}
            }
            index += length;
            continue;
        }

        if matches!(character, '(' | ')' | ',' | ':') {
            spans.push(HighlightSpan {
                range: index..index + character.len_utf8(),
                role: SemanticRole::Operator,
            });
            match character {
                '(' => paren_depth += 1,
                ')' => paren_depth = paren_depth.saturating_sub(1),
                _ => {}
            }
            index += character.len_utf8();
            continue;
        }

        let end = word_end(line, index);
        let word = &line[index..end];
        let next = next_non_whitespace_char(line, end);
        let function_call = next == Some('(') && snapshot.has_function(word);
        let named_parameter = paren_depth > 0 && next == Some(':');

        let role = match mixed_state {
            MixedHighlightState::AfterBytePipe if word == "from" => {
                mixed_state = MixedHighlightState::DecoderFormat;
                active_command = None;
                SemanticRole::SparSyntax
            }
            MixedHighlightState::AfterBytePipe => {
                mixed_state = MixedHighlightState::Shell;
                let role = command_role(snapshot, word);
                active_command = Some(word.to_string());
                role
            }
            MixedHighlightState::DecoderFormat => {
                mixed_state = MixedHighlightState::Structured;
                if is_structured_codec(word) {
                    SemanticRole::Argument
                } else {
                    SemanticRole::Warning
                }
            }
            MixedHighlightState::AfterStructuredPipe if word == "to" => {
                mixed_state = MixedHighlightState::EncoderFormat;
                SemanticRole::SparSyntax
            }
            MixedHighlightState::AfterStructuredPipe => {
                mixed_state = MixedHighlightState::Structured;
                structured_word_role(word, function_call, named_parameter)
            }
            MixedHighlightState::Structured => {
                structured_word_role(word, function_call, named_parameter)
            }
            MixedHighlightState::EncoderFormat => {
                mixed_state = MixedHighlightState::ByteOutput;
                if is_structured_codec(word) {
                    SemanticRole::Argument
                } else {
                    SemanticRole::Warning
                }
            }
            MixedHighlightState::ByteOutput => {
                if word.starts_with('-') {
                    SemanticRole::Option
                } else {
                    SemanticRole::Argument
                }
            }
            MixedHighlightState::Shell if function_call => SemanticRole::Function,
            MixedHighlightState::Shell if named_parameter => SemanticRole::Parameter,
            MixedHighlightState::Shell if command_position && is_spar_keyword(word) => {
                SemanticRole::SparSyntax
            }
            MixedHighlightState::Shell if command_position => {
                let role = command_role(snapshot, word);
                active_command = Some(word.to_string());
                role
            }
            MixedHighlightState::Shell
                if active_command.as_deref().is_some_and(command_takes_path)
                    && !word.starts_with('-') =>
            {
                SemanticRole::Path
            }
            MixedHighlightState::Shell if word.starts_with('-') => SemanticRole::Option,
            MixedHighlightState::Shell => SemanticRole::Argument,
        };

        spans.push(HighlightSpan {
            range: index..end,
            role,
        });
        command_position = false;
        index = end;
    }

    spans
}

fn command_role(snapshot: &ShellUiSnapshot, word: &str) -> SemanticRole {
    if crate::editor::HOST_COMMANDS.contains(&word) {
        return SemanticRole::Builtin;
    }
    match snapshot.classify_command(word) {
        CommandKind::Builtin => SemanticRole::Builtin,
        CommandKind::Alias => SemanticRole::Alias,
        CommandKind::External => SemanticRole::ExternalCommand,
        CommandKind::Unknown => SemanticRole::UnknownCommand,
    }
}

fn structured_word_role(word: &str, function_call: bool, named_parameter: bool) -> SemanticRole {
    if function_call {
        SemanticRole::Function
    } else if named_parameter {
        SemanticRole::Parameter
    } else {
        match word {
            "fn" | "if" | "else" | "for" | "while" | "loop" | "break" | "continue" | "const"
            | "return" | "mut" => SemanticRole::SparSyntax,
            "true" | "false" => SemanticRole::DataBool,
            "null" | "None" => SemanticRole::DataNull,
            _ => SemanticRole::Argument,
        }
    }
}

fn is_structured_codec(word: &str) -> bool {
    matches!(
        word,
        "json" | "jsonl" | "csv" | "tsv" | "yaml" | "toml" | "lines" | "text"
    )
}

pub(crate) fn paint_range(
    line: &str,
    range: Range<usize>,
    snapshot: &ShellUiSnapshot,
    theme: &Theme,
) -> String {
    let start = range.start.min(line.len());
    let end = range.end.min(line.len());
    if start >= end || !line.is_char_boundary(start) || !line.is_char_boundary(end) {
        return String::new();
    }

    let mut rendered = String::new();
    let mut cursor = start;
    for span in scan(line, snapshot) {
        let span_start = span.range.start.max(start);
        let span_end = span.range.end.min(end);
        if span_start >= span_end {
            continue;
        }
        if cursor < span_start {
            rendered.push_str(&line[cursor..span_start]);
        }
        rendered.push_str(&theme.paint(span.role, &line[span_start..span_end]));
        cursor = span_end;
    }
    if cursor < end {
        rendered.push_str(&line[cursor..end]);
    }
    rendered
}

fn quoted_end(line: &str, start: usize, quote: char) -> usize {
    let mut escaped = false;
    for (offset, character) in line[start + quote.len_utf8()..].char_indices() {
        let absolute = start + quote.len_utf8() + offset;
        if quote == '"' && character == '\\' && !escaped {
            escaped = true;
            continue;
        }
        if character == quote && !escaped {
            return absolute + character.len_utf8();
        }
        escaped = false;
    }
    line.len()
}

fn operator_at(line: &str, index: usize) -> Option<(usize, OperatorKind)> {
    for (operator, kind) in [
        ("|>", OperatorKind::StructuredPipe),
        ("2>>", OperatorKind::Expression),
        ("&&", OperatorKind::CommandSeparator),
        ("||", OperatorKind::CommandSeparator),
        (">>", OperatorKind::Expression),
        ("2>", OperatorKind::Expression),
        ("=>", OperatorKind::Expression),
        (">=", OperatorKind::Expression),
        ("<=", OperatorKind::Expression),
        ("==", OperatorKind::Expression),
        ("!=", OperatorKind::Expression),
        ("|", OperatorKind::ShellPipe),
        (";", OperatorKind::CommandSeparator),
        ("<", OperatorKind::Expression),
        (">", OperatorKind::Expression),
        ("=", OperatorKind::Expression),
    ] {
        if line[index..].starts_with(operator) {
            return Some((operator.len(), kind));
        }
    }
    None
}

fn word_end(line: &str, start: usize) -> usize {
    for (offset, character) in line[start..].char_indices() {
        let index = start + offset;
        if index > start
            && (character.is_whitespace()
                || matches!(character, '\'' | '"' | '(' | ')' | ',' | ':')
                || operator_at(line, index).is_some())
        {
            return index;
        }
    }
    line.len()
}

fn next_non_whitespace_char(line: &str, start: usize) -> Option<char> {
    line[start..]
        .chars()
        .find(|character| !character.is_whitespace())
}

fn command_takes_path(command: &str) -> bool {
    matches!(command, "cd" | "pushd" | "source" | ".")
}

fn is_spar_keyword(word: &str) -> bool {
    matches!(
        word,
        "var"
            | "const"
            | "mut"
            | "export"
            | "fn"
            | "function"
            | "functionGroup"
            | "type"
            | "struct"
            | "impl"
            | "enum"
            | "private"
            | "import"
            | "if"
            | "else"
            | "for"
            | "while"
            | "loop"
            | "break"
            | "continue"
            | "return"
            | "__shell"
            | "exec"
            | "await"
            | "async"
    )
}

pub struct SparshHighlighter {
    snapshot: Arc<RwLock<ShellUiSnapshot>>,
    theme: Theme,
}

impl SparshHighlighter {
    pub fn new(snapshot: Arc<RwLock<ShellUiSnapshot>>, theme: Theme) -> Self {
        Self { snapshot, theme }
    }
}

impl Highlighter for SparshHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        styled.push((Style::new(), line.to_string()));
        let Ok(snapshot) = self.snapshot.read() else {
            return styled;
        };
        for span in scan(line, &snapshot) {
            styled.style_range(
                span.range.start,
                span.range.end,
                self.theme.style(span.role),
            );
        }
        styled
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;
    use std::sync::{Arc, RwLock};

    use reedline::Highlighter;
    use sparsh_core::ShellSession;

    use crate::theme::{SemanticRole, Theme};

    use super::{paint_range, scan, HighlightSpan, SparshHighlighter};

    fn roles(spans: &[HighlightSpan]) -> Vec<(Range<usize>, SemanticRole)> {
        spans
            .iter()
            .map(|span| (span.range.clone(), span.role))
            .collect()
    }

    #[test]
    fn classifies_commands_options_quotes_operators_and_arguments() {
        let snapshot = ShellSession::new().ui_snapshot();
        let spans = scan("/bin/sh build --release && pwd \"done\"", &snapshot);

        assert_eq!(
            roles(&spans),
            vec![
                (0..7, SemanticRole::ExternalCommand),
                (8..13, SemanticRole::Argument),
                (14..23, SemanticRole::Option),
                (24..26, SemanticRole::Operator),
                (27..30, SemanticRole::Builtin),
                (31..37, SemanticRole::QuotedString),
            ]
        );
    }

    #[test]
    fn ll_is_highlighted_as_a_builtin_not_an_unknown_command() {
        let snapshot = ShellSession::new().ui_snapshot();
        let spans = scan("ll -la src", &snapshot);

        assert_eq!(spans[0].role, SemanticRole::Builtin);
    }

    #[test]
    fn unknown_command_is_only_a_visual_warning() {
        let snapshot = ShellSession::new().ui_snapshot();
        let spans = scan("sparsh-command-that-cannot-exist test", &snapshot);

        assert_eq!(spans[0].role, SemanticRole::UnknownCommand);
    }

    #[test]
    fn new_language_keywords_are_highlighted_as_spar_syntax() {
        let snapshot = ShellSession::new().ui_snapshot();
        for (input, word) in [
            ("while count < 3 {", "while"),
            ("loop {", "loop"),
            ("const LIMIT: int = 4 % 3;", "const"),
        ] {
            let spans = scan(input, &snapshot);
            assert!(
                spans
                    .iter()
                    .any(|span| &input[span.range.clone()] == word
                        && span.role == SemanticRole::SparSyntax),
                "{input}: {:?}",
                roles(&spans)
            );
        }
    }

    #[test]
    fn recognizes_basic_spar_declaration_syntax() {
        let snapshot = ShellSession::new().ui_snapshot();
        let spans = scan("var project: str = \"spar\";", &snapshot);

        assert!(spans
            .iter()
            .any(|span| span.role == SemanticRole::SparSyntax));
        assert!(spans
            .iter()
            .any(|span| span.role == SemanticRole::QuotedString));
    }

    #[test]
    fn cd_path_uses_a_visible_path_role_instead_of_generic_argument_gray() {
        let snapshot = ShellSession::new().ui_snapshot();
        let spans = scan("cd ~/Projects/Spar", &snapshot);

        assert_eq!(spans[0].role, SemanticRole::Builtin);
        assert_eq!(spans[1].role, SemanticRole::Path);
    }

    #[test]
    fn painted_range_can_be_reused_by_the_full_screen_editor() {
        let snapshot = ShellSession::new().ui_snapshot();
        let source = "cd ~/Projects/Spar";

        assert_eq!(
            paint_range(source, 3..source.len(), &snapshot, &Theme::plain()),
            "~/Projects/Spar"
        );
        let colored = paint_range(source, 0..source.len(), &snapshot, &Theme::colored());
        assert!(colored.contains("\x1b["));
        assert!(colored.contains("cd"));
        assert!(colored.contains("~/Projects/Spar"));
    }

    #[test]
    fn valid_spar_function_call_and_named_parameter_have_semantic_roles() {
        let mut session = ShellSession::new();
        session
            .submit("function create(name: str) -> str { return name; };")
            .unwrap();
        let snapshot = session.ui_snapshot();
        let spans = scan("create(name: \"OCC\")", &snapshot);

        assert_eq!(spans[0].role, SemanticRole::Function);
        assert!(spans
            .iter()
            .any(|span| span.role == SemanticRole::Parameter));
        assert!(spans
            .iter()
            .all(|span| span.role != SemanticRole::UnknownCommand));
    }

    #[test]
    fn tilde_marker_is_spar_syntax_and_the_command_after_it_is_a_command() {
        let snapshot = ShellSession::new().ui_snapshot();
        let source = "~ ls -la";
        let spans = scan(source, &snapshot);
        assert_eq!(spans[0].range, 0..1);
        assert_eq!(spans[0].role, SemanticRole::SparSyntax);
        assert_eq!(role_of(source, "ls", &snapshot), Some(SemanticRole::Builtin));
        assert_eq!(role_of(source, "-la", &snapshot), Some(SemanticRole::Option));
        let spans = scan("~/bin/tool", &snapshot);
        assert_ne!(spans[0].role, SemanticRole::SparSyntax);
    }

    #[test]
    fn tilde_marker_before_an_external_command_and_lone_tilde() {
        let snapshot = ShellSession::new().ui_snapshot();
        let source = "~ git status";
        let spans = scan(source, &snapshot);
        assert_eq!((spans[0].range.clone(), spans[0].role), (0..1, SemanticRole::SparSyntax));
        assert_eq!(role_of(source, "git", &snapshot), Some(SemanticRole::ExternalCommand));
        for lone in ["~", "~\n", "~\nls"] {
            let spans = scan(lone, &snapshot);
            assert!(
                spans.iter().all(|span| span.range.end <= lone.len()),
                "{lone:?}: {:?}",
                roles(&spans)
            );
        }
        let spans = scan("~", &snapshot);
        assert!(spans.iter().all(|span| span.role != SemanticRole::SparSyntax));
    }

    #[test]
    fn command_is_not_a_spar_keyword() {
        assert!(!super::is_spar_keyword("command"));
        assert!(!super::is_spar_keyword("shell"));
    }

    fn role_of(
        source: &str,
        word: &str,
        snapshot: &sparsh_core::ShellUiSnapshot,
    ) -> Option<SemanticRole> {
        scan(source, snapshot)
            .into_iter()
            .find(|span| &source[span.range.clone()] == word)
            .map(|span| span.role)
    }

    #[test]
    fn data_functions_are_green_at_the_prompt_without_an_import() {
        let source = "printf 'a\\n1\\n' | from csv |> where(fn(r) => r.a > 0) |> take(1)";
        let session = ShellSession::try_new_interactive().unwrap();
        let snapshot = session.ui_snapshot();

        assert_eq!(
            role_of(source, "where", &snapshot),
            Some(SemanticRole::Function)
        );
        assert_eq!(
            role_of(source, "take", &snapshot),
            Some(SemanticRole::Function)
        );
        assert_eq!(
            role_of("where(x, fn(r) => true)", "where", &snapshot),
            Some(SemanticRole::Function)
        );

        // Scripts (non-interactive sessions) still need the import, so the
        // name is not highlighted as a function there.
        let script = ShellSession::try_new().unwrap().ui_snapshot();
        assert_ne!(
            role_of(source, "where", &script),
            Some(SemanticRole::Function)
        );
    }

    #[test]
    fn await_is_a_keyword_and_the_awaited_call_is_a_function() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        session
            .submit("async function fetch(url: str) -> int { return 1; };")
            .unwrap();
        let snapshot = session.ui_snapshot();
        let source = "await fetch(url: \"x\")";

        assert_eq!(
            role_of(source, "await", &snapshot),
            Some(SemanticRole::SparSyntax)
        );
        assert_eq!(
            role_of(source, "fetch", &snapshot),
            Some(SemanticRole::Function)
        );
    }

    #[test]
    fn a_user_declared_name_stops_being_a_prelude_function() {
        let mut session = ShellSession::try_new_interactive().unwrap();
        session.submit("var mut count: int = 0;").unwrap();
        let snapshot = session.ui_snapshot();

        assert_ne!(
            role_of("count(1)", "count", &snapshot),
            Some(SemanticRole::Function)
        );
        assert_eq!(
            role_of("take(1)", "take", &snapshot),
            Some(SemanticRole::Function)
        );
    }

    #[test]
    fn mixed_pipeline_bridge_and_structured_stage_are_not_unknown_commands() {
        let mut session = ShellSession::new();
        session
            .submit_spar(r#"import pkg { where } from "std/data";"#)
            .unwrap();
        let snapshot = session.ui_snapshot();
        let source =
            "printf 'name,age\\nObi,24\\n' | from csv |> where(fn(row) => row.age > 20) |> to json";
        let spans = scan(source, &snapshot);

        let slice = |span: &HighlightSpan| &source[span.range.clone()];
        assert!(spans
            .iter()
            .any(|span| slice(span) == "from" && span.role == SemanticRole::SparSyntax));
        assert!(spans
            .iter()
            .any(|span| slice(span) == "csv" && span.role != SemanticRole::UnknownCommand));
        assert!(spans
            .iter()
            .any(|span| slice(span) == "|>" && span.role == SemanticRole::Operator));
        assert!(spans
            .iter()
            .any(|span| slice(span) == "where" && span.role == SemanticRole::Function));
        assert!(spans
            .iter()
            .any(|span| slice(span) == "fn" && span.role == SemanticRole::SparSyntax));
        assert!(spans
            .iter()
            .any(|span| slice(span) == "to" && span.role == SemanticRole::SparSyntax));
        assert!(spans
            .iter()
            .any(|span| slice(span) == "json" && span.role != SemanticRole::UnknownCommand));
        assert!(
            spans
                .iter()
                .all(|span| span.role != SemanticRole::UnknownCommand),
            "known mixed-pipeline syntax must not render as an unknown command: {spans:?}"
        );
    }

    #[test]
    fn from_is_not_a_global_bridge_keyword() {
        let snapshot = ShellSession::new().ui_snapshot();
        let spans = scan("from something", &snapshot);
        assert_ne!(spans[0].role, SemanticRole::SparSyntax);
    }

    #[test]
    fn structured_pipe_is_one_span_not_pipe_plus_greater_than() {
        let snapshot = ShellSession::new().ui_snapshot();
        let source = "value |> take(2)";
        let spans = scan(source, &snapshot);
        let pipe = spans
            .iter()
            .find(|span| &source[span.range.clone()] == "|>")
            .unwrap();
        assert_eq!(pipe.role, SemanticRole::Operator);
        assert_eq!(pipe.range.end - pipe.range.start, 2);
    }

    #[test]
    fn mixed_highlighting_preserves_every_input_byte() {
        let snapshot = Arc::new(RwLock::new(ShellSession::new().ui_snapshot()));
        let highlighter = SparshHighlighter::new(snapshot, Theme::colored());
        let source = "printf 'x\\n' | from lines |> to json";
        let styled = highlighter.highlight(source, source.len());
        let reconstructed = styled
            .buffer
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>();
        assert_eq!(reconstructed, source);
    }

    #[test]
    fn mixed_highlighter_recognizes_all_supported_codecs() {
        let snapshot = ShellSession::new().ui_snapshot();
        for codec in [
            "json", "jsonl", "csv", "tsv", "yaml", "toml", "lines", "text",
        ] {
            let source = format!("printf x | from {codec} |> to {codec}");
            let spans = scan(&source, &snapshot);
            let codec_spans = spans
                .iter()
                .filter(|span| &source[span.range.clone()] == codec)
                .collect::<Vec<_>>();
            assert_eq!(codec_spans.len(), 2, "{codec}: {spans:?}");
            assert!(codec_spans
                .iter()
                .all(|span| span.role != SemanticRole::UnknownCommand));
        }
    }

    #[test]
    fn reedline_highlighter_preserves_every_input_byte() {
        let snapshot = Arc::new(RwLock::new(ShellSession::new().ui_snapshot()));
        let highlighter = SparshHighlighter::new(snapshot, Theme::colored());

        let styled = highlighter.highlight("pwd --logical", 4);
        let rendered = styled
            .buffer
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>();

        assert_eq!(rendered, "pwd --logical");
        assert!(styled.buffer.len() >= 3);
    }

    #[test]
    fn poisoned_snapshot_falls_back_to_neutral_text() {
        let snapshot = Arc::new(RwLock::new(ShellSession::new().ui_snapshot()));
        let poison = Arc::clone(&snapshot);
        let _ = std::thread::spawn(move || {
            let _guard = poison.write().unwrap();
            panic!("poison test lock");
        })
        .join();
        let highlighter = SparshHighlighter::new(snapshot, Theme::colored());

        let styled = highlighter.highlight("pwd", 0);

        assert_eq!(styled.buffer.len(), 1);
        assert_eq!(styled.buffer[0].1, "pwd");
    }

    fn word_role(
        input: &str,
        snapshot: &sparsh_core::ShellUiSnapshot,
        word: &str,
    ) -> Option<SemanticRole> {
        scan(input, snapshot)
            .into_iter()
            .find(|span| &input[span.range.clone()] == word)
            .map(|span| span.role)
    }

    #[test]
    fn view_is_highlighted_as_a_builtin() {
        let snapshot = ShellSession::new().ui_snapshot();
        assert_eq!(scan("view", &snapshot)[0].role, SemanticRole::Builtin);
    }

    #[test]
    fn spar_lines_use_the_lexer_for_numbers_types_and_comments() {
        let snapshot = ShellSession::new().ui_snapshot();
        let input = "var count: int = 42; // note";
        assert_eq!(word_role(input, &snapshot, "var"), Some(SemanticRole::SparSyntax));
        assert_eq!(word_role(input, &snapshot, "int"), Some(SemanticRole::TypeName));
        assert_eq!(word_role(input, &snapshot, "42"), Some(SemanticRole::DataNumber));
        assert_eq!(word_role(input, &snapshot, "// note"), Some(SemanticRole::Comment));
        assert_eq!(word_role(input, &snapshot, "count"), Some(SemanticRole::Argument));
    }

    #[test]
    fn spar_types_booleans_and_null_get_their_roles() {
        let snapshot = ShellSession::new().ui_snapshot();
        let input = "var flags: List<bool> = [true, false, null];";
        assert_eq!(word_role(input, &snapshot, "List"), Some(SemanticRole::TypeName));
        assert_eq!(word_role(input, &snapshot, "bool"), Some(SemanticRole::TypeName));
        assert_eq!(word_role(input, &snapshot, "true"), Some(SemanticRole::DataBool));
        assert_eq!(word_role(input, &snapshot, "false"), Some(SemanticRole::DataBool));
        assert_eq!(word_role(input, &snapshot, "null"), Some(SemanticRole::DataNull));
    }

    #[test]
    fn string_interpolation_highlights_the_expression_inside() {
        let snapshot = ShellSession::new().ui_snapshot();
        let input = "var s: str = \"n=${1 + 2}!\";";
        assert_eq!(word_role(input, &snapshot, "1"), Some(SemanticRole::DataNumber));
        assert_eq!(word_role(input, &snapshot, "2"), Some(SemanticRole::DataNumber));
        let fragment = input.find("n=").unwrap();
        let spans = scan(input, &snapshot);
        assert!(
            spans.iter().any(|span| span.range.start <= fragment
                && span.range.end > fragment
                && span.role == SemanticRole::QuotedString),
            "{:?}",
            roles(&spans)
        );
    }

    #[test]
    fn known_function_calls_and_named_parameters_are_highlighted() {
        let mut session = ShellSession::new();
        session
            .submit("fn add(a: int, b: int) -> int {\n    return a + b;\n};")
            .unwrap();
        let snapshot = session.ui_snapshot();
        let input = "add(a: 1, b: 2)";
        assert_eq!(word_role(input, &snapshot, "add"), Some(SemanticRole::Function));
        assert_eq!(word_role(input, &snapshot, "a"), Some(SemanticRole::Parameter));
        assert_eq!(word_role(input, &snapshot, "1"), Some(SemanticRole::DataNumber));
    }

    #[test]
    fn half_typed_spar_falls_back_without_losing_highlighting() {
        let snapshot = ShellSession::new().ui_snapshot();
        let input = "var s: str = \"abc";
        let spans = scan(input, &snapshot);
        assert!(!spans.is_empty());
        assert_eq!(word_role(input, &snapshot, "var"), Some(SemanticRole::SparSyntax));
    }

    #[test]
    fn spar_highlighting_never_panics_or_overlaps_on_awkward_input() {
        let snapshot = ShellSession::new().ui_snapshot();
        let inputs = [
            "var s: str = \"é ${ \"",
            "var s: str = \"${1 + ${2}}\" // end",
            "var s: str = \"unterminated ${",
            "fn f(a: int) -> int { return a; }; // ünï",
            "if x { var y: int = 1 } else {",
            "var 🙂: int = 1;",
            "// only a comment",
            "var s: str = \"\\\"quoted\\\"\";",
            "const N: int = 4 % 3; while N < 9 { loop { break; } }",
            "var t: (int, str) = (1, \"a\"); t",
            "f(a: \"${g(b: 1)}\")",
        ];
        for input in inputs {
            let spans = scan(input, &snapshot);
            let mut last_end = 0;
            for span in &spans {
                assert!(
                    span.range.start >= last_end && span.range.end <= input.len(),
                    "{input:?}: {:?}",
                    roles(&spans)
                );
                assert!(input.is_char_boundary(span.range.start), "{input:?}");
                assert!(input.is_char_boundary(span.range.end), "{input:?}");
                last_end = span.range.end;
            }
            // Painting must reproduce every input byte.
            let painted = paint_range(input, 0..input.len(), &snapshot, &Theme::plain());
            assert_eq!(painted, input, "{input:?}");
        }
    }
}
