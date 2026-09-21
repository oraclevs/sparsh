//! Full-screen pager for values too big for the inline preview.
//!
//! `view` (or the `pager` key binding) renders the last structured result
//! without row, line or width limits and scrolls through it. Table headers stay
//! pinned, wide tables scroll sideways, and `/` searches. Keys come from
//! `pagerKeybindings` in the config on top of the built-in set.

use std::io::{self, IsTerminal, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use sparsh_core::{KeyChord, KeybindingKey, PagerAction, PagerKeybindingConfig};
use unicode_width::UnicodeWidthChar;

/// Horizontal scroll step in columns.
const SIDEWAYS_STEP: usize = 8;

/// Pages `lines` (already painted with ANSI colors). Without a terminal the
/// lines are written straight to stdout.
pub fn run(lines: &[String], keys: &[PagerKeybindingConfig]) -> io::Result<()> {
    let mut stdout = io::stdout();
    if !stdout.is_terminal() || !io::stdin().is_terminal() {
        for line in lines {
            writeln!(stdout, "{line}")?;
        }
        return Ok(());
    }
    let _guard = TerminalGuard::enter(&mut stdout)?;
    let (width, height) = terminal::size()?;
    let mut state = PagerState::new(lines, usize::from(width), usize::from(height));
    loop {
        draw(&mut stdout, &state)?;
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if state.search_input.is_some() {
                    state.type_search(key);
                } else if let Some(action) = lookup(keys, key) {
                    if !state.apply(action) {
                        return Ok(());
                    }
                }
            }
            Event::Resize(width, height) => state.resize(usize::from(width), usize::from(height)),
            _ => {}
        }
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter(stdout: &mut io::Stdout) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn lookup(keys: &[PagerKeybindingConfig], event: KeyEvent) -> Option<PagerAction> {
    keys.iter()
        .find(|binding| chord_matches(&binding.chord, event))
        .map(|binding| binding.action)
}

fn chord_matches(chord: &KeyChord, event: KeyEvent) -> bool {
    if chord.control != event.modifiers.contains(KeyModifiers::CONTROL)
        || chord.alt != event.modifiers.contains(KeyModifiers::ALT)
    {
        return false;
    }
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    match (chord.key, event.code) {
        // The character itself already says whether shift was held.
        (KeybindingKey::Char(wanted), KeyCode::Char(got)) => wanted == got,
        (KeybindingKey::BackTab, KeyCode::BackTab) => true,
        (key, code) => chord.shift == shift && key_code(key) == Some(code),
    }
}

fn key_code(key: KeybindingKey) -> Option<KeyCode> {
    Some(match key {
        KeybindingKey::Char(character) => KeyCode::Char(character),
        KeybindingKey::Tab => KeyCode::Tab,
        KeybindingKey::BackTab => KeyCode::BackTab,
        KeybindingKey::Enter => KeyCode::Enter,
        KeybindingKey::Esc => KeyCode::Esc,
        KeybindingKey::Backspace => KeyCode::Backspace,
        KeybindingKey::Delete => KeyCode::Delete,
        KeybindingKey::Insert => KeyCode::Insert,
        KeybindingKey::Left => KeyCode::Left,
        KeybindingKey::Right => KeyCode::Right,
        KeybindingKey::Up => KeyCode::Up,
        KeybindingKey::Down => KeyCode::Down,
        KeybindingKey::Home => KeyCode::Home,
        KeybindingKey::End => KeyCode::End,
        KeybindingKey::PageUp => KeyCode::PageUp,
        KeybindingKey::PageDown => KeyCode::PageDown,
        KeybindingKey::Function(number) => KeyCode::F(number),
    })
}

/// Scroll position and search state, independent of the terminal.
struct PagerState<'a> {
    lines: &'a [String],
    plain: Vec<String>,
    /// Lines that stay on screen while scrolling (a table's top border,
    /// header and separator).
    pinned: usize,
    top: usize,
    left: usize,
    width: usize,
    height: usize,
    max_width: usize,
    query: String,
    search_input: Option<String>,
    status: Option<String>,
}

