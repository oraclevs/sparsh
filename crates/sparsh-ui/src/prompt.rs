use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch};
use sparsh_core::PromptConfig;

use crate::project::ProjectKind;
use crate::theme::{SemanticRole, Theme};
use crate::width::{display_width, fold_path};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitState {
    pub branch: String,
    pub staged: usize,
    pub modified: usize,
    pub untracked: usize,
    pub conflicts: usize,
    pub ahead: usize,
    pub behind: usize,
}

pub struct PromptData {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub git: Option<GitState>,
    pub projects: Vec<ProjectKind>,
    pub python_environment: Option<String>,
    pub previous_status: i32,
    pub previous_duration: Option<Duration>,
    pub terminal_width: usize,
    pub current_time: Option<String>,
}

pub struct PromptState {
    slow_threshold: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitMode {
    Full,
    BranchOnly,
    Hidden,
}

impl PromptState {
    pub fn new(slow_threshold: Duration) -> Self {
        Self { slow_threshold }
    }

    pub fn prompt(
        &self,
        data: &PromptData,
        config: &PromptConfig,
        theme: &Theme,
    ) -> SparshPrompt {
        let width = data.terminal_width.max(12);
        let prefix = "╭─ ";
        let separator = "  ";

        let mut right_segments = self.right_segments(data, config);
        let mut git_mode = if config.git.enabled && data.git.is_some() {
            GitMode::Full
        } else {
            GitMode::Hidden
        };

        // First preserve full Git information and shed right-side optional
        // information (time, then duration, then status) until the first line
        // has a useful cwd budget.
        while !right_segments.is_empty()
            && minimum_first_line_width(prefix, separator, data, config, git_mode, &right_segments)
                > width
        {
            right_segments.pop();
        }

        // If the repository segment is still too wide, degrade predictably:
        // full counters -> branch only -> hidden.  Unlike the previous prompt,
        // this mode is shared by width calculation and colored rendering.
        if minimum_first_line_width(prefix, separator, data, config, git_mode, &right_segments)
            > width
        {
            git_mode = GitMode::BranchOnly;
        }
        if minimum_first_line_width(prefix, separator, data, config, git_mode, &right_segments)
            > width
        {
            git_mode = GitMode::Hidden;
        }

        let context_segments = context_segments(data, config, git_mode);
        let context_plain = join_plain(&context_segments, " ");
        let right_plain = join_plain(&right_segments, separator);

        let reserved = display_width(prefix)
            + if context_plain.is_empty() {
                0
            } else {
                display_width(separator) + display_width(&context_plain)
            }
            + if right_plain.is_empty() {
                0
            } else {
                display_width(separator) + display_width(&right_plain)
            };
        let path_budget = width
            .saturating_sub(reserved)
            .max(1)
            .min(config.path.max_width.max(1));
        let path = if config.path.enabled {
            fold_path(
                &data.cwd,
                data.home.as_deref(),
                path_budget,
                config.path.parent_length,
                config.path.max_last_length,
            )
        } else {
            String::new()
        };

        let path_colored = theme.paint(SemanticRole::Cwd, &path);
        let context_colored = paint_segments(&context_segments, " ", theme);
        let right_colored = paint_segments(&right_segments, separator, theme);

        let left_plain_width = display_width(prefix)
            + display_width(&path)
            + if context_plain.is_empty() {
                0
            } else {
                display_width(separator) + display_width(&context_plain)
            };

        let mut first_line = format!("{prefix}{path_colored}");
        if !context_colored.is_empty() {
            first_line.push_str(separator);
            first_line.push_str(&context_colored);
        }
        if !right_colored.is_empty() {
            let spaces = width
                .saturating_sub(left_plain_width + display_width(&right_plain))
                .max(1);
            first_line.push_str(&" ".repeat(spaces));
            first_line.push_str(&right_colored);
        }

        // The width budget above normally guarantees this already.  On very
        // small terminals with an unusually wide glyph, keep the prompt safe
        // by dropping the optional right segment rather than slicing ANSI.
        if display_width(&strip_ansi_for_width(&first_line)) > width && !right_colored.is_empty() {
            first_line = format!("{prefix}{path_colored}");
            if !context_colored.is_empty() {
                first_line.push_str(separator);
                first_line.push_str(&context_colored);
            }
        }

        first_line.push_str("\n╰─ ");
        SparshPrompt {
            left: first_line,
            right: String::new(),
            indicator: theme.paint(SemanticRole::PromptMarker, "❯ "),
        }
    }

