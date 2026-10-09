//! The completion popup: a rounded, bordered box drawn under the prompt with
//! a marker column, label, dim detail, a right-aligned kind word, a footer
//! with the selected item's full detail and an `i/n` indicator.
//!
//! The layout is a pure function ([`render_box`]) so it can be tested without
//! a terminal; [`BorderedMenu`] adapts it to reedline's `Menu` trait. Very
//! narrow terminals fall back to reedline's `ColumnarMenu`.

use nu_ansi_term::Style;
use reedline::{
    menu_functions, ColumnarMenu, Completer, Editor, InputMode, Menu, MenuBuilder, MenuEvent,
    MenuTextStyle, Painter, Suggestion,
};

use crate::width::{display_width, truncate_display};

/// Most item rows shown at once; the window scrolls to follow the selection.
pub(crate) const MAX_ROWS: usize = 8;
/// Below this terminal width the box is not drawn.
pub(crate) const MIN_TERMINAL_WIDTH: usize = 30;
const MIN_BOX_WIDTH: usize = 24;
const MAX_DETAIL_WIDTH: usize = 48;
const MAX_KIND_WIDTH: usize = 10;
const MAX_FOOTER_LINES: usize = 3;

/// One row of the popup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Row {
    pub label: String,
    pub detail: String,
    pub kind: String,
}

/// Styles for the pieces of the box. `None` renders plain text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BoxStyles {
    pub border: Style,
    pub text: Style,
    pub selected: Style,
    pub detail: Style,
    pub footer: Style,
    /// The `n/total` marker in the bottom border: bright, so it is never mistaken for border.
    pub count: Style,
}

/// Whether the terminal is wide enough for the box.
pub(crate) fn use_box(terminal_width: usize) -> bool {
    terminal_width >= MIN_TERMINAL_WIDTH
}

/// The first visible row so that `selected` stays inside a window of
/// `visible` rows over `total` items, moving `first` as little as possible.
pub(crate) fn window_start(selected: usize, first: usize, visible: usize, total: usize) -> usize {
    let visible = visible.max(1);
    if total <= visible {
        return 0;
    }
    let selected = selected.min(total - 1);
    let first = first.min(total - visible);
    if selected < first {
        selected
    } else if selected >= first + visible {
        selected + 1 - visible
    } else {
        first
    }
}

/// First line of `text`, control characters replaced by spaces, trimmed.
fn clean(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn pad_right(text: &str, width: usize) -> String {
    let used = display_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(used)))
}

fn pad_left(text: &str, width: usize) -> String {
    let used = display_width(text);
    format!("{}{text}", " ".repeat(width.saturating_sub(used)))
}

/// Splits `text` into at most `max_lines` lines of at most `width` columns;
/// when text remains after the last line, that line ends in `…`.
fn wrap_display(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = display_width(ch.encode_utf8(&mut [0; 4]));
        if used + w > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(ch);
        used += w;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let mut last = lines.pop().unwrap_or_default();
        while display_width(&last) + 1 > width && !last.is_empty() {
            last.pop();
        }
        last.push('…');
        lines.push(last);
    }
    lines
}

fn paint(style: Option<Style>, text: &str) -> String {
    match style {
        Some(style) if !text.is_empty() => style.paint(text).to_string(),
        _ => text.to_string(),
    }
}

