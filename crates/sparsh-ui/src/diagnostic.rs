use std::io::{self, Write};

use sparsh_core::ShellError;

use crate::theme::{SemanticRole, Theme};

pub fn render_error_text(error: &ShellError, source: Option<&str>, theme: &Theme) -> String {
    match error {
        ShellError::CommandNotFound {
            program,
            suggestions,
        } => {
            let mut text = format!(
                "{}: command not found: `{program}`\n",
                theme.paint(SemanticRole::Error, "error")
            );
            if let Some(source) = source {
                let offset = source.find(program).unwrap_or(0);
                text.push_str("\n  ");
                text.push_str(source);
                text.push_str("\n  ");
                text.push_str(&" ".repeat(offset));
                text.push_str(
                    &theme.paint(SemanticRole::Warning, &"^".repeat(program.chars().count())),
                );
                text.push('\n');
            }
            if !suggestions.is_empty() {
                text.push_str("\nDid you mean:\n");
                for suggestion in suggestions {
                    text.push_str("  ");
                    text.push_str(&theme.paint(SemanticRole::Warning, suggestion));
                    text.push('\n');
                }
            }
            text
        }
        _ => format!("{}: {error}\n", theme.paint(SemanticRole::Error, "error")),
    }
}

pub fn render_error<W: Write>(
    error: &ShellError,
    source: Option<&str>,
    theme: &Theme,
    err: &mut W,
) -> io::Result<()> {
    err.write_all(render_error_text(error, source, theme).as_bytes())
}

#[cfg(test)]
mod tests {
    use sparsh_core::ShellError;

    use crate::theme::Theme;

    use super::render_error_text;

    #[test]
    fn plain_missing_command_diagnostic_locates_first_command() {
        let error = ShellError::CommandNotFound {
            program: "cargoo".into(),
            suggestions: vec!["cargo".into()],
        };

        let text = render_error_text(&error, Some("cargoo test"), &Theme::plain());

        assert_eq!(
            text,
            "error: command not found: `cargoo`\n\n  cargoo test\n  ^^^^^^\n\nDid you mean:\n  cargo\n"
        );
    }

    #[test]
    fn no_color_diagnostic_contains_no_escape_bytes() {
        let error = ShellError::CommandNotFound {
            program: "bad".into(),
            suggestions: Vec::new(),
        };

        let text = render_error_text(&error, Some("bad"), &Theme::plain());

        assert!(!text.contains("\x1b["));
    }
}
