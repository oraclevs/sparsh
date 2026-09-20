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

pub fn scan(line: &str, snapshot: &ShellUiSnapshot) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let mut index = 0;
    let mut command_position = true;
    let mut active_command: Option<String> = None;
    let mut paren_depth = 0usize;

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
            let role = if active_command.as_deref().is_some_and(command_takes_path) {
                SemanticRole::Path
            } else {
                SemanticRole::QuotedString
            };
            spans.push(HighlightSpan { range: index..end, role });
            command_position = false;
            index = end;
            continue;
        }

        if let Some((length, starts_command)) = operator_at(line, index) {
            spans.push(HighlightSpan {
                range: index..index + length,
                role: SemanticRole::Operator,
            });
            if starts_command {
                command_position = true;
                active_command = None;
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

        let role = if function_call {
            SemanticRole::Function
        } else if named_parameter {
            SemanticRole::Parameter
        } else if command_position && is_spar_keyword(word) {
            SemanticRole::SparSyntax
        } else if command_position {
            let kind = snapshot.classify_command(word);
            active_command = Some(word.to_string());
            match kind {
                CommandKind::Builtin => SemanticRole::Builtin,
                CommandKind::Alias => SemanticRole::Alias,
                CommandKind::External => SemanticRole::ExternalCommand,
                CommandKind::Unknown => SemanticRole::UnknownCommand,
            }
        } else if active_command.as_deref().is_some_and(command_takes_path)
            && !word.starts_with('-')
        {
            SemanticRole::Path
        } else if word.starts_with('-') {
            SemanticRole::Option
        } else {
            SemanticRole::Argument
        };

        spans.push(HighlightSpan { range: index..end, role });
        command_position = false;
        index = end;
    }

    spans
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

fn operator_at(line: &str, index: usize) -> Option<(usize, bool)> {
    for (operator, starts_command) in [
        ("2>>", false),
        ("&&", true),
        ("||", true),
        (">>", false),
        ("2>", false),
        ("|", true),
        (";", true),
        ("<", false),
        (">", false),
        ("=", false),
    ] {
        if line[index..].starts_with(operator) {
            return Some((operator.len(), starts_command));
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
    line[start..].chars().find(|character| !character.is_whitespace())
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
        assert!(spans.iter().any(|span| span.role == SemanticRole::Parameter));
        assert!(spans.iter().all(|span| span.role != SemanticRole::UnknownCommand));
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