/// Lines of the popup (each starting with a one-column indent), without
/// trailing newlines. `terminal_width` is the full terminal width.
/// `kind_styles[i]` is the color of row `i`'s kind word.
pub(crate) fn render_box(
    rows: &[Row],
    selected: usize,
    first: usize,
    max_rows: usize,
    terminal_width: usize,
    styles: Option<&BoxStyles>,
    kind_styles: &[Option<Style>],
) -> Vec<String> {
    let max_w = terminal_width.saturating_sub(2);
    if max_w < 8 {
        return Vec::new();
    }
    let border = styles.map(|s| s.border);
    let text_style = styles.map(|s| s.text);
    let detail_style = styles.map(|s| s.detail);
    let footer_style = styles.map(|s| s.footer);
    let selected_style = styles.map(|s| s.selected);

    let labels: Vec<String> = rows.iter().map(|r| clean(&r.label)).collect();
    let details: Vec<String> = rows.iter().map(|r| clean(&r.detail)).collect();
    let kinds: Vec<String> = rows.iter().map(|r| clean(&r.kind)).collect();

    let total = rows.len();
    let label_nat = labels.iter().map(|s| display_width(s)).max().unwrap_or(0);
    let detail_nat = details
        .iter()
        .map(|s| display_width(s))
        .max()
        .unwrap_or(0)
        .min(MAX_DETAIL_WIDTH);
    let mut kind_w = kinds
        .iter()
        .map(|s| display_width(s))
        .max()
        .unwrap_or(0)
        .min(MAX_KIND_WIDTH);

    let inner_nat = 3
        + label_nat
        + if detail_nat > 0 { 2 + detail_nat } else { 0 }
        + if kind_w > 0 { 2 + kind_w } else { 0 }
        + 1;
    let box_w = (inner_nat + 2).max(MIN_BOX_WIDTH).min(max_w);
    let inner = box_w - 2;
    let top = format!(" {}", paint(border, &format!("╭{}╮", "─".repeat(inner))));
    let side = |s: &str| paint(border, s);

    if total == 0 {
        let msg = pad_right("no matches", inner.saturating_sub(3));
        let body = format!("   {msg}");
        return vec![
            top,
            format!(" {}{}{}", side("│"), paint(detail_style, &pad_right(&body, inner)), side("│")),
            format!(" {}", paint(border, &format!("╰{}╯", "─".repeat(inner)))),
        ];
    }

    let avail = inner.saturating_sub(4);
    kind_w = kind_w.min(avail / 3);
    let kind_part = if kind_w > 0 { 2 + kind_w } else { 0 };
    let room = avail.saturating_sub(kind_part);
    let reserve_detail = if detail_nat > 0 { 2 + detail_nat.min(12) } else { 0 };
    let label_w = label_nat.min(room.saturating_sub(reserve_detail).max(room.min(10)));
    let after_label = room - label_w;
    let detail_w = if detail_nat > 0 && after_label > 2 {
        detail_nat.min(after_label - 2)
    } else {
        0
    };

    let selected = selected.min(total - 1);
    let visible = max_rows.max(1).min(total);
    let first = first.min(total - visible);

    let mut out = vec![top];
    for index in first..first + visible {
        let is_selected = index == selected;
        let marker = if is_selected { "▶" } else { " " };
        let prefix = format!(" {marker} ");
        let label = pad_right(&truncate_display(&labels[index], label_w), label_w);
        let detail_part = if detail_w > 0 {
            format!("  {}", pad_right(&truncate_display(&details[index], detail_w), detail_w))
        } else {
            String::new()
        };
        let kind = pad_left(&truncate_display(&kinds[index], kind_w), kind_w);
        let used = display_width(&prefix)
            + display_width(&label)
            + display_width(&detail_part)
            + display_width(&kind)
            + 1;
        let fill = " ".repeat(inner.saturating_sub(used));
        let body = if is_selected {
            let all = format!("{prefix}{label}{detail_part}{fill}{kind} ");
            paint(selected_style, &all)
        } else {
            let kind_style = kind_styles.get(index).copied().flatten().or(text_style);
            format!(
                "{}{}{}{}{}",
                paint(text_style, &format!("{prefix}{label}")),
                paint(detail_style, &detail_part),
                paint(text_style, &fill),
                paint(kind_style, &kind),
                paint(text_style, " "),
            )
        };
        out.push(format!(" {}{}{}", side("│"), body, side("│")));
    }

    let detail = &details[selected];
    if !detail.is_empty() {
        out.push(format!(" {}", paint(border, &format!("├{}┤", "─".repeat(inner)))));
        for line in wrap_display(detail, inner.saturating_sub(2), MAX_FOOTER_LINES) {
            let padded = pad_right(&line, inner.saturating_sub(2));
            out.push(format!(
                " {} {} {}",
                side("│"),
                paint(footer_style, &padded),
                side("│")
            ));
        }
    }

    let indicator = format!(" {}/{} ", selected + 1, total);
    let count_style = styles.map(|s| s.count);
    let bottom = if display_width(&indicator) + 2 <= inner {
        format!(
            "{}{}{}",
            paint(border, &format!("╰{}", "─".repeat(inner - display_width(&indicator)))),
            paint(count_style, &indicator),
            paint(border, "╯")
        )
    } else {
        paint(border, &format!("╰{}╯", "─".repeat(inner)))
    };
    out.push(format!(" {bottom}"));
    out
}