    fn right_segments(
        &self,
        data: &PromptData,
        config: &PromptConfig,
    ) -> Vec<(SemanticRole, String)> {
        let mut segments = Vec::new();
        if config.show_status && data.previous_status != 0 {
            segments.push((SemanticRole::Failure, format!("✕ {}", data.previous_status)));
        }
        if config.show_duration {
            if let Some(duration) = data
                .previous_duration
                .filter(|duration| *duration >= self.slow_threshold)
            {
                segments.push((SemanticRole::Duration, format_duration(duration)));
            }
        }
        if config.time.enabled {
            if let Some(time) = &data.current_time {
                segments.push((SemanticRole::Time, time.clone()));
            }
        }
        segments
    }
}

fn minimum_first_line_width(
    prefix: &str,
    separator: &str,
    data: &PromptData,
    config: &PromptConfig,
    git_mode: GitMode,
    right: &[(SemanticRole, String)],
) -> usize {
    let context = join_plain(&context_segments(data, config, git_mode), " ");
    let right = join_plain(right, separator);
    // Reserve at least one display column for cwd so the prompt never lets
    // metadata consume the entire input line.
    display_width(prefix)
        + 1
        + if context.is_empty() {
            0
        } else {
            display_width(separator) + display_width(&context)
        }
        + if right.is_empty() {
            0
        } else {
            display_width(separator) + display_width(&right)
        }
}

fn context_segments(
    data: &PromptData,
    config: &PromptConfig,
    git_mode: GitMode,
) -> Vec<(SemanticRole, String)> {
    let mut segments = Vec::new();

    for project in &data.projects {
        // An active Python environment already carries the Python glyph, so
        // avoid showing the same icon twice for the common Python-project case.
        if *project == ProjectKind::Python && data.python_environment.is_some() {
            continue;
        }
        segments.push((project.role(), project.icon().to_string()));
    }

    if let Some(environment) = &data.python_environment {
        segments.push((
            SemanticRole::VirtualEnvironment,
            format!(" {environment}"),
        ));
    }

    segments.extend(git_segments(data.git.as_ref(), config, git_mode));
    segments
}

fn git_segments(
    git: Option<&GitState>,
    config: &PromptConfig,
    mode: GitMode,
) -> Vec<(SemanticRole, String)> {
    let Some(git) = git else {
        return Vec::new();
    };
    if mode == GitMode::Hidden {
        return Vec::new();
    }

    let mut segments = Vec::new();
    if config.git.show_branch && !git.branch.is_empty() {
        segments.push((SemanticRole::GitBranch, format!(" {}", git.branch)));
    }
    if mode == GitMode::BranchOnly {
        return segments;
    }
    if config.git.show_staged && git.staged > 0 {
        segments.push((SemanticRole::GitStaged, format!("+{}", git.staged)));
    }
    if config.git.show_modified && git.modified > 0 {
        segments.push((SemanticRole::GitModified, format!("~{}", git.modified)));
    }
    if config.git.show_untracked && git.untracked > 0 {
        segments.push((SemanticRole::GitUntracked, format!("?{}", git.untracked)));
    }
    if config.git.show_conflicts && git.conflicts > 0 {
        segments.push((SemanticRole::GitConflict, format!("!{}", git.conflicts)));
    }
    if config.git.show_ahead_behind && git.ahead > 0 {
        segments.push((SemanticRole::GitAhead, format!("↑{}", git.ahead)));
    }
    if config.git.show_ahead_behind && git.behind > 0 {
        segments.push((SemanticRole::GitBehind, format!("↓{}", git.behind)));
    }
    if segments.len() == 1
        && git.staged + git.modified + git.untracked + git.conflicts == 0
    {
        segments.push((SemanticRole::GitClean, "✓".into()));
    }
    segments
}

fn join_plain(segments: &[(SemanticRole, String)], separator: &str) -> String {
    segments
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}

fn paint_segments(
    segments: &[(SemanticRole, String)],
    separator: &str,
    theme: &Theme,
) -> String {
    segments
        .iter()
        .map(|(role, text)| theme.paint(*role, text))
        .collect::<Vec<_>>()
        .join(separator)
}

// Prompt width decisions are always made from plain text before coloring.
// This helper is only a defensive assertion path; it recognizes the CSI SGR
// sequences emitted by nu-ansi-term without attempting to be a general ANSI
// parser.
fn strip_ansi_for_width(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            index += 2;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&byte) {
                    break;
                }
            }
            continue;
        }
        let Some(ch) = value[index..].chars().next() else {
            break;
        };
        out.push(ch);
        index += ch.len_utf8();
    }
    out
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
    right: String,
    indicator: String,
}

