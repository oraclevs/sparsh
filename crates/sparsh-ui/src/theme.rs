use nu_ansi_term::{Color, Style};
use sparsh_core::{ColorSpec, PaletteKey, TextStyle, ThemeColor, ThemeLayer, ThemeStyleSpec};

pub use sparsh_core::ThemeRole as SemanticRole;

#[derive(Clone, Debug)]
pub struct Theme {
    enabled: bool,
    file: ThemeLayer,
    config: ThemeLayer,
}

impl Theme {
    pub fn colored() -> Self {
        Self::from_layers(true, &ThemeLayer::default(), &ThemeLayer::default())
    }

    pub fn plain() -> Self {
        Self::from_layers(false, &ThemeLayer::default(), &ThemeLayer::default())
    }

    pub fn from_layers(enabled: bool, file: &ThemeLayer, config: &ThemeLayer) -> Self {
        Self { enabled, file: file.clone(), config: config.clone() }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn paint(&self, role: SemanticRole, text: &str) -> String {
        if self.enabled {
            self.style(role).paint(text).to_string()
        } else {
            text.to_string()
        }
    }

    /// Paints `text` with a user-configured color and text style. Nothing is
    /// emitted when colors are disabled or when there is nothing to apply.
    pub fn paint_spec(&self, color: Option<ColorSpec>, style: TextStyle, text: &str) -> String {
        if !self.enabled || (color.is_none() && style == TextStyle::default()) {
            return text.to_string();
        }
        let mut rendered = apply_text_style(Style::new(), style);
        if let Some(color) = color {
            rendered = rendered.fg(match color {
                ColorSpec::Indexed(index) => Color::Fixed(index),
                ColorSpec::Rgb(r, g, b) if truecolor_supported() => Color::Rgb(r, g, b),
                ColorSpec::Rgb(r, g, b) => Color::Fixed(rgb_to_ansi256(r, g, b)),
            });
        }
        rendered.paint(text).to_string()
    }

    /// A role's own color with extra text style flags on top (used when a slot
    /// sets `style` but no `color`).
    pub fn paint_role_styled(&self, role: SemanticRole, style: TextStyle, text: &str) -> String {
        if !self.enabled {
            return text.to_string();
        }
        apply_text_style(self.style(role), style)
            .paint(text)
            .to_string()
    }

    pub(crate) fn style(&self, role: SemanticRole) -> Style {
        if !self.enabled {
            return Style::new();
        }
        let mut style = self.default_style(role);
        // `file` is the tool-generated layer and outranks the user's config theme.
        let mut merged_palette = self.config.palette.clone();
        merged_palette.extend(self.file.palette.iter().map(|(key, value)| (*key, *value)));
        style = apply_layer(style, role, &self.config, &merged_palette);
        apply_layer(style, role, &self.file, &merged_palette)
    }

    fn default_style(&self, role: SemanticRole) -> Style {
        match role {
            SemanticRole::Cwd => Style::new().fg(Color::LightBlue).bold(),
            SemanticRole::GitBranch => Style::new().fg(Color::LightMagenta),
            SemanticRole::GitClean | SemanticRole::Success => {
                Style::new().fg(Color::LightGreen).bold()
            }
            SemanticRole::GitStaged => Style::new().fg(Color::LightGreen),
            SemanticRole::GitModified | SemanticRole::GitDirty | SemanticRole::Warning => {
                Style::new().fg(Color::Yellow)
            }
            SemanticRole::GitUntracked => Style::new().fg(Color::LightBlue),
            SemanticRole::GitConflict => Style::new().fg(Color::LightRed).bold(),
            SemanticRole::GitAhead => Style::new().fg(Color::LightCyan),
            SemanticRole::GitBehind => Style::new().fg(Color::LightMagenta),
            SemanticRole::Time => Style::new().fg(Color::LightCyan).bold(),
            SemanticRole::Failure | SemanticRole::Error | SemanticRole::UnknownCommand => {
                Style::new().fg(Color::LightRed).bold()
            }
            SemanticRole::Duration => Style::new().fg(Color::LightYellow),
            SemanticRole::Secondary => Style::new().fg(Color::DarkGray),
            SemanticRole::Argument => Style::new().fg(Color::White),
            SemanticRole::Path => Style::new().fg(Color::LightBlue),
            SemanticRole::Function => Style::new().fg(Color::LightGreen).bold(),
            SemanticRole::Parameter => Style::new().fg(Color::LightCyan),
            SemanticRole::PromptMarker => Style::new().fg(Color::LightGreen).bold(),
            SemanticRole::Builtin => Style::new().fg(Color::LightCyan).bold(),
            SemanticRole::Alias => Style::new().fg(Color::LightMagenta).bold(),
            SemanticRole::ExternalCommand => Style::new().fg(Color::LightGreen),
            SemanticRole::Option => Style::new().fg(Color::LightYellow),
            SemanticRole::QuotedString => Style::new().fg(Color::LightGreen),
            SemanticRole::Operator => Style::new().fg(Color::LightBlue).bold(),
            SemanticRole::SparSyntax => Style::new().fg(Color::LightPurple).bold(),
            SemanticRole::Comment => Style::new().fg(Color::DarkGray).italic(),
            SemanticRole::TypeName => Style::new().fg(Color::Yellow),
            SemanticRole::VirtualEnvironment => Style::new().fg(Color::LightMagenta).bold(),
            SemanticRole::ProjectPython => Style::new().fg(Color::LightBlue).bold(),
            SemanticRole::ProjectRust => Style::new().fg(Color::LightYellow).bold(),
            SemanticRole::ProjectFlutter | SemanticRole::ProjectDart | SemanticRole::ProjectGo => {
                Style::new().fg(Color::LightCyan).bold()
            }
            SemanticRole::ProjectNode => Style::new().fg(Color::LightGreen).bold(),
            SemanticRole::DataKey => Style::new().fg(Color::LightBlue).bold(),
            SemanticRole::DataString => Style::new().fg(Color::LightGreen),
            SemanticRole::DataNumber => Style::new().fg(Color::LightCyan),
            SemanticRole::DataBool => Style::new().fg(Color::LightMagenta).bold(),
            SemanticRole::DataNull => Style::new().fg(Color::DarkGray).italic(),
            SemanticRole::DataPunct | SemanticRole::TableBorder => Style::new().fg(Color::DarkGray),
            SemanticRole::TableHeader => Style::new().fg(Color::LightGreen).bold(),
            SemanticRole::TableIndex => Style::new().fg(Color::Green).bold(),
            SemanticRole::FileDirectory => Style::new().fg(Color::LightBlue).bold(),
            SemanticRole::FileExecutable => Style::new().fg(Color::LightRed).bold(),
            SemanticRole::FileSymlink => Style::new().fg(Color::LightCyan),
            SemanticRole::FileSpecial => Style::new().fg(Color::Yellow),
            // The completion roles default to the colors of the matching
            // highlight roles, so the menu agrees with the buffer.
            SemanticRole::CompletionFunction => self.default_style(SemanticRole::Function),
            SemanticRole::CompletionVariable => self.default_style(SemanticRole::Parameter),
            SemanticRole::CompletionType => self.default_style(SemanticRole::TypeName),
            SemanticRole::CompletionKeyword => self.default_style(SemanticRole::SparSyntax),
            SemanticRole::CompletionPath => self.default_style(SemanticRole::Path),
            SemanticRole::CompletionDirectory => self.default_style(SemanticRole::FileDirectory),
            SemanticRole::CompletionCommand => self.default_style(SemanticRole::ExternalCommand),
            SemanticRole::MenuBorder => Style::new().fg(Color::DarkGray),
            SemanticRole::MenuText => Style::new().fg(Color::White),
            SemanticRole::MenuSelected => Style::new().fg(Color::Black).on(Color::LightCyan).bold(),
            SemanticRole::MenuDetail => Style::new().dimmed(),
            SemanticRole::MenuFooter => Style::new().fg(Color::LightBlue),
            SemanticRole::MenuCount => Style::new().fg(Color::LightCyan).bold(),
            SemanticRole::MenuMatch => Style::new().fg(Color::LightYellow).bold(),
            SemanticRole::HintHistory => Style::new().fg(Color::Fixed(243)),
            SemanticRole::HintSignature => Style::new().fg(Color::DarkGray),
            SemanticRole::HintActive => Style::new().fg(Color::LightCyan).bold(),
            SemanticRole::HelpTitle => Style::new().fg(Color::LightCyan).bold(),
            SemanticRole::HelpHeading => Style::new().fg(Color::LightGreen).bold(),
            SemanticRole::HelpBody => Style::new(),
            SemanticRole::HelpOption => Style::new().fg(Color::LightYellow),
            SemanticRole::HelpExample => Style::new().fg(Color::LightGreen).bold(),
            SemanticRole::HelpCrossReference => Style::new().fg(Color::LightCyan).bold(),
            SemanticRole::EditorHeader => Style::new().fg(Color::Cyan).bold(),
            SemanticRole::EditorPosition => Style::new().fg(Color::White),
            SemanticRole::EditorStatus => Style::new().fg(Color::Black).on(Color::Cyan),
            SemanticRole::EditorSelection => Style::new().reverse(),
            SemanticRole::PagerStatus => Style::new().reverse(),
            SemanticRole::PagerSearch => Style::new().reverse(),
            SemanticRole::PagerSelected => Style::new().fg(Color::Black).on(Color::LightCyan),
        }
    }
}

fn apply_text_style(mut style: Style, spec: TextStyle) -> Style {
    if spec.bold {
        style = style.bold();
    }
    if spec.dim {
        style = style.dimmed();
    }
    if spec.italic {
        style = style.italic();
    }
    if spec.underline {
        style = style.underline();
    }
    style
}

/// True when the terminal advertises 24-bit color.
pub(crate) fn truecolor_supported() -> bool {
    std::env::var("COLORTERM")
        .map(|value| value.contains("truecolor") || value.contains("24bit"))
        .unwrap_or(false)
}

/// The nearest xterm-256 color: the 6x6x6 cube, or the grayscale ramp for
/// neutral colors.
pub(crate) fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        return if r < 8 {
            16
        } else if r > 248 {
            231
        } else {
            232 + ((u32::from(r) - 8 + 5) / 10).min(23) as u8
        };
    }
    let level = |value: u8| (f64::from(value) / 51.0).round() as u8;
    16 + 36 * level(r) + 6 * level(g) + level(b)
}

