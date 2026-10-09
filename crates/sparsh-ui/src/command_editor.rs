use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::{Print, ResetColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use sparsh_core::{ShellSession, ShellUiSnapshot};
use unicode_width::UnicodeWidthChar;

use crate::highlight::paint_range;
use crate::{SemanticRole, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorAction {
    Save,
    Execute,
    // Only produced by the paste-review flow's tests today.
    #[allow(dead_code)]
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorResult {
    pub(crate) text: String,
    pub(crate) action: EditorAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TextBuffer {
    lines: Vec<Vec<char>>,
    row: usize,
    column: usize,
    /// Source-file editing: Enter keeps the indent, `{` adds a level, `}` removes one.
    auto_indent: bool,
    /// Where a selection started; the cursor is its other end.
    anchor: Option<(usize, usize)>,
}

impl From<&str> for TextBuffer {
    fn from(source: &str) -> Self {
        let mut lines = source
            .split('\n')
            .map(|line| line.chars().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        Self {
            lines,
            row: 0,
            column: 0,
            auto_indent: false,
            anchor: None,
        }
    }
}

impl fmt::Display for TextBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            for character in line {
                write!(formatter, "{character}")?;
            }
        }
        Ok(())
    }
}

impl TextBuffer {
    /// The selection as ordered `(row, column)` ends, if any text is selected.
    fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        let cursor = (self.row, self.column);
        if anchor == cursor {
            return None;
        }
        Some(if anchor < cursor { (anchor, cursor) } else { (cursor, anchor) })
    }

    /// The selected columns of `row`, as a char range.
    fn selected_columns(&self, row: usize) -> Option<std::ops::Range<usize>> {
        let ((start_row, start_column), (end_row, end_column)) = self.selection()?;
        if row < start_row || row > end_row {
            return None;
        }
        let from = if row == start_row { start_column } else { 0 };
        let to = if row == end_row { end_column } else { self.lines[row].len() };
        (from < to || (row != end_row && from <= to)).then_some(from..to.max(from))
    }

    fn selected_text(&self) -> String {
        let Some(((start_row, start_column), (end_row, end_column))) = self.selection() else {
            return String::new();
        };
        let mut text = String::new();
        for row in start_row..=end_row {
            let line = &self.lines[row];
            let from = if row == start_row { start_column } else { 0 };
            let to = if row == end_row { end_column } else { line.len() };
            text.extend(&line[from.min(line.len())..to.min(line.len())]);
            if row != end_row {
                text.push('\n');
            }
        }
        text
    }

    /// Removes the selection and puts the cursor where it was; false if none.
    fn delete_selection(&mut self) -> bool {
        let Some(((start_row, start_column), (end_row, end_column))) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let tail = self.lines[end_row][end_column.min(self.lines[end_row].len())..].to_vec();
        self.lines[start_row].truncate(start_column);
        self.lines[start_row].extend(tail);
        self.lines.drain(start_row + 1..=end_row);
        self.row = start_row;
        self.column = start_column;
        self.anchor = None;
        true
    }

    fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        self.row = self.lines.len() - 1;
        self.column = self.lines[self.row].len();
    }

    /// Inserts pasted text exactly as given: no auto-indent, no brace handling.
    fn insert_text(&mut self, text: &str) {
        self.delete_selection();
        for character in text.chars() {
            match character {
                '\r' => {}
                '\n' => {
                    let column = self.column;
                    let tail = self.current_line_mut().split_off(column);
                    self.row += 1;
                    self.lines.insert(self.row, tail);
                    self.column = 0;
                }
                '\t' => {
                    for _ in 0..4 {
                        let column = self.column;
                        self.current_line_mut().insert(column, ' ');
                        self.column += 1;
                    }
                }
                other => {
                    let column = self.column;
                    self.current_line_mut().insert(column, other);
                    self.column += 1;
                }
            }
        }
    }

    fn current_line(&self) -> &[char] {
        &self.lines[self.row]
    }

    fn current_line_mut(&mut self) -> &mut Vec<char> {
        &mut self.lines[self.row]
    }

    fn insert_char(&mut self, character: char) {
        let column = self.column;
        self.current_line_mut().insert(column, character);
        self.column += 1;
        // A closing brace that starts a line steps back one indent level.
        if self.auto_indent && character == '}' {
            let line = self.current_line_mut();
            let before = &line[..line.len().min(column)];
            if before.iter().all(|c| *c == ' ') && column >= 4 {
                line.drain(..4);
                self.column -= 4;
            }
        }
    }

    fn insert_indent(&mut self) {
        for _ in 0..4 {
            self.insert_char(' ');
        }
    }

    /// True when Tab should indent rather than complete: at the start of a
    /// line or after whitespace.
    fn at_indent_position(&self) -> bool {
        self.column == 0 || self.current_line()[self.column - 1].is_whitespace()
    }

    fn insert_newline(&mut self) {
        let column = self.column;
        let line = self.current_line();
        let mut indent: String = line.iter().take_while(|c| **c == ' ').collect();
        if self.auto_indent && line[..column.min(line.len())].last() == Some(&'{') {
            indent.push_str("    ");
        }
        let tail = self.current_line_mut().split_off(column);
        self.row += 1;
        self.lines.insert(self.row, tail);
        self.column = 0;
        if self.auto_indent {
            for character in indent.chars() {
                self.insert_char(character);
            }
        }
    }

    fn backspace(&mut self) {
        if self.column > 0 {
            self.column -= 1;
            let column = self.column;
            self.current_line_mut().remove(column);
        } else if self.row > 0 {
            let current = self.lines.remove(self.row);
            self.row -= 1;
            self.column = self.lines[self.row].len();
            self.lines[self.row].extend(current);
        }
    }

    fn delete(&mut self) {
        if self.column < self.current_line().len() {
            let column = self.column;
            self.current_line_mut().remove(column);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.current_line_mut().extend(next);
        }
    }

    fn move_left(&mut self) {
        if self.column > 0 {
            self.column -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.column = self.current_line().len();
        }
    }

    fn move_right(&mut self) {
        if self.column < self.current_line().len() {
            self.column += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.column = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.column = self.column.min(self.current_line().len());
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.column = self.column.min(self.current_line().len());
        }
    }

    fn complete(&mut self, session: &ShellSession) -> Vec<String> {
        let line = self.current_line().iter().collect::<String>();
        let cursor = char_index_to_byte(&line, self.column);
        let snapshot = session.completion_snapshot();
        let items = sparsh_core::complete(
            &snapshot,
            sparsh_core::CompletionRequest { line: &line, cursor },
        );
        if items.len() == 1 {
            let item = &items[0];
            if item.span.end <= line.len()
                && line.is_char_boundary(item.span.start)
                && line.is_char_boundary(item.span.end)
            {
                let start = line[..item.span.start].chars().count();
                let end = line[..item.span.end].chars().count();
                self.current_line_mut()
                    .splice(start..end, item.replacement.chars());
                self.column = start + item.replacement.chars().count();
            }
            Vec::new()
        } else {
            items
                .iter()
                .take(8)
                .map(|item| item.replacement.clone())
                .collect()
        }
    }

    fn move_home(&mut self) {
        self.column = 0;
    }

    fn move_end(&mut self) {
        self.column = self.current_line().len();
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter(output: &mut impl Write) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(output, EnterAlternateScreen, Hide, EnableBracketedPaste)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut output = io::stdout();
        let _ = execute!(output, DisableBracketedPaste, Show, LeaveAlternateScreen, ResetColor);
    }
}

