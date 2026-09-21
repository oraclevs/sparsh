//! Styled text lines shared by the table and encoded-data renderers.
//!
//! Widths are measured on the plain text, and color is applied last through the
//! `Theme`, so `Theme::plain()` output is exactly the uncolored layout.

use unicode_width::UnicodeWidthChar;

use crate::{SemanticRole, Theme};

pub(crate) type Role = Option<SemanticRole>;

/// One physical output line made of independently colored segments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Line {
    segments: Vec<(Role, String)>,
}

impl Line {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn of(role: Role, text: impl Into<String>) -> Self {
        let mut line = Self::new();
        line.push(role, text);
        line
    }

    pub(crate) fn push(&mut self, role: Role, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.segments.push((role, text));
        }
    }

    pub(crate) fn extend(&mut self, other: Line) {
        self.segments.extend(other.segments);
    }

    pub(crate) fn width(&self) -> usize {
        self.segments.iter().map(|(_, text)| text_width(text)).sum()
    }

    pub(crate) fn plain(&self) -> String {
        self.segments
            .iter()
            .map(|(_, text)| text.as_str())
            .collect()
    }

    /// The role of the line when a single segment carries all of it.
    pub(crate) fn sole_role(&self) -> Role {
        match self.segments.as_slice() {
            [(role, _)] => *role,
            _ => None,
        }
    }

    pub(crate) fn paint(&self, theme: &Theme) -> String {
        let mut out = String::new();
        for (role, text) in &self.segments {
            match role {
                Some(role) => out.push_str(&theme.paint(*role, text)),
                None => out.push_str(text),
            }
        }
        out
    }

    /// Splits the line into pieces no wider than `width` display columns.
    pub(crate) fn wrap(&self, width: usize) -> Vec<Line> {
        let width = width.max(1);
        let mut lines = vec![Line::new()];
        let mut used = 0;
        for (role, text) in &self.segments {
            let mut chunk = String::new();
            for character in text.chars() {
                let cell = char_width(character);
                if used + cell > width && used > 0 {
                    lines
                        .last_mut()
                        .expect("line")
                        .push(*role, take(&mut chunk));
                    lines.push(Line::new());
                    used = 0;
                }
                chunk.push(character);
                used += cell;
            }
            lines.last_mut().expect("line").push(*role, chunk);
        }
        lines
    }

    /// Cuts the line to `width` columns, ending in `…` when anything was lost
    /// (or when `force` asks for the marker on a line that already fits).
    pub(crate) fn ellipsize(&self, width: usize, force: bool) -> Line {
        let width = width.max(1);
        if !force && self.width() <= width {
            return self.clone();
        }
        let budget = width - 1;
        let mut out = Line::new();
        let mut used = 0;
        'segments: for (role, text) in &self.segments {
            let mut chunk = String::new();
            for character in text.chars() {
                let cell = char_width(character);
                if used + cell > budget {
                    out.push(*role, take(&mut chunk));
                    break 'segments;
                }
                chunk.push(character);
                used += cell;
            }
            out.push(*role, chunk);
        }
        out.push(Some(SemanticRole::DataNull), "…");
        out
    }
}

fn take(text: &mut String) -> String {
    std::mem::take(text)
}

pub(crate) fn char_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

pub(crate) fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// Makes text safe to lay out: tabs become spaces and other control characters
/// (which would move the cursor) are dropped.
pub(crate) fn sanitize(text: &str) -> String {
    text.chars()
        .filter_map(|character| match character {
            '\t' => Some(' '),
            character if character.is_control() => None,
            character => Some(character),
        })
        .collect()
}

pub(crate) fn join_lines(lines: &[Line], theme: &Theme) -> String {
    lines
        .iter()
        .map(|line| line.paint(theme))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The role a bare token (YAML/TOML/CSV scalar) is drawn with.
pub(crate) fn scalar_role(token: &str) -> SemanticRole {
    let token = token.trim();
    match token {
        "true" | "false" | "True" | "False" => SemanticRole::DataBool,
        "null" | "~" | "Null" | "None" => SemanticRole::DataNull,
        _ if token.parse::<f64>().is_ok() => SemanticRole::DataNumber,
        _ => SemanticRole::DataString,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_splits_by_display_width_and_keeps_roles() {
        let mut line = Line::of(Some(SemanticRole::DataKey), "abcd");
        line.push(None, "efgh");
        let wrapped = line.wrap(3);
        let plain = wrapped.iter().map(Line::plain).collect::<Vec<_>>();
        assert_eq!(plain, ["abc", "def", "gh"]);
        assert_eq!(wrapped[0].sole_role(), Some(SemanticRole::DataKey));
    }

    #[test]
    fn wide_characters_are_never_split_across_the_limit() {
        let wrapped = Line::of(None, "日本語").wrap(5);
        assert_eq!(
            wrapped.iter().map(Line::plain).collect::<Vec<_>>(),
            ["日本", "語"]
        );
        assert!(wrapped.iter().all(|line| line.width() <= 5));
    }

    #[test]
    fn ellipsize_marks_lost_text_and_can_be_forced() {
        assert_eq!(Line::of(None, "abcdef").ellipsize(4, false).plain(), "abc…");
        assert_eq!(Line::of(None, "abc").ellipsize(4, false).plain(), "abc");
        assert_eq!(Line::of(None, "abc").ellipsize(4, true).plain(), "abc…");
    }

    #[test]
    fn scalar_roles_follow_the_token() {
        assert_eq!(scalar_role("42"), SemanticRole::DataNumber);
        assert_eq!(scalar_role("true"), SemanticRole::DataBool);
        assert_eq!(scalar_role("~"), SemanticRole::DataNull);
        assert_eq!(scalar_role("\"x\""), SemanticRole::DataString);
    }

    #[test]
    fn sanitize_drops_control_characters() {
        assert_eq!(sanitize("a\tb\x1b[31mc\r"), "a b[31mc");
    }
}