#[cfg(test)]
mod tests {
    use sparsh_core::{ColorSpec, TextStyle};

    use super::{rgb_to_ansi256, SemanticRole, Theme};

    #[test]
    fn exported_defaults_match_the_ui_default_style_table() {
        use nu_ansi_term::Color;
        use sparsh_core::{ThemeColor, ThemeLayer, ThemeRole};
        let exported = ThemeLayer::resolved(&ThemeLayer::default(), &ThemeLayer::default());
        let theme = Theme::colored();
        let index = |color: Color| -> Option<ColorSpec> {
            let value = match color {
                Color::Black => 0, Color::Red => 1, Color::Green => 2,
                Color::Yellow => 3, Color::Blue => 4, Color::Purple | Color::Magenta => 5,
                Color::Cyan => 6, Color::White => 7, Color::DarkGray => 8,
                Color::LightRed => 9, Color::LightGreen => 10, Color::LightYellow => 11,
                Color::LightBlue => 12, Color::LightPurple | Color::LightMagenta => 13,
                Color::LightCyan => 14, Color::LightGray => 15,
                Color::Fixed(value) => value,
                Color::Rgb(r, g, b) => return Some(ColorSpec::Rgb(r, g, b)),
                Color::Default => return None,
            };
            Some(ColorSpec::Indexed(value))
        };
        for role in ThemeRole::ALL {
            let actual = theme.style(*role);
            let spec = &exported.roles[role];
            assert_eq!(actual.foreground.and_then(index).map(ThemeColor::Literal), spec.foreground, "{role:?} foreground");
            assert_eq!(actual.background.and_then(index).map(ThemeColor::Literal), spec.background, "{role:?} background");
            assert_eq!(actual.is_bold, spec.bold.unwrap_or(false), "{role:?} bold");
            assert_eq!(actual.is_dimmed, spec.dim.unwrap_or(false), "{role:?} dim");
            assert_eq!(actual.is_italic, spec.italic.unwrap_or(false), "{role:?} italic");
        }
    }

