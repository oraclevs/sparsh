use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{
    self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use sparsh_core::{ShellSession, ShellUiSnapshot};
use unicode_width::UnicodeWidthChar;

use crate::highlight::paint_range;
use crate::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorAction {
    Save,
    Execute,
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
    }

    fn insert_newline(&mut self) {
        let column = self.column;
        let tail = self.current_line_mut().split_off(column);
        self.row += 1;
        self.lines.insert(self.row, tail);
        self.column = 0;
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
        execute!(output, EnterAlternateScreen, Hide)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut output = io::stdout();
        let _ = execute!(output, Show, LeaveAlternateScreen, ResetColor);
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
) -> io::Result<Option<EditorResult>> {
    let mut buffer = TextBuffer::from(initial);
    let mut output = io::stdout();
    let _guard = TerminalGuard::enter(&mut output)?;
    let mut viewport_row = 0usize;
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
            snapshot,
            &theme,
            &mut viewport_row,
        )?;
        match event::read()? {
            Event::Key(key) => {
                if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    continue;
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
                        KeyCode::Char('c') => return Ok(None),
                        KeyCode::Char('a') => buffer.move_home(),
                        KeyCode::Char('e') => buffer.move_end(),
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::F(5) if allow_execute => {
                        return Ok(finalize_editor_result(
                            initial,
                            &buffer.to_string(),
                            EditorAction::Execute,
                        ));
                    }
                    KeyCode::Char(character) => buffer.insert_char(character),
                    KeyCode::Enter => buffer.insert_newline(),
                    KeyCode::Backspace => buffer.backspace(),
                    KeyCode::Delete => buffer.delete(),
                    KeyCode::Left => buffer.move_left(),
                    KeyCode::Right => buffer.move_right(),
                    KeyCode::Up => buffer.move_up(),
                    KeyCode::Down => buffer.move_down(),
                    KeyCode::Home => buffer.move_home(),
                    KeyCode::End => buffer.move_end(),
                    KeyCode::Tab => {
                        for _ in 0..4 {
                            buffer.insert_char(' ');
                        }
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

pub fn edit_buffer_file(path: &Path) -> io::Result<()> {
    let original = fs::read_to_string(path)?;
    let session = ShellSession::try_new().map_err(|error| io::Error::other(error.to_string()))?;
    let snapshot = session.ui_snapshot();
    if let Some(result) = edit_text(&original, false, &snapshot)? {
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
    snapshot: &ShellUiSnapshot,
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
    if theme.enabled() {
        queue!(
            output,
            SetForegroundColor(Color::Cyan),
            SetAttribute(Attribute::Bold),
            Print("Sparsh Editor"),
            SetAttribute(Attribute::Reset),
            ResetColor,
            Print("  "),
            SetForegroundColor(Color::White),
            Print(format!("line {}:{}", buffer.row + 1, buffer.column + 1)),
            ResetColor,
        )?;
    } else {
        queue!(
            output,
            Print("Sparsh Editor  "),
            Print(format!("line {}:{}", buffer.row + 1, buffer.column + 1)),
        )?;
    }

    let editor_width = width.saturating_sub(1);
    let mut cursor_x = 0usize;
    let mut cursor_y = 1usize;
    for screen_row in 0..content_height {
        let row = *viewport_row + screen_row;
        queue!(output, MoveTo(0, (screen_row + 1) as u16), Clear(ClearType::CurrentLine))?;
        if row >= buffer.lines.len() {
            continue;
        }
        let line_cursor = if row == buffer.row { buffer.column } else { 0 };
        let (visible, x, start_char) = visible_line(&buffer.lines[row], line_cursor, editor_width);
        if row == buffer.row {
            cursor_x = x;
            cursor_y = screen_row + 1;
        }
        if theme.enabled() {
            let line = buffer.lines[row].iter().collect::<String>();
            let start_byte = char_index_to_byte(&line, start_char);
            let end_byte = start_byte + visible.len();
            queue!(output, Print(paint_range(&line, start_byte..end_byte, snapshot, theme)))?;
        } else {
            queue!(output, Print(visible))?;
        }
    }

    let status_row = height.saturating_sub(1);
    let help = if allow_execute {
        "Ctrl+S save draft  ·  F5 execute  ·  Esc cancel"
    } else {
        "Ctrl+S save to prompt  ·  Esc cancel"
    };
    queue!(output, MoveTo(0, status_row), Clear(ClearType::CurrentLine))?;
    if theme.enabled() {
        queue!(
            output,
            SetForegroundColor(Color::Black),
            crossterm::style::SetBackgroundColor(Color::Cyan),
            Print(truncate_to_width(help, width)),
            ResetColor,
        )?;
    } else {
        queue!(output, Print(truncate_to_width(help, width)))?;
    }
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
}