pub(crate) fn finalize_editor_result(
    _original: &str,
    edited: &str,
    action: EditorAction,
) -> Option<EditorResult> {
    match action {
        EditorAction::Cancel => None,
        EditorAction::Save | EditorAction::Execute => Some(EditorResult {
            text: edited.to_string(),
            action,
        }),
    }
}

pub(crate) fn edit_text(
    initial: &str,
    allow_execute: bool,
    snapshot: &ShellUiSnapshot,
    session: &ShellSession,
) -> io::Result<Option<EditorResult>> {
    edit_buffer(initial, allow_execute, false, snapshot, session)
}

/// The editor for a Spar source file (`repl`): the whole buffer is lexed for
/// highlighting, Tab indents or completes, Enter keeps the indentation, and
/// the compiler's first error is shown live.
pub(crate) fn edit_spar_source(
    initial: &str,
    snapshot: &ShellUiSnapshot,
    session: &ShellSession,
) -> io::Result<Option<EditorResult>> {
    edit_buffer(initial, false, true, snapshot, session)
}

fn edit_buffer(
    initial: &str,
    allow_execute: bool,
    spar_file: bool,
    snapshot: &ShellUiSnapshot,
    session: &ShellSession,
) -> io::Result<Option<EditorResult>> {
    let mut buffer = TextBuffer::from(initial);
    buffer.auto_indent = spar_file;
    let mut output = io::stdout();
    let _guard = TerminalGuard::enter(&mut output)?;
    let mut viewport_row = 0usize;
    let mut clipboard = String::new();
    let mut completion_hints = Vec::<String>::new();
    let colors_enabled = std::env::var_os("NO_COLOR").is_none();
    let theme = if colors_enabled {
        Theme::colored()
    } else {
        Theme::plain()
    };

    loop {
        render(
            &mut output,
            &buffer,
            allow_execute,
            spar_file,
            snapshot,
            session,
            &completion_hints,
            &theme,
            &mut viewport_row,
        )?;
        match event::read()? {
            Event::Key(key) => {
                if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    continue;
                }
                if key.code != KeyCode::Tab {
                    completion_hints.clear();
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('s') => {
                            return Ok(finalize_editor_result(
                                initial,
                                &buffer.to_string(),
                                EditorAction::Save,
                            ));
                        }
                        // Copy when something is selected; otherwise leave the editor.
                        KeyCode::Char('c') if buffer.selection().is_some() => {
                            clipboard = buffer.selected_text();
                            copy_to_terminal_clipboard(&mut output, &clipboard)?;
                        }
                        KeyCode::Char('c') => return Ok(None),
                        KeyCode::Char('x') if buffer.selection().is_some() => {
                            clipboard = buffer.selected_text();
                            copy_to_terminal_clipboard(&mut output, &clipboard)?;
                            buffer.delete_selection();
                        }
                        KeyCode::Char('v') => buffer.insert_text(&clipboard),
                        KeyCode::Char('a') => {
                            buffer.anchor = None;
                            buffer.move_home();
                        }
                        KeyCode::Char('e') => {
                            buffer.anchor = None;
                            buffer.move_end();
                        }
                        _ => {}
                    }
                    continue;
                }
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let movement = matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End
                );
                if movement {
                    // Shift extends a selection from where it started; a plain move clears it.
                    if shift {
                        buffer.anchor.get_or_insert((buffer.row, buffer.column));
                    } else {
                        buffer.anchor = None;
                    }
                }

                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('e' | 'E') if allow_execute && key.modifiers.contains(KeyModifiers::ALT) => {
                        return Ok(finalize_editor_result(
                            initial,
                            &buffer.to_string(),
                            EditorAction::Execute,
                        ));
                    }
                    KeyCode::Char('a' | 'A') if key.modifiers.contains(KeyModifiers::ALT) => buffer.select_all(),
                    KeyCode::Char(character) => {
                        buffer.delete_selection();
                        buffer.insert_char(character);
                    }
                    KeyCode::Enter => {
                        buffer.delete_selection();
                        buffer.insert_newline();
                    }
                    KeyCode::Backspace => {
                        if !buffer.delete_selection() {
                            buffer.backspace();
                        }
                    }
                    KeyCode::Delete => {
                        if !buffer.delete_selection() {
                            buffer.delete();
                        }
                    }
                    KeyCode::Left => buffer.move_left(),
                    KeyCode::Right => buffer.move_right(),
                    KeyCode::Up => buffer.move_up(),
                    KeyCode::Down => buffer.move_down(),
                    KeyCode::Home => buffer.move_home(),
                    KeyCode::End => buffer.move_end(),
                    KeyCode::Tab if spar_file && buffer.at_indent_position() => {
                        buffer.delete_selection();
                        buffer.insert_indent();
                    }
                    KeyCode::Tab => completion_hints = buffer.complete(session),
                    _ => {}
                }
            }
            // Text pasted into the terminal (bracketed paste) arrives as one event.
            Event::Paste(text) => {
                completion_hints.clear();
                buffer.insert_text(&text);
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

/// Asks the terminal to put `text` on the system clipboard (OSC 52).
fn copy_to_terminal_clipboard(output: &mut impl Write, text: &str) -> io::Result<()> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(ALPHABET[(n >> 18) as usize & 63] as char);
        encoded.push(ALPHABET[(n >> 12) as usize & 63] as char);
        encoded.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        encoded.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    write!(output, "\x1b]52;c;{encoded}\x07")?;
    output.flush()
}

