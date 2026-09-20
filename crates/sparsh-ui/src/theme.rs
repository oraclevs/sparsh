use nu_ansi_term::{Color, Style};

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
            SemanticRole::GitClean | SemanticRole::Success => Style::new().fg(Color::LightGreen).bold(),
            SemanticRole::GitStaged => Style::new().fg(Color::LightGreen),
            SemanticRole::GitModified | SemanticRole::GitDirty | SemanticRole::Warning => Style::new().fg(Color::Yellow),
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