impl<'a> PagerState<'a> {
    fn new(lines: &'a [String], width: usize, height: usize) -> Self {
        let plain = lines
            .iter()
            .map(|line| strip_ansi(line))
            .collect::<Vec<_>>();
        let pinned = if plain.first().is_some_and(|line| line.starts_with('╭')) {
            3.min(plain.len())
        } else {
            0
        };
        let max_width = plain
            .iter()
            .map(|line| display_width(line))
            .max()
            .unwrap_or(0);
        let mut state = Self {
            lines,
            plain,
            pinned,
            top: pinned,
            left: 0,
            width,
            height,
            max_width,
            query: String::new(),
            search_input: None,
            status: None,
        };
        state.resize(width, height);
        state
    }

    fn resize(&mut self, width: usize, height: usize) {
        self.width = width.max(1);
        self.height = height.max(2);
        self.clamp();
    }

    /// Body rows between the pinned header and the status line.
    fn body_height(&self) -> usize {
        self.height.saturating_sub(1 + self.pinned).max(1)
    }

    fn max_top(&self) -> usize {
        self.lines
            .len()
            .saturating_sub(self.body_height())
            .max(self.pinned)
    }

    fn clamp(&mut self) {
        self.top = self.top.clamp(self.pinned, self.max_top());
        self.left = self.left.min(self.max_width.saturating_sub(self.width / 2));
    }

    /// Returns `false` when the pager should close.
    fn apply(&mut self, action: PagerAction) -> bool {
        self.status = None;
        let page = self.body_height();
        match action {
            PagerAction::LineDown => self.top += 1,
            PagerAction::LineUp => self.top = self.top.saturating_sub(1),
            PagerAction::PageDown => self.top += page.saturating_sub(1).max(1),
            PagerAction::PageUp => {
                self.top = self.top.saturating_sub(page.saturating_sub(1).max(1))
            }
            PagerAction::HalfPageDown => self.top += (page / 2).max(1),
            PagerAction::HalfPageUp => self.top = self.top.saturating_sub((page / 2).max(1)),
            PagerAction::Top => {
                self.top = 0;
                self.left = 0;
            }
            PagerAction::Bottom => self.top = usize::MAX,
            PagerAction::Left => self.left = self.left.saturating_sub(SIDEWAYS_STEP),
            PagerAction::Right => self.left += SIDEWAYS_STEP,
            PagerAction::Search => self.search_input = Some(String::new()),
            PagerAction::SearchNext => self.find(true),
            PagerAction::SearchPrevious => self.find(false),
            PagerAction::Quit => return false,
        }
        self.clamp();
        true
    }

    /// Keys while the `/` prompt is open.
    fn type_search(&mut self, key: KeyEvent) {
        let Some(input) = self.search_input.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Enter => {
                self.query = self.search_input.take().unwrap_or_default();
                // A new search starts at the top of what is visible.
                self.top = self.top.saturating_sub(1);
                self.find(true);
            }
            KeyCode::Esc => self.search_input = None,
            KeyCode::Backspace => {
                if input.pop().is_none() {
                    self.search_input = None;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_input = None;
            }
            KeyCode::Char(character) => input.push(character),
            _ => {}
        }
    }

    fn find(&mut self, forward: bool) {
        if self.query.is_empty() {
            self.status = Some("no search pattern; press / to search".into());
            return;
        }
        let needle = self.query.to_lowercase();
        let total = self.plain.len();
        let start = if forward { self.top + 1 } else { self.top };
        let hit = (0..total).find_map(|step| {
            let index = if forward {
                (start + step) % total
            } else {
                (start + total - 1 - step % total) % total
            };
            (index >= self.pinned && self.plain[index].to_lowercase().contains(&needle))
                .then_some(index)
        });
        match hit {
            Some(index) => {
                self.top = index.saturating_sub(self.body_height() / 3);
                self.status = Some(format!("/{}  line {}", self.query, index + 1));
            }
            None => self.status = Some(format!("pattern not found: {}", self.query)),
        }
        self.clamp();
    }

