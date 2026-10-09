use indexmap::IndexMap;
use spar::ConfigValue;
use sparsh_core::{ColorSpec, PaletteKey, ThemeColor, ThemeLayer, ThemeRole};

fn object(fields: &[(&str, ConfigValue)]) -> ConfigValue {
    ConfigValue::Object(
        fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect::<IndexMap<_, _>>(),
    )
}

#[test]
fn parses_palette_references_and_background_overrides() {
    let value = object(&[
        (
            "palette",
            object(&[("accent", ConfigValue::Str("#abcdef".into()))]),
        ),
        (
            "completion",
            object(&[(
                "menuSelected",
                object(&[
                    ("foreground", ConfigValue::Str("$accent".into())),
                    ("background", ConfigValue::Str("black".into())),
                    ("bold", ConfigValue::Bool(false)),
                ]),
            )]),
        ),
    ]);
    let layer = ThemeLayer::parse(&value, "config.theme").unwrap();
    assert_eq!(
        layer.palette[&PaletteKey::Accent],
        ColorSpec::Rgb(0xab, 0xcd, 0xef)
    );
    let selected = &layer.roles[&ThemeRole::MenuSelected];
    assert_eq!(
        selected.foreground,
        Some(ThemeColor::Palette(PaletteKey::Accent))
    );
    assert_eq!(
        selected.background,
        Some(ThemeColor::Literal(ColorSpec::Indexed(0)))
    );
    assert_eq!(selected.bold, Some(false));
}

#[test]
fn errors_name_the_full_path_and_suggest_roles() {
    let value = object(&[("completion", object(&[("menuSelectd", object(&[]))]))]);
    let error = ThemeLayer::parse(&value, "config.theme").unwrap_err();
    assert!(
        error.contains("config.theme.completion.menuSelectd"),
        "{error}"
    );
    assert!(error.contains("menuSelected"), "{error}");
}

#[test]
fn rejects_invalid_color_and_unknown_palette_reference() {
    for color in ["#oops", "$accnt"] {
        let value = object(&[(
            "completion",
            object(&[(
                "menuSelected",
                object(&[("background", ConfigValue::Str(color.into()))]),
            )]),
        )]);
        let error = ThemeLayer::parse(&value, "config.theme").unwrap_err();
        assert!(
            error.contains("config.theme.completion.menuSelected.background"),
            "{error}"
        );
    }
}

#[test]
fn resolved_export_bakes_palette_and_role_precedence() {
    let mut config = ThemeLayer::default();
    config
        .palette
        .insert(PaletteKey::Accent, ColorSpec::Indexed(201));
    config.roles.insert(
        ThemeRole::Cwd,
        sparsh_core::ThemeStyleSpec {
            foreground: Some(ThemeColor::Literal(ColorSpec::Indexed(22))),
            ..Default::default()
        },
    );
    let mut generated = ThemeLayer::default();
    generated
        .palette
        .insert(PaletteKey::Accent, ColorSpec::Indexed(31));
    let resolved = ThemeLayer::resolved(&config, &generated);
    assert_eq!(
        resolved.roles[&ThemeRole::Cwd].foreground,
        Some(ThemeColor::Literal(ColorSpec::Indexed(31)))
    );
    assert_eq!(resolved.palette[&PaletteKey::Accent], ColorSpec::Indexed(31));
    assert_eq!(resolved.roles.len(), ThemeRole::ALL.len());
}

#[test]
fn generated_role_override_beats_config_role_override() {
    let style = |n| sparsh_core::ThemeStyleSpec {
        foreground: Some(ThemeColor::Literal(ColorSpec::Indexed(n))),
        ..Default::default()
    };
    let mut config = ThemeLayer::default();
    config.roles.insert(ThemeRole::Cwd, style(22));
    let mut generated = ThemeLayer::default();
    generated.roles.insert(ThemeRole::Cwd, style(44));
    let resolved = ThemeLayer::resolved(&config, &generated);
    assert_eq!(
        resolved.roles[&ThemeRole::Cwd].foreground,
        Some(ThemeColor::Literal(ColorSpec::Indexed(44)))
    );
}

#[test]
fn generated_palette_beats_config_palette_for_unoverridden_roles() {
    let mut config = ThemeLayer::default();
    config.palette.insert(PaletteKey::Accent, ColorSpec::Indexed(201));
    let mut generated = ThemeLayer::default();
    generated.palette.insert(PaletteKey::Accent, ColorSpec::Indexed(31));
    let resolved = ThemeLayer::resolved(&config, &generated);
    assert_eq!(
        resolved.roles[&ThemeRole::Cwd].foreground,
        Some(ThemeColor::Literal(ColorSpec::Indexed(31)))
    );
}
