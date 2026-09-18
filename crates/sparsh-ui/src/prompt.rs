use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch};

use crate::theme::{SemanticRole, Theme};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitState {
    pub branch: String,
    pub dirty: bool,
}

pub struct PromptData {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub git: Option<GitState>,
    pub previous_status: i32,
    pub previous_duration: Option<Duration>,
}

pub struct PromptState {
    slow_threshold: Duration,
}

impl PromptState {
    pub fn new(slow_threshold: Duration) -> Self {
        Self { slow_threshold }
    }

    pub fn render_left(&self, data: &PromptData, theme: &Theme) -> String {
        let mut segments = vec![theme.paint(
            SemanticRole::Cwd,
            &abbreviate_home(&data.cwd, data.home.as_deref()),
        )];
        if let Some(git) = &data.git {
            let mut branch = theme.paint(SemanticRole::GitBranch, &git.branch);
            if git.dirty {
                branch.push_str(&theme.paint(SemanticRole::GitDirty, "*"));
            }
            segments.push(branch);
        }
        if data.previous_status != 0 {
            segments.push(theme.paint(
                SemanticRole::Failure,
                &format!("✕ {}", data.previous_status),
            ));
        }
        if let Some(duration) = data
            .previous_duration
            .filter(|duration| *duration >= self.slow_threshold)
        {
            segments.push(theme.paint(SemanticRole::Duration, &format_duration(duration)));
        }
        segments.join("  ")
    }

    pub fn prompt(&self, data: &PromptData, theme: &Theme) -> SparshPrompt {
        SparshPrompt {
            left: format!("{}  ", self.render_left(data, theme)),
            indicator: theme.paint(SemanticRole::PromptMarker, "❯ "),
        }
    }
}

fn abbreviate_home(cwd: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return cwd.display().to_string();
    };
    if cwd == home {
        return "~".into();
    }
    match cwd.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => cwd.display().to_string(),
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds.fract() < 0.05 {
        format!("{}s", seconds.round() as u64)
    } else {
        format!("{seconds:.1}s")
    }
}

pub struct SparshPrompt {
    left: String,
    indicator: String,
}

impl Prompt for SparshPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(&self.indicator)
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Owned(format!("(reverse-search: {}) ", history_search.term))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use reedline::{Prompt, PromptEditMode};

    use crate::theme::Theme;

    use super::{GitState, PromptData, PromptState};

    #[test]
    fn plain_prompt_abbreviates_home_and_omits_quiet_success() {
        let prompt = PromptState::new(Duration::from_secs(2));
        let data = PromptData {
            cwd: PathBuf::from("/home/alice/code/sparsh"),
            home: Some(PathBuf::from("/home/alice")),
            git: Some(GitState {
                branch: "main".into(),
                dirty: false,
            }),
            previous_status: 0,
            previous_duration: Some(Duration::from_millis(50)),
        };

        assert_eq!(
            prompt.render_left(&data, &Theme::plain()),
            "~/code/sparsh  main"
        );
    }

    #[test]
    fn plain_prompt_shows_failure_dirty_state_and_slow_duration() {
        let prompt = PromptState::new(Duration::from_secs(2));
        let data = PromptData {
            cwd: PathBuf::from("/work"),
            home: None,
            git: Some(GitState {
                branch: "topic".into(),
                dirty: true,
            }),
            previous_status: 101,
            previous_duration: Some(Duration::from_millis(2800)),
        };

        assert_eq!(
            prompt.render_left(&data, &Theme::plain()),
            "/work  topic*  ✕ 101  2.8s"
        );
    }

    #[test]
    fn reedline_prompt_keeps_state_left_of_the_marker() {
        let state = PromptState::new(Duration::from_secs(2));
        let data = PromptData {
            cwd: PathBuf::from("/work"),
            home: None,
            git: None,
            previous_status: 0,
            previous_duration: None,
        };

        let prompt = state.prompt(&data, &Theme::plain());

        assert_eq!(prompt.render_prompt_left(), "/work  ");
        assert_eq!(prompt.render_prompt_indicator(PromptEditMode::Emacs), "❯ ");
    }
}
