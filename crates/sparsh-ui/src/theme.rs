use nu_ansi_term::{Color, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticRole {
    Cwd,
    GitBranch,
    GitDirty,
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
    Option,
    QuotedString,
    Operator,
    SparSyntax,
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

    pub(crate) fn style(&self, role: SemanticRole) -> Style {
        if !self.enabled {
            return Style::new();
        }
        match role {
            SemanticRole::Cwd => Style::new().fg(Color::LightBlue).bold(),
            SemanticRole::GitBranch => Style::new().fg(Color::LightMagenta),
            SemanticRole::GitDirty | SemanticRole::Warning => Style::new().fg(Color::Yellow),
            SemanticRole::Failure | SemanticRole::Error | SemanticRole::UnknownCommand => {
                Style::new().fg(Color::LightRed).bold()
            }
            SemanticRole::Duration | SemanticRole::Secondary | SemanticRole::Argument => {
                Style::new().fg(Color::DarkGray)
            }
            SemanticRole::PromptMarker => Style::new().fg(Color::LightGreen).bold(),
            SemanticRole::Builtin => Style::new().fg(Color::LightCyan).bold(),
            SemanticRole::Alias => Style::new().fg(Color::LightMagenta).bold(),
            SemanticRole::ExternalCommand => Style::new().fg(Color::LightGreen),
            SemanticRole::Option => Style::new().fg(Color::LightYellow),
            SemanticRole::QuotedString => Style::new().fg(Color::LightGreen),
            SemanticRole::Operator => Style::new().fg(Color::LightBlue).bold(),
            SemanticRole::SparSyntax => Style::new().fg(Color::LightPurple).bold(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SemanticRole, Theme};

    #[test]
    fn colored_theme_uses_distinct_semantic_roles() {
        let theme = Theme::colored();

        assert_ne!(
            theme.paint(SemanticRole::Cwd, "same"),
            theme.paint(SemanticRole::Error, "same")
        );
        assert!(theme.paint(SemanticRole::Cwd, "cwd").contains("\x1b["));
        assert_eq!(Theme::plain().paint(SemanticRole::Cwd, "cwd"), "cwd");
    }
}
