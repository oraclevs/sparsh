//! Completion menu rendering: one entry is `tag label  detail`, with the tag
//! and label colored by the item's kind. Pure functions, so the layout is
//! tested without a terminal.

use reedline::{Span, Suggestion};
use sparsh_core::{CompletionItem, ItemKind};

use crate::styled::{char_width, sanitize, Line};
use crate::{SemanticRole, Theme};

pub(crate) type StyledLine = Line;

/// Short fixed-vocabulary tag for the kind column.
pub(crate) fn kind_tag(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Command => "cmd",
        ItemKind::Alias => "als",
        ItemKind::Builtin => "bi",
        ItemKind::Function => "fn",
        ItemKind::Variable => "var",
        ItemKind::Field => "fld",
        ItemKind::Method => "mth",
        ItemKind::Struct => "st",
        ItemKind::Enum => "enm",
        ItemKind::EnumMember => "mem",
        ItemKind::Type => "ty",
        ItemKind::Keyword => "kw",
        ItemKind::Parameter => "prm",
        ItemKind::File => "file",
        ItemKind::Directory => "dir",
    }
}

/// The theme role a kind is drawn with.
pub(crate) fn kind_role(kind: ItemKind) -> SemanticRole {
    match kind {
        ItemKind::Command => SemanticRole::CompletionCommand,
        ItemKind::Alias => SemanticRole::Alias,
        ItemKind::Builtin => SemanticRole::Builtin,
        ItemKind::Function | ItemKind::Method => SemanticRole::CompletionFunction,
        ItemKind::Variable | ItemKind::Field | ItemKind::Parameter => {
            SemanticRole::CompletionVariable
        }
        ItemKind::Struct | ItemKind::Enum | ItemKind::EnumMember | ItemKind::Type => {
            SemanticRole::CompletionType
        }
        ItemKind::Keyword => SemanticRole::CompletionKeyword,
        ItemKind::File => SemanticRole::CompletionPath,
        ItemKind::Directory => SemanticRole::CompletionDirectory,
    }
}

const TAG_COLUMN: usize = 4;

fn first_line(text: &str) -> String {
    sanitize(text.lines().next().unwrap_or("")).trim().to_string()
}

fn clip(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cell = char_width(character);
        if used + cell > width {
            break;
        }
        out.push(character);
        used += cell;
    }
    out
}

/// `tag label  detail`, cut to `width` display columns with an ellipsis.
pub(crate) fn menu_entry(item: &CompletionItem, _theme: &Theme, width: usize) -> StyledLine {
    if width == 0 {
        return Line::new();
    }
    let mut line = Line::new();
    let role = item.kind.map(kind_role);
    if let Some(kind) = item.kind {
        let tag = kind_tag(kind);
        line.push(Some(SemanticRole::Secondary), tag);
        line.push(
            None,
            " ".repeat(TAG_COLUMN.saturating_sub(tag.chars().count()) + 1),
        );
    }
    line.push(role, sanitize(&item.replacement));
    if let Some(detail) = item.description.as_deref().map(first_line) {
        if !detail.is_empty() {
            line.push(None, "  ");
            line.push(Some(SemanticRole::Secondary), detail);
        }
    }
    if line.width() <= width {
        return line;
    }
    line.ellipsize(width, false)
}