/// Reedline menu that draws the completion popup. It owns a `ColumnarMenu`
/// that holds the values, answers partial completion, and is what gets drawn
/// when the terminal is too narrow for the box.
pub(crate) struct BorderedMenu {
    inner: ColumnarMenu,
    indicator: String,
    styles: MenuTextStyle,
    selected: usize,
    first: usize,
    width: usize,
    height: usize,
    event: Option<MenuEvent>,
}

impl BorderedMenu {
    /// `indicator` is what the prompt shows in place of its own marker while
    /// the menu is open; pass the prompt's own marker so the prompt does not
    /// change.
    pub(crate) fn new(name: &str, indicator: &str, styles: MenuTextStyle) -> Self {
        let inner = ColumnarMenu::default()
            .with_name(name)
            // The completer needs the text after the cursor too
            // (`import { | } from "x"`).
            .with_input_mode(InputMode::FullBuffer)
            .with_text_style(styles.text_style)
            .with_selected_text_style(styles.selected_text_style)
            .with_description_text_style(styles.description_style)
            .with_match_text_style(styles.match_style)
            .with_selected_match_text_style(styles.selected_match_style);
        Self {
            inner,
            indicator: indicator.to_string(),
            styles,
            selected: 0,
            first: 0,
            width: 0,
            height: 0,
            event: None,
        }
    }

    fn boxed(&self) -> bool {
        use_box(self.width)
    }

    fn total(&self) -> usize {
        self.inner.get_values().len()
    }

    fn visible_rows(&self) -> usize {
        // Leave room for the two prompt lines, the borders and a footer.
        MAX_ROWS.min(self.height.saturating_sub(8)).max(1)
    }

    fn reset(&mut self) {
        self.selected = 0;
        self.first = 0;
    }

    fn step(&mut self, forward: bool) {
        let total = self.total();
        if total == 0 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % total
        } else {
            (self.selected + total - 1) % total
        };
    }

    fn apply(&mut self, event: &MenuEvent) {
        match event {
            MenuEvent::Activate(_) | MenuEvent::Edit(_) | MenuEvent::Deactivate => self.reset(),
            MenuEvent::NextElement | MenuEvent::MoveDown | MenuEvent::MoveRight => self.step(true),
            MenuEvent::PreviousElement | MenuEvent::MoveUp | MenuEvent::MoveLeft => {
                self.step(false)
            }
            MenuEvent::NextPage | MenuEvent::PreviousPage => {}
        }
        let total = self.total();
        self.selected = self.selected.min(total.saturating_sub(1));
        self.first = window_start(self.selected, self.first, self.visible_rows(), total);
    }

    fn rows(&self) -> (Vec<Row>, Vec<Option<Style>>) {
        let values = self.inner.get_values();
        let rows = values
            .iter()
            .map(|s: &Suggestion| Row {
                label: s.value.clone(),
                detail: s.description.clone().unwrap_or_default(),
                kind: s
                    .extra
                    .as_ref()
                    .and_then(|extra| extra.first().cloned())
                    .unwrap_or_default(),
            })
            .collect();
        let kind_styles = values.iter().map(|s| s.style).collect();
        (rows, kind_styles)
    }

    fn lines(&self, ansi: bool) -> Vec<String> {
        let (rows, kind_styles) = self.rows();
        let box_styles = BoxStyles {
            border: Style::new().fg(nu_ansi_term::Color::DarkGray),
            text: self.styles.text_style,
            selected: self.styles.selected_text_style,
            detail: Style::new().dimmed(),
            footer: self.styles.description_style,
            count: Style::new().bold().fg(nu_ansi_term::Color::LightCyan),
        };
        render_box(
            &rows,
            self.selected,
            self.first,
            self.visible_rows(),
            self.width,
            ansi.then_some(&box_styles),
            &kind_styles,
        )
    }
}