pub fn edit_buffer_file(path: &Path) -> io::Result<()> {
    let original = fs::read_to_string(path)?;
    let session = ShellSession::try_new().map_err(|error| io::Error::other(error.to_string()))?;
    let snapshot = session.ui_snapshot();
    if let Some(result) = edit_text(&original, false, &snapshot, &session)? {
        if result.action == EditorAction::Save {
            fs::write(path, result.text)?;
        }
    }
    Ok(())
}

fn render(
    output: &mut impl Write,
    buffer: &TextBuffer,
    allow_execute: bool,
    spar_file: bool,
    snapshot: &ShellUiSnapshot,
    session: &ShellSession,
    completion_hints: &[String],
    theme: &Theme,
    viewport_row: &mut usize,
) -> io::Result<()> {
    let (width, height) = terminal::size().unwrap_or((80, 24));
    let width = usize::from(width.max(20));
    let content_height = usize::from(height.saturating_sub(3).max(1));

    if buffer.row < *viewport_row {
        *viewport_row = buffer.row;
    } else if buffer.row >= *viewport_row + content_height {
        *viewport_row = buffer.row + 1 - content_height;
    }

    queue!(output, Hide, MoveTo(0, 0), Clear(ClearType::All))?;
    queue!(
        output,
        Print(theme.paint(SemanticRole::EditorHeader, "Sparsh Editor")),
        Print("  "),
        Print(theme.paint(
            SemanticRole::EditorPosition,
            &format!("line {}:{}", buffer.row + 1, buffer.column + 1),
        )),
    )?;

    let editor_width = width.saturating_sub(1);
    // Whole-file highlighting: lex once, then slice the spans per row.
    let file_spans = if spar_file && theme.enabled() {
        crate::highlight::scan_source(&buffer.to_string(), snapshot)
    } else {
        None
    };
    let mut row_starts = Vec::with_capacity(buffer.lines.len());
    let mut offset = 0usize;
    for line in &buffer.lines {
        row_starts.push(offset);
        offset += line.iter().map(|c| c.len_utf8()).sum::<usize>() + 1;
    }
    let mut cursor_x = 0usize;
    let mut cursor_y = 1usize;
    for screen_row in 0..content_height {
        let row = *viewport_row + screen_row;
        queue!(
            output,
            MoveTo(0, (screen_row + 1) as u16),
            Clear(ClearType::CurrentLine)
        )?;
        if row >= buffer.lines.len() {
            continue;
        }
        let line_cursor = if row == buffer.row { buffer.column } else { 0 };
        let (visible, x, start_char) = visible_line(&buffer.lines[row], line_cursor, editor_width);
        if row == buffer.row {
            cursor_x = x;
            cursor_y = screen_row + 1;
        }
        if let Some(selected) = buffer.selected_columns(row) {
            // Selected text is drawn in reverse video over the visible slice.
            let chars: Vec<char> = visible.chars().collect();
            let from = selected.start.saturating_sub(start_char).min(chars.len());
            let to = selected.end.saturating_sub(start_char).clamp(from, chars.len());
            // An empty selected part (a selected line break) still shows one cell.
            let to_cells = if from == to && selected.start == selected.end { to } else { to };
            queue!(
                output,
                Print(chars[..from].iter().collect::<String>()),
                Print(theme.paint(
                    SemanticRole::EditorSelection,
                    &chars[from..to_cells].iter().collect::<String>(),
                )),
                Print(chars[to_cells..].iter().collect::<String>()),
            )?;
        } else if theme.enabled() {
            let line = buffer.lines[row].iter().collect::<String>();
            let start_byte = char_index_to_byte(&line, start_char);
            let end_byte = start_byte + visible.len();
            let painted = match &file_spans {
                Some(spans) => {
                    let base = row_starts[row];
                    let local: Vec<crate::highlight::HighlightSpan> = spans
                        .iter()
                        .filter(|span| span.range.end > base && span.range.start < base + line.len())
                        .map(|span| crate::highlight::HighlightSpan {
                            range: span.range.start.saturating_sub(base)
                                ..(span.range.end - base).min(line.len()),
                            role: span.role,
                        })
                        .collect();
                    crate::highlight::paint_spans(&line, start_byte..end_byte, &local, theme)
                }
                None => paint_range(&line, start_byte..end_byte, snapshot, theme),
            };
            queue!(output, Print(painted))?;
        } else {
            queue!(output, Print(visible))?;
        }
    }

    let diagnostics = if spar_file {
        session.repo_diagnostics(&buffer.to_string())
    } else {
        session.editor_diagnostics(&buffer.to_string())
    };
    let diagnostic_row = height.saturating_sub(2);
    queue!(output, MoveTo(0, diagnostic_row), Clear(ClearType::CurrentLine))?;
    if let Some(error) = diagnostics.first() {
        let message = spar::naming::demangle(&format!("{}", error));
        let text = truncate_to_width(&message, width);
        queue!(output, Print(theme.paint(crate::theme::SemanticRole::Error, &text)))?;
    } else if !completion_hints.is_empty() {
        let message = completion_hints.join("  ");
        let text = truncate_to_width(&message, width);
        queue!(output, Print(theme.paint(crate::theme::SemanticRole::Secondary, &text)))?;
    }

    let status_row = height.saturating_sub(1);
    let help = if allow_execute {
        "Ctrl+S save draft  ·  Alt+E execute  ·  Esc cancel"
    } else {
        if spar_file {
            "Ctrl+S apply  ·  Tab indent/complete  ·  Shift+arrows select  ·  Ctrl+C/X/V copy/cut/paste  ·  Esc close"
        } else {
            "Ctrl+S save to prompt  ·  Esc cancel"
        }
    };
    queue!(output, MoveTo(0, status_row), Clear(ClearType::CurrentLine))?;
    queue!(
        output,
        Print(theme.paint(SemanticRole::EditorStatus, &truncate_to_width(help, width))),
    )?;
    queue!(
        output,
        MoveTo(
            cursor_x.min(width.saturating_sub(1)) as u16,
            cursor_y as u16
        ),
        Show,
    )?;
    output.flush()
}