    fn visible(&self) -> impl Iterator<Item = &String> {
        let body = self.top..(self.top + self.body_height()).min(self.lines.len());
        self.lines[..self.pinned]
            .iter()
            .chain(self.lines[body].iter())
    }

    fn status_line(&self) -> String {
        if let Some(input) = &self.search_input {
            return format!("/{input}");
        }
        if let Some(status) = &self.status {
            return status.clone();
        }
        let last = (self.top + self.body_height()).min(self.lines.len());
        let mut line = format!(
            " lines {}-{} of {}",
            self.top + 1 - usize::from(self.top == 0),
            last,
            self.lines.len()
        );
        if self.left > 0 {
            line.push_str(&format!("  ·  col {}", self.left + 1));
        }
        line.push_str("  ·  j/k scroll  space/b page  h/l sideways  / search  q quit");
        line
    }
}

fn draw(out: &mut io::Stdout, state: &PagerState) -> io::Result<()> {
    queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
    for (row, line) in state.visible().enumerate() {
        queue!(
            out,
            MoveTo(0, row as u16),
            Print(slice_columns(line, state.left, state.width))
        )?;
    }
    let status = truncate(&state.status_line(), state.width);
    queue!(
        out,
        MoveTo(0, (state.height - 1) as u16),
        SetAttribute(Attribute::Reverse),
        Print(format!("{status:<width$}", width = state.width)),
        SetAttribute(Attribute::Reset)
    )?;
    out.flush()
}

fn truncate(text: &str, width: usize) -> String {
    slice_columns(text, 0, width)
}

fn display_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            skip_escape(&mut chars);
        } else {
            out.push(character);
        }
    }
    out
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    if chars.peek() == Some(&'[') {
        chars.next();
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }
}