impl Prompt for SparshPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.right)
    }

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(&self.indicator)
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("· ")
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

    use sparsh_core::PromptConfig;

    use super::{GitState, PromptData, PromptState};
    use crate::project::ProjectKind;
    use crate::theme::Theme;
    use crate::width::display_width;

    fn data(width: usize) -> PromptData {
        PromptData {
            cwd: PathBuf::from(
                "/home/occ/Projects/Rust/occ_lang/sparsh/crates/sparsh-core",
            ),
            home: Some(PathBuf::from("/home/occ")),
            git: Some(GitState {
                branch: "main".into(),
                staged: 1,
                modified: 2,
                untracked: 3,
                conflicts: 1,
                ahead: 4,
                behind: 5,
            }),
            projects: vec![ProjectKind::Rust],
            python_environment: None,
            previous_status: 7,
            previous_duration: Some(Duration::from_secs(3)),
            terminal_width: width,
            current_time: Some("05:31:46".into()),
        }
    }

    #[test]
    fn wide_prompt_shows_path_shape_git_counters_status_duration_and_time() {
        let prompt = PromptState::new(Duration::from_secs(2)).prompt(
            &data(160),
            &PromptConfig::default(),
            &Theme::plain(),
        );
        let first = prompt.left.lines().next().unwrap();

        assert!(
            first.contains("~/Projects/Rust/occ_lang/sparsh/crates/sparsh-core"),
            "{first}"
        );
        assert!(first.contains(" main"), "{first}");
        for token in ["+1", "~2", "?3", "!1", "↑4", "↓5", "✕ 7", "3s", "05:31:46"] {
            assert!(first.contains(token), "missing {token} in {first}");
        }
    }

    #[test]
    fn prompt_shows_project_icon_and_active_python_environment() {
        let mut data = data(160);
        data.projects = vec![ProjectKind::Python, ProjectKind::Rust];
        data.python_environment = Some(".venv".into());

        let prompt = PromptState::new(Duration::from_secs(2)).prompt(
            &data,
            &PromptConfig::default(),
            &Theme::plain(),
        );
        let first = prompt.left.lines().next().unwrap();

        assert!(first.contains(" .venv"), "{first}");
        assert!(first.contains(""), "{first}");
        assert_eq!(first.matches('').count(), 1, "python icon should not be duplicated: {first}");
    }

    #[test]
    fn narrow_prompt_really_drops_git_counters_and_stays_within_width() {
        let width = 28;
        let prompt = PromptState::new(Duration::from_secs(2)).prompt(
            &data(width),
            &PromptConfig::default(),
            &Theme::plain(),
        );
        let first = prompt.left.lines().next().unwrap();

        assert!(display_width(first) <= width, "{first:?} is too wide");
        assert!(!first.contains("+1"), "full git counters should be compacted: {first}");
        assert!(first.contains("~/") || first.contains("~"), "path shape disappeared: {first}");
    }
}