impl Menu for BorderedMenu {
    // `Menu::settings` returns a type reedline does not export, so it cannot
    // be implemented here; everything that would read it is overridden.
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn indicator(&self) -> &str {
        &self.indicator
    }

    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    fn menu_event(&mut self, event: MenuEvent) {
        self.inner.menu_event(event.clone());
        self.event = Some(event);
    }

    fn can_quick_complete(&self) -> bool {
        true
    }

    fn can_partially_complete(
        &mut self,
        values_updated: bool,
        editor: &mut Editor,
        completer: &mut dyn Completer,
    ) -> bool {
        let done = self
            .inner
            .can_partially_complete(values_updated, editor, completer);
        if done {
            self.reset();
        }
        done
    }

    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        self.inner.update_values(editor, completer);
        self.reset();
    }

    fn update_working_details(
        &mut self,
        editor: &mut Editor,
        completer: &mut dyn Completer,
        painter: &Painter,
    ) {
        self.width = usize::from(painter.screen_width());
        self.height = usize::from(painter.screen_height());
        self.inner.update_working_details(editor, completer, painter);
        if let Some(event) = self.event.take() {
            self.apply(&event);
        }
    }

    fn replace_in_buffer(&self, editor: &mut Editor) {
        if self.boxed() {
            let value = self.inner.get_values().get(self.selected).cloned();
            menu_functions::replace_in_buffer(value, editor, None);
        } else {
            self.inner.replace_in_buffer(editor);
        }
    }

    fn menu_required_lines(&self, terminal_columns: u16) -> u16 {
        if self.boxed() {
            self.lines(false).len() as u16
        } else {
            self.inner.menu_required_lines(terminal_columns)
        }
    }

    fn menu_string(&self, available_lines: u16, use_ansi_coloring: bool) -> String {
        if self.boxed() {
            self.lines(use_ansi_coloring).join("\r\n")
        } else {
            self.inner.menu_string(available_lines, use_ansi_coloring)
        }
    }

    fn min_rows(&self) -> u16 {
        if self.boxed() {
            (self.lines(false).len() as u16).min(6)
        } else {
            self.inner.min_rows()
        }
    }

    fn get_values(&self) -> &[Suggestion] {
        self.inner.get_values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nu_ansi_term::Color;

    fn row(label: &str, detail: &str, kind: &str) -> Row {
        Row {
            label: label.into(),
            detail: detail.into(),
            kind: kind.into(),
        }
    }

    fn sample() -> Vec<Row> {
        vec![
            row("name", "str", "field"),
            row("nameOf", "fn(id: int) -> str", "fn"),
            row("nameless", "bool", "field"),
        ]
    }

    fn plain(rows: &[Row], selected: usize, first: usize, max_rows: usize, width: usize) -> Vec<String> {
        render_box(rows, selected, first, max_rows, width, None, &[])
    }

    #[test]
    fn small_case_renders_exact_plain_box() {
        let lines = plain(&sample(), 1, 0, MAX_ROWS, 80);
        let expected = vec![
            " ╭───────────────────────────────────────╮",
            " │   name      str                 field │",
            " │ ▶ nameOf    fn(id: int) -> str     fn │",
            " │   nameless  bool                field │",
            " ├───────────────────────────────────────┤",
            " │ fn(id: int) -> str                    │",
            " ╰────────────────────────────────── 2/3 ╯",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn every_line_has_the_same_display_width() {
        for w in [30usize, 31, 40, 60, 120] {
            let lines = plain(&sample(), 0, 0, MAX_ROWS, w);
            let widths: Vec<usize> = lines.iter().map(|l| display_width(l)).collect();
            assert!(widths.iter().all(|x| *x == widths[0]), "{w}: {lines:#?}");
            assert!(widths[0] <= w - 1, "{w}: {widths:?}");
        }
    }

    #[test]
    fn border_characters_are_rounded() {
        let lines = plain(&sample(), 0, 0, MAX_ROWS, 80);
        assert!(lines[0].starts_with(" ╭") && lines[0].ends_with('╮'));
        assert!(lines.last().unwrap().starts_with(" ╰") && lines.last().unwrap().ends_with('╯'));
        assert!(lines.iter().any(|l| l.starts_with(" ├") && l.ends_with('┤')));
    }

    #[test]
    fn long_detail_is_truncated_with_an_ellipsis_but_footer_keeps_it() {
        let rows = vec![row("f", &"x".repeat(80), "fn")];
        let lines = plain(&rows, 0, 0, MAX_ROWS, 60);
        assert!(lines[1].contains('…'), "{lines:#?}");
        assert!(lines.iter().all(|l| display_width(l) <= 59));
        assert!(lines.len() >= 5, "footer present: {lines:#?}");
    }

    #[test]
    fn label_longer_than_the_box_is_truncated() {
        let rows = vec![row(&"n".repeat(100), "", "var")];
        let lines = plain(&rows, 0, 0, MAX_ROWS, 40);
        assert!(lines[1].contains('…'));
        assert!(lines.iter().all(|l| display_width(l) <= 39));
    }

    #[test]
    fn no_detail_hides_separator_and_footer() {
        let rows = vec![row("a", "", "var"), row("b", "", "var")];
        let lines = plain(&rows, 0, 0, MAX_ROWS, 80);
        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains('├')));
        assert!(lines[3].contains("1/2"));
    }

    #[test]
    fn selection_marker_follows_selection() {
        let a = plain(&sample(), 0, 0, MAX_ROWS, 80);
        let b = plain(&sample(), 2, 0, MAX_ROWS, 80);
        assert!(a[1].contains('▶') && !a[2].contains('▶'));
        assert!(b[3].contains('▶') && !b[1].contains('▶'));
        assert!(b.last().unwrap().contains("3/3"));
    }

    #[test]
    fn at_most_max_rows_rows_and_window_follows_selection() {
        let rows: Vec<Row> = (0..20).map(|i| row(&format!("item{i:02}"), "", "var")).collect();
        let first = window_start(10, 0, MAX_ROWS, rows.len());
        assert_eq!(first, 3);
        let lines = plain(&rows, 10, first, MAX_ROWS, 80);
        assert_eq!(lines.len(), MAX_ROWS + 2);
        assert!(lines[1].contains("item03"));
        assert!(lines[MAX_ROWS].contains("item10") && lines[MAX_ROWS].contains('▶'));
        assert!(lines.last().unwrap().contains("11/20"));
    }

    #[test]
    fn window_start_moves_minimally_and_clamps() {
        assert_eq!(window_start(0, 0, 8, 20), 0);
        assert_eq!(window_start(7, 0, 8, 20), 0);
        assert_eq!(window_start(8, 0, 8, 20), 1);
        assert_eq!(window_start(2, 5, 8, 20), 2);
        assert_eq!(window_start(19, 0, 8, 20), 12);
        assert_eq!(window_start(0, 12, 8, 20), 0);
        assert_eq!(window_start(3, 9, 8, 5), 0);
        assert_eq!(window_start(0, 0, 0, 0), 0);
        assert_eq!(window_start(5, 0, 0, 10), 5);
    }

    #[test]
    fn zero_items_do_not_panic_and_say_so() {
        let lines = plain(&[], 0, 0, MAX_ROWS, 80);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains("no matches"));
    }

    #[test]
    fn one_item_renders() {
        let lines = plain(&[row("only", "d", "kw")], 0, 0, MAX_ROWS, 80);
        assert!(lines[1].contains("only") && lines[1].contains('▶'));
        assert!(lines.last().unwrap().contains("1/1"));
    }

    #[test]
    fn narrow_width_decision() {
        assert!(!use_box(0));
        assert!(!use_box(29));
        assert!(use_box(30));
        assert!(use_box(200));
    }

    #[test]
    fn narrow_but_boxed_terminals_and_degenerate_input_do_not_panic() {
        for w in 0..40usize {
            let _ = plain(&sample(), 1, 0, MAX_ROWS, w);
            let _ = plain(&[], 0, 0, MAX_ROWS, w);
            let _ = plain(&sample(), 99, 99, 0, w);
        }
    }

    #[test]
    fn wide_unicode_keeps_columns_aligned() {
        let rows = vec![row("日本語", "説明", "var"), row("abc", "", "fn")];
        let lines = plain(&rows, 0, 0, MAX_ROWS, 80);
        let widths: Vec<usize> = lines.iter().map(|l| display_width(l)).collect();
        assert!(widths.iter().all(|x| *x == widths[0]), "{lines:#?}");
    }

    #[test]
    fn newlines_and_controls_in_text_never_break_lines() {
        let rows = vec![row("a\nb", "first\nsecond", "fn\t")];
        let lines = plain(&rows, 0, 0, MAX_ROWS, 80);
        assert!(lines.iter().all(|l| !l.contains('\n') && !l.contains('\t')));
        assert!(lines.iter().any(|l| l.contains("first")));
    }

    #[test]
    fn long_footer_wraps_to_a_few_lines_then_truncates() {
        let rows = vec![row("f", &"word ".repeat(60), "fn")];
        let lines = plain(&rows, 0, 0, MAX_ROWS, 40);
        // top, row, separator, up to 3 footer lines, bottom
        assert!(lines.len() <= 1 + 1 + 1 + MAX_FOOTER_LINES + 1, "{lines:#?}");
        assert!(lines[lines.len() - 2].contains('…'));
    }

    #[test]
    fn styled_output_wraps_segments_and_strips_to_the_plain_text() {
        let styles = BoxStyles {
            border: Style::new().fg(Color::DarkGray),
            text: Style::new().fg(Color::White),
            selected: Style::new().fg(Color::Black).on(Color::LightCyan).bold(),
            detail: Style::new().dimmed(),
            footer: Style::new(),
            count: Style::new().bold().fg(Color::LightCyan),
        };
        let kinds = vec![Some(Style::new().fg(Color::Green)); 3];
        let styled = render_box(&sample(), 1, 0, MAX_ROWS, 80, Some(&styles), &kinds);
        let plain_lines = plain(&sample(), 1, 0, MAX_ROWS, 80);
        assert!(styled[2].contains("\x1b["));
        let strip = |s: &str| {
            let mut out = String::new();
            let mut esc = false;
            for c in s.chars() {
                if esc {
                    if c.is_ascii_alphabetic() {
                        esc = false;
                    }
                } else if c == '\x1b' {
                    esc = true;
                } else {
                    out.push(c);
                }
            }
            out
        };
        let stripped: Vec<String> = styled.iter().map(|l| strip(l)).collect();
        assert_eq!(stripped, plain_lines);
    }
}
