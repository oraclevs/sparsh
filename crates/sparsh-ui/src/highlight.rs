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
            "fn" | "if" | "else" | "for" | "return" | "mut" => SemanticRole::SparSyntax,
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
            | "mut"
            | "export"
            | "function"
            | "functionGroup"
            | "type"
            | "struct"
            | "enum"
            | "private"
            | "import"
            | "if"
            | "else"
            | "for"
            | "return"
            | "shell"
            | "command"
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
    fn unknown_command_is_only_a_visual_warning() {
        let snapshot = ShellSession::new().ui_snapshot();
        let spans = scan("sparsh-command-that-cannot-exist test", &snapshot);

        assert_eq!(spans[0].role, SemanticRole::UnknownCommand);
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
}
