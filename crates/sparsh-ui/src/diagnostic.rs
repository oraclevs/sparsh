use std::io::{self, Write};

use sparsh_core::ShellError;

use crate::theme::{SemanticRole, Theme};

pub fn render_error_text(error: &ShellError, source: Option<&str>, theme: &Theme) -> String {
    match error {
        ShellError::SparSource {
            errors,
            source: file_source,
            filename,
        } => {
            let renderer = if theme.enabled() {
                spar::ErrorRenderer::with_color(file_source, filename)
            } else {
                spar::ErrorRenderer::new(file_source, filename)
            };
            let mut text = renderer.render_all(errors);
            text.push('\n');
            text
        }
        ShellError::Spar(errors) if source.is_some() => {
            let source = source.expect("matched Some source");
            let renderer = if theme.enabled() {
                spar::ErrorRenderer::with_color(source, "<sparsh>")
            } else {
                spar::ErrorRenderer::new(source, "<sparsh>")
            };
            let mut text = renderer.render_all(errors);
            text.push('\n');
            text
        }
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
    fn spar_diagnostic_uses_original_source_span() {
        let error = ShellError::Spar(vec![spar::SparError::ParseError {
            message: "expected expression".into(),
            span: spar::Span::new(16, 17, 2, 5),
        }]);
        let source = "var x: int = 1;\nfoo(}";

        let text = render_error_text(&error, Some(source), &Theme::plain());

        assert!(text.contains("<sparsh>:2:5"), "{text}");
        assert!(text.contains("foo(}"), "{text}");
        assert!(text.contains('^'), "{text}");
        assert!(!text.contains(":0:0"), "{text}");
    }

    #[test]
    fn spar_file_diagnostic_uses_file_source_and_real_filename() {
        let error = ShellError::SparSource {
            errors: vec![spar::SparError::EvalError {
                message: "command substitution exited with status 1".into(),
                span: spar::Span::new(31, 40, 2, 9),
            }],
            source: "function main() -> int {\n    var x: str = $(false);\n    return 0;\n};\n".into(),
            filename: "/tmp/main.spar".into(),
        };

        let text = render_error_text(&error, Some("./main.spar"), &Theme::plain());

        assert!(text.contains("/tmp/main.spar:2:9"), "{text}");
        assert!(text.contains("var x: str = $(false);"), "{text}");
        assert!(!text.contains("<sparsh>:2:9"), "{text}");
    }

    #[test]
    fn colored_spar_diagnostic_is_colored_only_when_theme_is_enabled() {
        let error = ShellError::Spar(vec![spar::SparError::ParseError {
            message: "expected expression".into(),
            span: spar::Span::new(0, 1, 1, 1),
        }]);

        let plain = render_error_text(&error, Some("}"), &Theme::plain());
        let colored = render_error_text(&error, Some("}"), &Theme::colored());

        assert!(!plain.contains("\x1b["));
        assert!(colored.contains("\x1b["));
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
