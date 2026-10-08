//! Completion menu rendering: the reedline suggestion for an item, with the
//! kind tag and detail in the description and the kind color as its style.

use reedline::{Span, Suggestion};
use sparsh_core::{CompletionItem, ItemKind};

use crate::styled::sanitize;
use crate::{SemanticRole, Theme};

/// Short fixed-vocabulary tag for the kind column.
pub(crate) fn kind_tag(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Command => "cmd",
        ItemKind::Alias => "alias",
        ItemKind::Builtin => "builtin",
        ItemKind::Function => "fn",
        ItemKind::Variable => "var",
        ItemKind::Field => "field",
        ItemKind::Method => "method",
        ItemKind::Struct => "struct",
        ItemKind::Enum => "enum",
        ItemKind::EnumMember => "member",
        ItemKind::Type => "type",
        ItemKind::Keyword => "kw",
        ItemKind::Parameter => "param",
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

fn first_line(text: &str) -> String {
    sanitize(text.lines().next().unwrap_or("")).trim().to_string()
}

/// Details that only restate the kind (or say nothing useful) are not shown.
fn is_generic_detail(detail: &str, tag: Option<&str>) -> bool {
    if detail.is_empty() || tag.is_some_and(|tag| detail.eq_ignore_ascii_case(tag)) {
        return true;
    }
    const GENERIC: &[&str] = &[
        "file",
        "directory",
        "alias",
        "spar identifier",
        "spar function",
        "external command",
    ];
    GENERIC.iter().any(|g| detail.eq_ignore_ascii_case(g))
}

/// The reedline suggestion for an item. `value` stays the replacement text;
/// the description carries only the detail (signature, doc line), `extra`
/// carries the kind word for the popup's right-hand column, and the style
/// carries the kind color.
pub(crate) fn suggestion(item: &CompletionItem, theme: &Theme) -> Suggestion {
    let tag = item.kind.map(kind_tag);
    let description = item
        .description
        .as_deref()
        .map(first_line)
        .filter(|detail| !is_generic_detail(detail, tag));
    Suggestion {
        value: item.replacement.clone(),
        description,
        extra: tag.map(|tag| vec![tag.to_string()]),
        style: item
            .kind
            .filter(|_| theme.enabled())
            .map(|kind| theme.style(kind_role(kind))),
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

    #[test]
    fn each_kind_family_has_its_tag_and_role() {
        let cases = [
            (ItemKind::Function, "fn", SemanticRole::CompletionFunction),
            (ItemKind::Method, "method", SemanticRole::CompletionFunction),
            (ItemKind::Variable, "var", SemanticRole::CompletionVariable),
            (ItemKind::Field, "field", SemanticRole::CompletionVariable),
            (ItemKind::Parameter, "param", SemanticRole::CompletionVariable),
            (ItemKind::Struct, "struct", SemanticRole::CompletionType),
            (ItemKind::Enum, "enum", SemanticRole::CompletionType),
            (ItemKind::EnumMember, "member", SemanticRole::CompletionType),
            (ItemKind::Type, "type", SemanticRole::CompletionType),
            (ItemKind::Keyword, "kw", SemanticRole::CompletionKeyword),
            (ItemKind::File, "file", SemanticRole::CompletionPath),
            (ItemKind::Directory, "dir", SemanticRole::CompletionDirectory),
            (ItemKind::Command, "cmd", SemanticRole::CompletionCommand),
            (ItemKind::Alias, "alias", SemanticRole::Alias),
            (ItemKind::Builtin, "builtin", SemanticRole::Builtin),
        ];
        let theme = Theme::colored();
        for (kind, tag, role) in cases {
            assert_eq!(kind_tag(kind), tag, "{kind:?}");
            assert_eq!(kind_role(kind), role, "{kind:?}");
            let s = suggestion(&item(Some(kind), "name", Some("detail")), &theme);
            assert_eq!(s.description.as_deref(), Some("detail"), "{kind:?}");
            assert_eq!(s.extra, Some(vec![tag.to_string()]), "{kind:?}");
            assert_eq!(s.style, Some(theme.style(role)), "{kind:?}");
        }
    }

    #[test]
    fn multi_line_detail_uses_its_first_line() {
        let s = suggestion(
            &item(Some(ItemKind::Function), "f", Some("first\nsecond")),
            &Theme::plain(),
        );
        assert_eq!(s.description.as_deref(), Some("first"));
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
        assert_eq!(s.description.as_deref(), Some("(name: str) -> str"));
                assert_eq!(s.style, Some(Theme::colored().style(SemanticRole::CompletionFunction)));
        let plain = suggestion(&it, &Theme::plain());
        assert_eq!(plain.style, None);
        let none = suggestion(&item(None, "x", None), &Theme::colored());
        assert_eq!((none.description, none.style), (None, None));
        let tag_only = suggestion(&item(Some(ItemKind::Directory), "d/", None), &Theme::plain());
        assert_eq!(tag_only.description, None);
        assert_eq!(tag_only.extra, Some(vec!["dir".to_string()]));
    }

    #[test]
    fn generic_or_repeated_details_are_dropped() {
        let cases = [
            (ItemKind::File, "file"),
            (ItemKind::Directory, "directory"),
            (ItemKind::Variable, "Spar identifier"),
            (ItemKind::Function, "Spar function"),
            (ItemKind::Command, "external command"),
            (ItemKind::Alias, "alias"),
            (ItemKind::Keyword, "kw"),
        ];
        for (kind, detail) in cases {
            let s = suggestion(&item(Some(kind), "x", Some(detail)), &Theme::plain());
            assert_eq!(s.description, None, "{kind:?} {detail}");
        }
        let kept = suggestion(&item(Some(ItemKind::Alias), "ll", Some("ls -la")), &Theme::plain());
        assert_eq!(kept.description.as_deref(), Some("ls -la"));
    }
}
