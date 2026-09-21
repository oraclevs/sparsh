use nu_ansi_term::{Color, Style};
use sparsh_core::{ColorSpec, TextStyle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticRole {
    Cwd,
    GitBranch,
    GitDirty,
    GitClean,
    GitStaged,
    GitModified,
    GitUntracked,
    GitConflict,
    GitAhead,
    GitBehind,
    Success,
    Time,
    Failure,
    Duration,
    PromptMarker,
    Secondary,
    Warning,
    Error,
    Builtin,
    Alias,
    ExternalCommand,
    UnknownCommand,
    Argument,
    Path,
    Function,
    Parameter,
    Option,
    QuotedString,
    Operator,
    SparSyntax,
    VirtualEnvironment,
    ProjectPython,
    ProjectRust,
    ProjectFlutter,
    ProjectDart,
    ProjectNode,
    ProjectGo,
    /// Keys of records, JSON objects, YAML and TOML.
    DataKey,
    DataString,
    DataNumber,
    DataBool,
    DataNull,
    /// Brackets, commas, colons and other structure in rendered data.
    DataPunct,
    TableHeader,
    TableIndex,
    TableBorder,
}

#[derive(Clone, Debug)]
pub struct Theme {
    enabled: bool,
}

impl Theme {
    pub fn colored() -> Self {
        Self { enabled: true }
    }

    pub fn plain() -> Self {
        Self { enabled: false }
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