/// The reedline suggestion for an item. `value` stays the replacement text;
/// the description carries `tag  detail` (the menu shows it after the value
/// and again, in full, for the selected item) and the style carries the kind
/// color.
pub(crate) fn suggestion(item: &CompletionItem, theme: &Theme) -> Suggestion {
    let detail = item.description.as_deref().map(first_line);
    let tag = item.kind.map(kind_tag);
    let description = match (tag, detail.as_deref()) {
        (Some(tag), Some(detail)) if !detail.is_empty() => Some(format!("{tag}  {detail}")),
        (Some(tag), _) => Some(tag.to_string()),
        (None, Some(detail)) if !detail.is_empty() => Some(detail.to_string()),
        _ => None,
    };
    Suggestion {
        value: item.replacement.clone(),
        description,
        style: item
            .kind
            .filter(|_| theme.enabled())
            .map(|kind| theme.style(kind_role(kind))),
        extra: tag.map(|tag| vec![tag.to_string()]),
        span: Span {
            start: item.span.start,
            end: item.span.end,
        },
        ..Suggestion::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: Option<ItemKind>, label: &str, detail: Option<&str>) -> CompletionItem {
        CompletionItem {
            replacement: label.into(),
            span: 2..5,
            description: detail.map(str::to_string),
            kind,
        }
    }

    fn entry(kind: ItemKind, width: usize) -> StyledLine {
        menu_entry(&item(Some(kind), "name", Some("detail")), &Theme::colored(), width)
    }

    #[test]
    fn each_kind_family_has_its_tag_and_role() {
        let cases = [
            (ItemKind::Function, "fn ", SemanticRole::CompletionFunction),
            (ItemKind::Method, "mth", SemanticRole::CompletionFunction),
            (ItemKind::Variable, "var", SemanticRole::CompletionVariable),
            (ItemKind::Field, "fld", SemanticRole::CompletionVariable),
            (ItemKind::Parameter, "prm", SemanticRole::CompletionVariable),
            (ItemKind::Struct, "st ", SemanticRole::CompletionType),
            (ItemKind::Enum, "enm", SemanticRole::CompletionType),
            (ItemKind::EnumMember, "mem", SemanticRole::CompletionType),
            (ItemKind::Type, "ty ", SemanticRole::CompletionType),
            (ItemKind::Keyword, "kw ", SemanticRole::CompletionKeyword),
            (ItemKind::File, "file", SemanticRole::CompletionPath),
            (ItemKind::Directory, "dir", SemanticRole::CompletionDirectory),
            (ItemKind::Command, "cmd", SemanticRole::CompletionCommand),
            (ItemKind::Alias, "als", SemanticRole::Alias),
            (ItemKind::Builtin, "bi ", SemanticRole::Builtin),
        ];
        for (kind, tag, role) in cases {
            let line = entry(kind, 80);
            assert!(line.plain().starts_with(tag.trim_end()), "{kind:?}: {}", line.plain());
            assert!(line.plain().contains("name  detail"), "{}", line.plain());
            assert_eq!(kind_role(kind), role, "{kind:?}");
            assert!(line.paint(&Theme::colored()).contains("\x1b["));
        }
    }

    #[test]
    fn entries_align_the_label_column() {
        let a = entry(ItemKind::Function, 80).plain();
        let b = entry(ItemKind::Directory, 80).plain();
        assert_eq!(a.find("name"), b.find("name"));
    }

    #[test]
    fn long_detail_is_truncated_with_an_ellipsis() {
        let wide = menu_entry(
            &item(Some(ItemKind::Function), "go", Some("a very long signature here")),
            &Theme::plain(),
            16,
        );
        assert_eq!(wide.width(), 16);
        assert!(wide.plain().ends_with('…'), "{}", wide.plain());
        assert!(wide.plain().starts_with("fn   go"));
    }

    #[test]
    fn width_zero_and_tiny_widths_do_not_panic() {
        for width in 0..4 {
            let line = entry(ItemKind::Function, width);
            assert!(line.width() <= width.max(1));
        }
        assert_eq!(entry(ItemKind::Function, 0).plain(), "");
    }

    #[test]
    fn wide_unicode_labels_stay_within_width() {
        let it = item(Some(ItemKind::File), "日本語ファイル", Some("詳細"));
        for width in 1..30 {
            let line = menu_entry(&it, &Theme::plain(), width);
            assert!(line.width() <= width, "{width}: {}", line.plain());
        }
    }

    #[test]
    fn items_without_a_kind_render_plain() {
        let line = menu_entry(&item(None, "thing", Some("d")), &Theme::colored(), 40);
        assert_eq!(line.plain(), "thing  d");
        assert_eq!(line.paint(&Theme::colored()).matches("\x1b[").count() > 0, true);
        let bare = menu_entry(&item(None, "thing", None), &Theme::colored(), 40);
        assert_eq!(bare.plain(), "thing");
        assert_eq!(bare.sole_role(), None);
    }

    #[test]
    fn multi_line_detail_uses_its_first_line() {
        let line = menu_entry(
            &item(Some(ItemKind::Function), "f", Some("first\nsecond")),
            &Theme::plain(),
            80,
        );
        assert!(line.plain().ends_with("f  first"), "{}", line.plain());
    }

    #[test]
    fn new_roles_default_to_the_highlight_role_colors() {
        let theme = Theme::colored();
        let pairs = [
            (SemanticRole::CompletionFunction, SemanticRole::Function),
            (SemanticRole::CompletionVariable, SemanticRole::Parameter),
            (SemanticRole::CompletionType, SemanticRole::TypeName),
            (SemanticRole::CompletionKeyword, SemanticRole::SparSyntax),
            (SemanticRole::CompletionPath, SemanticRole::Path),
            (SemanticRole::CompletionDirectory, SemanticRole::FileDirectory),
            (SemanticRole::CompletionCommand, SemanticRole::ExternalCommand),
        ];
        for (new, old) in pairs {
            assert_eq!(theme.style(new), theme.style(old), "{new:?}");
        }
        assert_eq!(Theme::plain().style(SemanticRole::CompletionFunction), nu_ansi_term::Style::new());
    }

    #[test]
    fn suggestion_keeps_value_span_and_carries_detail_and_style() {
        let it = item(Some(ItemKind::Function), "greet(", Some("(name: str) -> str"));
        let s = suggestion(&it, &Theme::colored());
        assert_eq!(s.value, "greet(");
        assert_eq!((s.span.start, s.span.end), (2, 5));
        assert_eq!(s.description.as_deref(), Some("fn  (name: str) -> str"));
        assert_eq!(s.extra, Some(vec!["fn".to_string()]));
        assert_eq!(s.style, Some(Theme::colored().style(SemanticRole::CompletionFunction)));
        let plain = suggestion(&it, &Theme::plain());
        assert_eq!(plain.style, None);
        let none = suggestion(&item(None, "x", None), &Theme::colored());
        assert_eq!((none.description, none.style, none.extra), (None, None, None));
        let tag_only = suggestion(&item(Some(ItemKind::Directory), "d/", None), &Theme::plain());
        assert_eq!(tag_only.description.as_deref(), Some("dir"));
    }
}