fn visible_line(line: &[char], cursor_column: usize, width: usize) -> (String, usize, usize) {
    if width == 0 {
        return (String::new(), 0, 0);
    }
    let cursor_display = display_width(&line[..cursor_column.min(line.len())]);
    let target_start = cursor_display.saturating_sub(width.saturating_sub(2));

    let mut start_index = 0usize;
    let mut skipped_width = 0usize;
    while start_index < line.len() && skipped_width < target_start {
        let character_width = UnicodeWidthChar::width(line[start_index]).unwrap_or(0);
        if skipped_width + character_width > target_start {
            break;
        }
        skipped_width += character_width;
        start_index += 1;
    }

    let mut rendered = String::new();
    let mut rendered_width = 0usize;
    for character in &line[start_index..] {
        let character_width = UnicodeWidthChar::width(*character).unwrap_or(0);
        if rendered_width + character_width > width {
            break;
        }
        rendered.push(*character);
        rendered_width += character_width;
    }
    (
        rendered,
        cursor_display.saturating_sub(skipped_width).min(width - 1),
        start_index,
    )
}

fn char_index_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

fn display_width(characters: &[char]) -> usize {
    characters
        .iter()
        .map(|character| UnicodeWidthChar::width(*character).unwrap_or(0))
        .sum()
}

