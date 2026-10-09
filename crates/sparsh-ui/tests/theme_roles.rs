use sparsh_ui::{SemanticRole, Theme};

#[test]
fn plain_theme_suppresses_new_menu_style() {
    assert_eq!(Theme::plain().paint(SemanticRole::MenuSelected, "x"), "x");
}

#[test]
fn new_menu_role_has_a_default_style() {
    assert_ne!(Theme::colored().paint(SemanticRole::MenuSelected, "x"), "x");
}

#[test]
fn new_ui_roles_have_colored_defaults_and_plain_fallbacks() {
    for role in [
        SemanticRole::MenuBorder,
        SemanticRole::HintHistory,
        SemanticRole::HelpHeading,
        SemanticRole::EditorHeader,
        SemanticRole::PagerStatus,
    ] {
        assert_ne!(Theme::colored().paint(role, "x"), "x", "{role:?}");
        assert_eq!(Theme::plain().paint(role, "x"), "x", "{role:?}");
    }
}