    #[test]
    fn explicit_prompt_slot_style_has_the_last_word() {
        let mut file = sparsh_core::ThemeLayer::default();
        file.palette.insert(sparsh_core::PaletteKey::Info, ColorSpec::Indexed(31));
        let theme = Theme::from_layers(true, &file, &sparsh_core::ThemeLayer::default());
        assert_ne!(theme.paint(SemanticRole::Path, "x"), Theme::colored().paint(SemanticRole::Path, "x"));
        let slot = theme.paint_spec(Some(ColorSpec::Indexed(201)), TextStyle::default(), "x");
        assert!(slot.contains("38;5;201"), "{slot:?}");
        assert!(!slot.contains("38;5;31"), "{slot:?}");
    }

    #[test]
    fn pager_default_reverses_only_without_a_color_override() {
        let default = Theme::colored();
        assert!(default.style(SemanticRole::PagerStatus).is_reverse);
        let mut config = sparsh_core::ThemeLayer::default();
        config.roles.insert(SemanticRole::PagerStatus, sparsh_core::ThemeStyleSpec {
            background: Some(sparsh_core::ThemeColor::Literal(ColorSpec::Indexed(55))),
            ..Default::default()
        });
        let changed = Theme::from_layers(true, &sparsh_core::ThemeLayer::default(), &config);
        assert!(!changed.style(SemanticRole::PagerStatus).is_reverse);
        assert_eq!(changed.style(SemanticRole::PagerStatus).background, Some(nu_ansi_term::Color::Fixed(55)));
    }