fn truncate_to_width(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0usize;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > width {
            break;
        }
        output.push(character);
        used += character_width;
    }
    if used < width {
        output.push_str(&" ".repeat(width - used));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_buffer_supports_multiline_editing_without_losing_unicode() {
        let mut buffer = TextBuffer::from("echo hello\nworld");
        buffer.move_down();
        buffer.move_end();
        buffer.insert_char('!');
        buffer.move_up();
        buffer.move_end();
        buffer.insert_newline();
        buffer.insert_char('界');

        assert_eq!(buffer.to_string(), "echo hello\n界\nworld!");
    }

    #[test]
    fn tab_completion_uses_live_spar_identifiers() {
        let mut session = ShellSession::new();
        session.submit("var balance: int = 4;").unwrap();
        let mut buffer = TextBuffer::from("var result: int = balan");
        buffer.move_end();
        assert!(buffer.complete(&session).is_empty());
        assert_eq!(buffer.to_string(), "var result: int = balance");
    }

    #[test]
    fn cancel_keeps_the_original_text_while_save_returns_the_edit() {
        let original = "printf hello";
        assert_eq!(
            finalize_editor_result(original, "printf changed", EditorAction::Cancel),
            None
        );
        assert_eq!(
            finalize_editor_result(original, "printf changed", EditorAction::Save),
            Some(EditorResult {
                text: "printf changed".into(),
                action: EditorAction::Save
            })
        );
    }

    #[test]
    fn source_buffers_keep_indentation_and_dedent_closing_braces() {
        let mut buffer = TextBuffer::from("");
        buffer.auto_indent = true;
        for character in "fn f() {".chars() {
            buffer.insert_char(character);
        }
        buffer.insert_newline();
        assert_eq!(buffer.current_line().iter().collect::<String>(), "    ");
        assert!(buffer.at_indent_position());
        buffer.insert_char('}');
        assert_eq!(buffer.current_line().iter().collect::<String>(), "}");
        buffer.insert_newline();
        assert_eq!(buffer.current_line().iter().collect::<String>(), "");
    }

    #[test]
    fn pasted_text_is_inserted_verbatim_and_replaces_a_selection() {
        let mut buffer = TextBuffer::from("one two");
        buffer.auto_indent = true;
        buffer.column = 4;
        buffer.anchor = Some((0, 7));
        buffer.column = 7;
        buffer.anchor = Some((0, 4));
        assert_eq!(buffer.selected_text(), "two");
        buffer.insert_text("fn f() {\r\n    x;\n}");
        assert_eq!(buffer.to_string(), "one fn f() {\n    x;\n}");
        assert_eq!((buffer.row, buffer.column), (2, 1));
    }

    #[test]
    fn selection_spans_lines_and_deleting_joins_them() {
        let mut buffer = TextBuffer::from("abc\ndef\nghi");
        buffer.anchor = Some((0, 1));
        buffer.row = 2;
        buffer.column = 2;
        assert_eq!(buffer.selected_text(), "bc\ndef\ngh");
        assert_eq!(buffer.selected_columns(1), Some(0..3));
        assert!(buffer.delete_selection());
        assert_eq!(buffer.to_string(), "ai");
        buffer.select_all();
        assert_eq!(buffer.selected_text(), "ai");
    }

    #[test]
    fn clipboard_sequence_is_base64_osc52() {
        let mut out = Vec::new();
        copy_to_terminal_clipboard(&mut out, "hi").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b]52;c;aGk=\x07");
    }
}
