use sparsh_core::{ColorSpec, PaletteKey, ThemeColor, ThemeLayer, ThemeRole, ThemeStyleSpec};
use sparsh_ui::Theme;

#[test]
fn every_group_accepts_a_role_foreground_override() {
    for role in [
        ThemeRole::Cwd,
        ThemeRole::Function,
        ThemeRole::MenuSelected,
        ThemeRole::DataKey,
        ThemeRole::GitBranch,
        ThemeRole::HelpHeading,
        ThemeRole::EditorHeader,
        ThemeRole::PagerStatus,
    ] {
        let mut file = ThemeLayer::default();
        file.roles.insert(
            role,
            ThemeStyleSpec {
                foreground: Some(ThemeColor::Literal(ColorSpec::Indexed(201))),
                ..Default::default()
            },
        );
        let changed = Theme::from_layers(true, &file, &ThemeLayer::default());
        assert_ne!(
            changed.paint(role, "x"),
            Theme::colored().paint(role, "x"),
            "{role:?}"
        );
        assert_eq!(
            Theme::from_layers(false, &file, &ThemeLayer::default()).paint(role, "x"),
            "x"
        );
    }
}

#[test]
fn generated_background_beats_config_and_palette_supplies_unset_roles() {
    let mut generated = ThemeLayer::default();
    generated
        .palette
        .insert(PaletteKey::Accent, ColorSpec::Indexed(201));
    generated.roles.insert(
        ThemeRole::MenuSelected,
        ThemeStyleSpec {
            background: Some(ThemeColor::Literal(ColorSpec::Indexed(30))),
            ..Default::default()
        },
    );
    let mut config = ThemeLayer::default();
    config.roles.insert(
        ThemeRole::MenuSelected,
        ThemeStyleSpec {
            background: Some(ThemeColor::Literal(ColorSpec::Indexed(22))),
            ..Default::default()
        },
    );
    let theme = Theme::from_layers(true, &generated, &config);
    let rendered = theme.paint(ThemeRole::MenuSelected, "x");
    assert!(rendered.contains("48;5;30"), "{rendered:?}");
    assert_ne!(
        theme.paint(ThemeRole::Cwd, "x"),
        Theme::colored().paint(ThemeRole::Cwd, "x")
    );
}

#[test]
fn exported_defaults_include_all_roles_as_literal_colors() {
    let resolved = ThemeLayer::resolved(&ThemeLayer::default(), &ThemeLayer::default());
    assert_eq!(resolved.roles.len(), ThemeRole::ALL.len());
    for role in ThemeRole::ALL {
        let spec = &resolved.roles[role];
        assert!(
            !matches!(spec.foreground, Some(ThemeColor::Palette(_))),
            "{role:?}"
        );
        assert!(
            !matches!(spec.background, Some(ThemeColor::Palette(_))),
            "{role:?}"
        );
    }
}