    #[test]
    fn pager_search_color_override_keeps_colors_in_their_requested_positions() {
        let mut config = sparsh_core::ThemeLayer::default();
        config.roles.insert(SemanticRole::PagerSearch, sparsh_core::ThemeStyleSpec {
            foreground: Some(sparsh_core::ThemeColor::Literal(ColorSpec::Indexed(201))),
            background: Some(sparsh_core::ThemeColor::Literal(ColorSpec::Indexed(202))),
            ..Default::default()
        });
        let changed = Theme::from_layers(true, &sparsh_core::ThemeLayer::default(), &config);
        let style = changed.style(SemanticRole::PagerSearch);
        assert!(!style.is_reverse);
        assert_eq!(style.foreground, Some(nu_ansi_term::Color::Fixed(201)));
        assert_eq!(style.background, Some(nu_ansi_term::Color::Fixed(202)));
    }

    #[test]
    fn paint_spec_applies_color_and_style_or_nothing_when_plain() {
        let t = Theme::colored();
        let bold = TextStyle {
            bold: true,
            ..Default::default()
        };
        let bold_cyan = t.paint_spec(Some(ColorSpec::Indexed(6)), bold, "x");
        assert!(bold_cyan.contains("\x1b[") && bold_cyan.contains('x'));
        assert_ne!(
            bold_cyan,
            t.paint_spec(Some(ColorSpec::Indexed(6)), TextStyle::default(), "x")
        );
        assert_eq!(
            Theme::plain().paint_spec(Some(ColorSpec::Indexed(6)), bold, "x"),
            "x"
        );
        assert_eq!(t.paint_spec(None, TextStyle::default(), "x"), "x");
        assert_ne!(
            t.paint_role_styled(SemanticRole::Duration, bold, "x"),
            t.paint(SemanticRole::Duration, "x")
        );
        assert_eq!(
            Theme::plain().paint_role_styled(SemanticRole::Duration, bold, "x"),
            "x"
        );
    }