/// The part of `text` between display columns `start` and `start + width`,
/// keeping every color sequence so styling survives scrolling.
fn slice_columns(text: &str, start: usize, width: usize) -> String {
    let mut out = String::new();
    let mut column = 0usize;
    let mut styled = false;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            let mut sequence = String::from('\u{1b}');
            if chars.peek() == Some(&'[') {
                sequence.push(chars.next().unwrap_or('['));
                for next in chars.by_ref() {
                    sequence.push(next);
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            styled = !sequence.ends_with("[0m");
            out.push_str(&sequence);
            continue;
        }
        let cells = character.width().unwrap_or(0);
        if column >= start && column + cells <= start + width {
            out.push(character);
        }
        column += cells;
        if column >= start + width {
            break;
        }
    }
    if styled {
        out.push_str("\u{1b}[0m");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(count: usize) -> Vec<String> {
        (0..count).map(|n| format!("row {n}")).collect()
    }

    #[test]
    fn scrolling_stops_at_the_last_full_page() {
        let lines = numbered(100);
        let mut state = PagerState::new(&lines, 40, 11);
        assert_eq!(state.body_height(), 10);
        state.apply(PagerAction::PageDown);
        assert_eq!(state.top, 9);
        state.apply(PagerAction::Bottom);
        assert_eq!(state.top, 90);
        state.apply(PagerAction::LineDown);
        assert_eq!(state.top, 90);
        state.apply(PagerAction::Top);
        assert_eq!(state.top, 0);
        state.apply(PagerAction::LineUp);
        assert_eq!(state.top, 0);
    }

    #[test]
    fn table_header_stays_pinned_while_scrolling() {
        let mut lines = vec!["╭──╮".to_string(), "│ a│".into(), "├──┤".into()];
        lines.extend(numbered(50));
        let mut state = PagerState::new(&lines, 40, 10);
        assert_eq!(state.pinned, 3);
        state.apply(PagerAction::HalfPageDown);
        let shown = state.visible().cloned().collect::<Vec<_>>();
        assert_eq!(&shown[..3], &lines[..3]);
        assert_eq!(shown.len(), 9);
        assert!(shown[3].starts_with("row"), "{shown:?}");
        assert!(state.top > 3);
    }

    #[test]
    fn short_content_never_scrolls() {
        let lines = numbered(3);
        let mut state = PagerState::new(&lines, 40, 20);
        state.apply(PagerAction::PageDown);
        state.apply(PagerAction::Bottom);
        assert_eq!(state.top, 0);
    }

    #[test]
    fn search_finds_matches_forward_and_backward_and_reports_misses() {
        let lines = numbered(200);
        let mut state = PagerState::new(&lines, 40, 12);
        state.apply(PagerAction::Search);
        for character in "ROW 150".chars() {
            state.type_search(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        state.type_search(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.search_input.is_none());
        assert!(
            state.visible().any(|line| line == "row 150"),
            "top {}",
            state.top
        );
        assert!(state.status_line().contains("line 151"));

        state.apply(PagerAction::SearchPrevious);
        assert!(
            state.status_line().contains("line 151"),
            "only one match wraps to itself"
        );

        state.query = "nothing like this".into();
        state.apply(PagerAction::SearchNext);
        assert!(state.status_line().contains("not found"));
    }

    #[test]
    fn escape_cancels_the_search_prompt() {
        let lines = numbered(5);
        let mut state = PagerState::new(&lines, 40, 12);
        state.apply(PagerAction::Search);
        state.type_search(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        state.type_search(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.search_input.is_none());
        assert!(state.query.is_empty());
    }

    #[test]
    fn quit_closes_the_pager() {
        let lines = numbered(5);
        let mut state = PagerState::new(&lines, 40, 12);
        assert!(!state.apply(PagerAction::Quit));
    }

    #[test]
    fn sideways_scroll_is_limited_by_content_width() {
        let lines = vec!["x".repeat(100)];
        let mut state = PagerState::new(&lines, 40, 10);
        for _ in 0..50 {
            state.apply(PagerAction::Right);
        }
        assert_eq!(state.left, 100 - 20);
        state.apply(PagerAction::Top);
        assert_eq!(state.left, 0);
    }

    #[test]
    fn slicing_keeps_colors_and_counts_display_columns() {
        let painted = "\u{1b}[1;32mabcdef\u{1b}[0m ghi";
        assert_eq!(slice_columns(painted, 0, 3), "\u{1b}[1;32mabc\u{1b}[0m");
        assert_eq!(strip_ansi(&slice_columns(painted, 2, 6)), "cdef g");
        assert_eq!(slice_columns("日本語", 0, 4), "日本");
        assert_eq!(display_width("日本語"), 6);
    }

    #[test]
    fn chords_match_keys_the_way_terminals_report_them() {
        let capital = KeyChord::parse("G").unwrap();
        assert!(chord_matches(
            &capital,
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT)
        ));
        assert!(!chord_matches(
            &capital,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)
        ));
        let control = KeyChord::parse("ctrl+d").unwrap();
        assert!(chord_matches(
            &control,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)
        ));
        assert!(!chord_matches(
            &control,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)
        ));
        let page = KeyChord::parse("pagedown").unwrap();
        assert!(chord_matches(
            &page,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)
        ));
    }

    #[test]
    fn default_keys_reach_every_action() {
        let keys = sparsh_core::default_pager_keybindings();
        for name in sparsh_core::PAGER_ACTION_NAMES {
            let action = PagerAction::parse(name).unwrap();
            assert!(
                keys.iter().any(|binding| binding.action == action),
                "no default key for {name}"
            );
        }
        let event = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(lookup(&keys, event), Some(PagerAction::PageDown));
    }
}
