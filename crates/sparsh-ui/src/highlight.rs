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
            spans.push(HighlightSpan {
                range: index..end,
                role: SemanticRole::QuotedString,
            });
            command_position = false;
            index = end;
            continue;
        }
        if let Some((length, starts_command)) = operator_at(line, index) {
            spans.push(HighlightSpan {
                range: index..index + length,
                role: SemanticRole::Operator,
            });
            command_position = starts_command;
            index += length;
            continue;
        }

        let end = word_end(line, index);
        let word = &line[index..end];
        let role = if command_position && is_spar_keyword(word) {
            SemanticRole::SparSyntax
        } else if command_position {
            match snapshot.classify_command(word) {
                CommandKind::Builtin => SemanticRole::Builtin,
                CommandKind::Alias => SemanticRole::Alias,
                CommandKind::External => SemanticRole::ExternalCommand,
                CommandKind::Unknown => SemanticRole::UnknownCommand,
            }
        } else if word.starts_with('-') {
            SemanticRole::Option
        } else {
            SemanticRole::Argument
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
                || matches!(character, '\'' | '"')
                || operator_at(line, index).is_some())
        {
            return index;
        }
    }
    line.len()
}

fn is_spar_keyword(word: &str) -> bool {
    matches!(
        word,
        "var" | "let" | "export" | "fn" | "type" | "private" | "import"
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

    use super::{scan, HighlightSpan, SparshHighlighter};

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