    #[test]
    fn rgb_downgrades_to_256_color_without_truecolor() {
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi256(128, 128, 128), 244);
    }

    #[test]
    fn colored_theme_uses_distinct_semantic_roles() {
        let theme = Theme::colored();

        assert_ne!(
            theme.paint(SemanticRole::Cwd, "same"),
            theme.paint(SemanticRole::Error, "same")
        );
        assert!(theme.paint(SemanticRole::Cwd, "cwd").contains("\x1b["));
        assert_ne!(
            theme.paint(SemanticRole::Time, "12:34:56"),
            theme.paint(SemanticRole::Secondary, "12:34:56")
        );
        assert_ne!(
            theme.paint(SemanticRole::Path, "~/Projects"),
            theme.paint(SemanticRole::Argument, "~/Projects")
        );
        assert_ne!(
            theme.paint(SemanticRole::Function, "create"),
            theme.paint(SemanticRole::UnknownCommand, "create")
        );
        assert_eq!(Theme::plain().paint(SemanticRole::Cwd, "cwd"), "cwd");
    }
}

fn palette_role(role: SemanticRole) -> PaletteKey {
    role.palette_key()
}

fn ansi_color(spec: ColorSpec) -> Color {
    match spec {
        ColorSpec::Indexed(index) => Color::Fixed(index),
        ColorSpec::Rgb(r, g, b) if truecolor_supported() => Color::Rgb(r, g, b),
        ColorSpec::Rgb(r, g, b) => Color::Fixed(rgb_to_ansi256(r, g, b)),
    }
}

fn apply_layer(
    mut style: Style,
    role: SemanticRole,
    layer: &ThemeLayer,
    merged_palette: &std::collections::BTreeMap<PaletteKey, ColorSpec>,
) -> Style {
    let mut has_color_override = false;
    if let Some(color) = layer.palette.get(&palette_role(role)) {
        style.foreground = Some(ansi_color(*color));
        has_color_override = true;
    }
    if palette_role(role) == PaletteKey::SelectionFg {
        if let Some(color) = layer.palette.get(&PaletteKey::SelectionBg) {
            style.background = Some(ansi_color(*color));
            has_color_override = true;
        }
    }
    if let Some(spec) = layer.roles.get(&role) {
        let resolve = |value: ThemeColor| match value {
            ThemeColor::Literal(color) => Some(color),
            ThemeColor::Palette(key) => merged_palette.get(&key).copied(),
        };
        if let Some(color) = spec.foreground.and_then(resolve) {
            style.foreground = Some(ansi_color(color));
            has_color_override = true;
        }
        if let Some(color) = spec.background.and_then(resolve) {
            style.background = Some(ansi_color(color));
            has_color_override = true;
        }
        merge_attributes(&mut style, spec);
    }
    if has_color_override && matches!(role, SemanticRole::PagerStatus | SemanticRole::PagerSearch | SemanticRole::EditorSelection) {
        style.is_reverse = false;
    }
    style
}

fn merge_attributes(style: &mut Style, spec: &ThemeStyleSpec) {
    if let Some(value) = spec.bold { style.is_bold = value; }
    if let Some(value) = spec.dim { style.is_dimmed = value; }
    if let Some(value) = spec.italic { style.is_italic = value; }
    if let Some(value) = spec.underline { style.is_underline = value; }
}
