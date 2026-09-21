use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch};
use sparsh_core::{PromptConfig, SlotConfig, SlotSpec, TextStyle, WidgetKind};

use crate::project::ProjectKind;
use crate::sampler::SystemSnapshot;
use crate::theme::{SemanticRole, Theme};
use crate::widgets::{render_slot, Level, LocalTime, RenderedSlot, WidgetInputs};
use crate::width::{display_width, display_width_with_glyphs, fold_path};

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
    /// The current local time, for `{time}` and `{date}` widgets.
    pub now: Option<LocalTime>,
    /// Number of background jobs, for `{jobs}`.
    pub jobs: usize,
    /// Machine readings for the widgets the config uses.
    pub system: SystemSnapshot,
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

    pub fn prompt(&self, data: &PromptData, config: &PromptConfig, theme: &Theme) -> SparshPrompt {
        let width = data.terminal_width.max(12);
        let prefix = "╭─ ";
        let separator = "  ";

        let mut right = self.right_blocks(data, config, theme);
        let mut git_mode = if config.git.enabled && data.git.is_some() {
            GitMode::Full
        } else {
            GitMode::Hidden
        };

        // First preserve full Git information and shed right-side optional
        // information (time, then duration, then status) until the first line
        // has a useful cwd budget.
        while !right.is_empty()
            && minimum_first_line_width(
                prefix,
                separator,
                data,
                config,
                git_mode,
                right_width(&right, config),
            ) > width
        {
            right.pop();
        }

        // If the repository segment is still too wide, degrade predictably:
        // full counters -> branch only -> hidden.  Unlike the previous prompt,
        // this mode is shared by width calculation and colored rendering.
        let right_cells = right_width(&right, config);
        if minimum_first_line_width(prefix, separator, data, config, git_mode, right_cells) > width
        {
            git_mode = GitMode::BranchOnly;
        }
        if minimum_first_line_width(prefix, separator, data, config, git_mode, right_cells) > width
        {
            git_mode = GitMode::Hidden;
        }

        let context_segments = context_segments(data, config, git_mode);
        let context_plain = join_plain(&context_segments, " ");

        let reserved = display_width(prefix)
            + if context_plain.is_empty() {
                0
            } else {
                display_width(separator) + display_width(&context_plain)
            }
            + if right.is_empty() {
                0
            } else {
                display_width(separator) + right_cells
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
        let right_colored = right
            .iter()
            .map(|block| block.colored.as_str())
            .collect::<Vec<_>>()
            .join(&config.right.separator);

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
            let spaces = width.saturating_sub(left_plain_width + right_cells).max(1);
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

    /// The right block, in display order: the fixed status marker, then the
    /// three user slots. Hidden slots are skipped; a broken slot shows a red
    /// marker so the user can see which one needs fixing.
    fn right_blocks(
        &self,
        data: &PromptData,
        config: &PromptConfig,
        theme: &Theme,
    ) -> Vec<RightBlock> {
        let mut blocks = Vec::new();
        if config.show_status && data.previous_status != 0 {
            let plain = format!("✕ {}", data.previous_status);
            blocks.push(RightBlock {
                colored: theme.paint(SemanticRole::Failure, &plain),
                plain,
            });
        }
        let inputs = WidgetInputs {
            now: data.now,
            last_duration: data.previous_duration,
            duration_threshold: self.slow_threshold,
            jobs: data.jobs,
            system: &data.system,
        };
        for (index, slot) in config.right.slots.iter().enumerate() {
            match slot {
                None => {}
                Some(SlotConfig::Broken { .. }) => {
                    let plain = format!("✕ slot{}", index + 1);
                    blocks.push(RightBlock {
                        colored: theme.paint(SemanticRole::Failure, &plain),
                        plain,
                    });
                }
                Some(SlotConfig::Ok(spec)) => {
                    if let Some(rendered) =
                        render_slot(&spec.template, &inputs, &config.right.thresholds)
                    {
                        blocks.push(paint_slot(spec, &rendered, theme));
                    }
                }
            }
        }
        blocks
    }
}

/// One piece of the right side of the first line, in plain and painted form.
struct RightBlock {
    plain: String,
    colored: String,
}

fn right_width(blocks: &[RightBlock], config: &PromptConfig) -> usize {
    if blocks.is_empty() {
        return 0;
    }
    let glyph = config.right.glyph_width;
    blocks
        .iter()
        .map(|block| display_width_with_glyphs(&block.plain, glyph))
        .sum::<usize>()
        + display_width_with_glyphs(&config.right.separator, glyph) * (blocks.len() - 1)
}

/// The theme role a widget uses when the slot sets no color.
fn widget_role(kind: WidgetKind) -> SemanticRole {
    match kind {
        WidgetKind::Time | WidgetKind::Date => SemanticRole::Time,
        WidgetKind::Duration => SemanticRole::Duration,
        _ => SemanticRole::Secondary,
    }
}

/// Paints a rendered slot. Precedence per piece: threshold level, then the
/// slot's own color/style, then the widget's default theme role.
fn paint_slot(spec: &SlotSpec, rendered: &RenderedSlot, theme: &Theme) -> RightBlock {
    let slot_role = rendered
        .pieces
        .iter()
        .find_map(|piece| piece.kind)
        .map_or(SemanticRole::Secondary, widget_role);
    let mut colored = String::new();
    for piece in &rendered.pieces {
        if piece.text.is_empty() {
            continue;
        }
        colored.push_str(&match piece.level {
            Level::Critical => theme.paint(SemanticRole::Failure, &piece.text),
            Level::Warn => theme.paint(SemanticRole::Warning, &piece.text),
            Level::Normal if spec.color.is_some() => {
                theme.paint_spec(spec.color, spec.style, &piece.text)
            }
            Level::Normal => {
                let role = piece.kind.map_or(slot_role, widget_role);
                if spec.style == TextStyle::default() {
                    theme.paint(role, &piece.text)
                } else {
                    theme.paint_role_styled(role, spec.style, &piece.text)
                }
            }
        });
    }
    RightBlock {
        plain: rendered.plain(),
        colored,
    }
}

fn minimum_first_line_width(
    prefix: &str,
    separator: &str,
    data: &PromptData,
    config: &PromptConfig,
    git_mode: GitMode,
    right_width: usize,
) -> usize {
    let context = join_plain(&context_segments(data, config, git_mode), " ");
    // Reserve at least one display column for cwd so the prompt never lets
    // metadata consume the entire input line.
    display_width(prefix)
        + 1
        + if context.is_empty() {
            0
        } else {
            display_width(separator) + display_width(&context)
        }
        + if right_width == 0 {
            0
        } else {
            display_width(separator) + right_width
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
        segments.push((SemanticRole::VirtualEnvironment, format!(" {environment}")));
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
    if segments.len() == 1 && git.staged + git.modified + git.untracked + git.conflicts == 0 {
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

fn paint_segments(segments: &[(SemanticRole, String)], separator: &str, theme: &Theme) -> String {
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

    use sparsh_core::{PromptConfig, RightPromptConfig, SlotConfig, SlotSpec, Template, TextStyle};

    use super::{GitState, PromptData, PromptState};
    use crate::project::ProjectKind;
    use crate::sampler::SystemSnapshot;
    use crate::theme::{SemanticRole, Theme};
    use crate::widgets::LocalTime;
    use crate::width::display_width;

    fn slot(template: &str) -> Option<SlotConfig> {
        Some(SlotConfig::Ok(SlotSpec {
            template: Template::parse(template).unwrap(),
            color: None,
            style: TextStyle::default(),
        }))
    }

    fn config_with(slots: [Option<SlotConfig>; 3], separator: &str) -> PromptConfig {
        let mut config = PromptConfig::default();
        config.right = RightPromptConfig {
            slots,
            separator: separator.into(),
            ..config.right
        };
        config
    }

    fn first_line(prompt: &super::SparshPrompt) -> &str {
        prompt.left.lines().next().unwrap()
    }

    fn data(width: usize) -> PromptData {
        PromptData {
            cwd: PathBuf::from("/home/occ/Projects/Rust/occ_lang/sparsh/crates/sparsh-core"),
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
            now: Some(LocalTime::from_utc_parts(2026, 9, 20, 5, 31, 46)),
            jobs: 0,
            system: SystemSnapshot::default(),
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
        assert_eq!(
            first.matches('').count(),
            1,
            "python icon should not be duplicated: {first}"
        );
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
        assert!(
            !first.contains("+1"),
            "full git counters should be compacted: {first}"
        );
        assert!(
            first.contains("~/") || first.contains("~"),
            "path shape disappeared: {first}"
        );
    }

    #[test]
    fn default_config_output_is_unchanged_from_the_legacy_layout() {
        // Captured from the renderer before right-prompt slots existed.
        let prompt = PromptState::new(Duration::from_secs(2)).prompt(
            &data(160),
            &PromptConfig::default(),
            &Theme::plain(),
        );
        let first = first_line(&prompt);
        assert!(
            first.starts_with(
                "╭─ ~/Projects/Rust/occ_lang/sparsh/crates/sparsh-core  \u{e7a8} \u{e0a0} main +1 ~2 ?3 !1 ↑4 ↓5"
            ),
            "{first:?}"
        );
        assert!(first.ends_with("✕ 7  3s  05:31:46"), "{first:?}");
        assert_eq!(display_width(first), 160, "{first:?}");

        let narrow = PromptState::new(Duration::from_secs(2)).prompt(
            &data(28),
            &PromptConfig::default(),
            &Theme::plain(),
        );
        assert_eq!(
            first_line(&narrow),
            "╭─ ~/…/…/…/sparsh…  \u{e7a8} \u{e0a0} main"
        );
    }

    #[test]
    fn custom_slots_render_in_order_after_the_status() {
        let mut d = data(160);
        d.system.host = Some("arch.local".into());
        let config = config_with([slot("{host}"), slot("mid"), slot("{date:%d %b}")], " | ");
        let prompt = PromptState::new(Duration::from_secs(2)).prompt(&d, &config, &Theme::plain());
        let first = first_line(&prompt);
        assert!(first.ends_with("✕ 7 | arch | mid | 20 Sep"), "{first:?}");
    }

    #[test]
    fn hidden_slots_leave_no_separator_gaps_or_stray_glyphs() {
        let config = config_with([slot("  {jobs}"), slot("mid"), None], " | ");
        let mut d = data(160);
        d.previous_status = 0;
        let prompt = PromptState::new(Duration::from_secs(2)).prompt(&d, &config, &Theme::plain());
        let first = first_line(&prompt);
        assert!(first.ends_with("  mid"), "{first:?}");
        assert!(!first.contains(" | "), "{first:?}");
        assert!(!first.contains("\u{f303}"), "{first:?}");
    }

    #[test]
    fn broken_slot_renders_a_red_marker_in_place_and_others_render() {
        let config = config_with(
            [
                slot("one"),
                Some(SlotConfig::Broken {
                    message: "unknown widget 'cpuu'".into(),
                }),
                slot("three"),
            ],
            " ",
        );
        let mut d = data(160);
        d.previous_status = 0;
        let plain = PromptState::new(Duration::from_secs(2)).prompt(&d, &config, &Theme::plain());
        assert!(
            first_line(&plain).ends_with("one ✕ slot2 three"),
            "{:?}",
            first_line(&plain)
        );
        assert!(
            !plain.left.contains('\u{1b}'),
            "plain theme must not emit escapes"
        );

        let colored =
            PromptState::new(Duration::from_secs(2)).prompt(&d, &config, &Theme::colored());
        let marker = Theme::colored().paint(SemanticRole::Failure, "✕ slot2");
        assert!(colored.left.contains(&marker), "{:?}", colored.left);
    }

    #[test]
    fn width_pressure_sheds_slot3_then_slot2_then_slot1_then_status_and_never_overflows() {
        let config = config_with([slot("AAAA"), slot("BBBB"), slot("CCCC")], "  ");
        for width in [120usize, 60, 30, 12] {
            let prompt = PromptState::new(Duration::from_secs(2)).prompt(
                &data(width),
                &config,
                &Theme::plain(),
            );
            assert!(
                display_width(first_line(&prompt)) <= width,
                "width {width}: {:?}",
                first_line(&prompt)
            );
        }
        // Shrink one column at a time: whatever survives is always a prefix of
        // the full right block (status, slot1, slot2, slot3), never a suffix.
        let full = "✕ 7  AAAA  BBBB  CCCC";
        for width in 12..=120 {
            let prompt = PromptState::new(Duration::from_secs(2)).prompt(
                &data(width),
                &config,
                &Theme::plain(),
            );
            let first = first_line(&prompt);
            let survivors: Vec<&str> = ["✕ 7", "AAAA", "BBBB", "CCCC"]
                .into_iter()
                .filter(|token| first.contains(token))
                .collect();
            let expected: Vec<&str> = ["✕ 7", "AAAA", "BBBB", "CCCC"]
                .into_iter()
                .take(survivors.len())
                .collect();
            assert_eq!(
                survivors, expected,
                "width {width}: {first:?} (full block {full:?})"
            );
        }
    }

    #[test]
    fn glyph_width_two_reserves_an_extra_cell_per_glyph() {
        let mut one = config_with([slot("\u{f303}"), None, None], "  ");
        one.right.glyph_width = 1;
        let mut two = one.clone();
        two.right.glyph_width = 2;
        let mut d = data(160);
        d.previous_status = 0;
        let p1 = PromptState::new(Duration::from_secs(2)).prompt(&d, &one, &Theme::plain());
        let p2 = PromptState::new(Duration::from_secs(2)).prompt(&d, &two, &Theme::plain());
        // Same terminal width; a wide glyph needs one fewer padding space so the
        // line still ends at the terminal edge.
        let (a, b) = (first_line(&p1).to_string(), first_line(&p2).to_string());
        let pad = |line: &str| line.matches(' ').count();
        assert_eq!(pad(&a), pad(&b) + 1, "{a:?} vs {b:?}");
    }

    #[test]
    fn slot_color_and_thresholds_take_precedence_over_role_colors() {
        use sparsh_core::ColorSpec;
        let config = config_with(
            [
                Some(SlotConfig::Ok(SlotSpec {
                    template: Template::parse("{cpu}%").unwrap(),
                    color: Some(ColorSpec::Indexed(6)),
                    style: TextStyle::default(),
                })),
                None,
                None,
            ],
            "  ",
        );
        let mut calm = data(160);
        calm.previous_status = 0;
        calm.system.cpu_busy = Some(10.0);
        let mut hot = data(160);
        hot.previous_status = 0;
        hot.system.cpu_busy = Some(95.0);
        let t = Theme::colored();
        let calm_line = PromptState::new(Duration::from_secs(2)).prompt(&calm, &config, &t);
        let hot_line = PromptState::new(Duration::from_secs(2)).prompt(&hot, &config, &t);
        assert!(
            calm_line.left.contains(&t.paint_spec(
                Some(ColorSpec::Indexed(6)),
                TextStyle::default(),
                "10"
            )),
            "{:?}",
            calm_line.left
        );
        assert!(
            hot_line
                .left
                .contains(&t.paint(SemanticRole::Failure, "95")),
            "critical level overrides the slot color: {:?}",
            hot_line.left
        );
    }
}
