//! Nushell-style rendering of structured values: bordered tables with an index
//! column, key/value views for records, nested cells, and terminal-width fitting.

use std::collections::BTreeSet;

use spar::{TableValue, Value};

use crate::styled::{join_lines, sanitize, Line, Role};
use crate::{SemanticRole, Theme};

/// Columns narrower than this are never squeezed further; extra columns are
/// hidden behind a `…` column instead.
const MIN_COLUMN: usize = 8;
/// A single table cell shows at most this many lines.
const MAX_CELL_LINES: usize = 6;
/// Nested records/lists deeper than this collapse into a one-line summary.
const MAX_DEPTH: usize = 3;
/// Lists of scalars up to this many items are written on one line.
const MAX_INLINE_ITEMS: usize = 8;
const MAX_INLINE_WIDTH: usize = 48;

pub(crate) const DEFAULT_MAX_ROWS: usize = 50;
pub(crate) const DEFAULT_MAX_LINES: usize = 80;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RenderOptions {
    /// Terminal width in columns.
    pub width: usize,
    /// Table rows / record fields shown before the "more rows" footer.
    pub max_rows: usize,
    /// Lines of encoded text (JSON, YAML, ...) shown before the footer.
    pub max_lines: usize,
}

impl RenderOptions {
    pub(crate) fn new(width: usize) -> Self {
        Self {
            width: width.max(20),
            max_rows: DEFAULT_MAX_ROWS,
            max_lines: DEFAULT_MAX_LINES,
        }
    }

    /// The current terminal width, with `SPARSH_MAX_ROWS` / `SPARSH_MAX_LINES`
    /// overriding the preview limits.
    pub(crate) fn for_terminal() -> Self {
        let mut options = Self::new(crate::editor::terminal_width());
        if let Some(rows) = env_limit("SPARSH_MAX_ROWS") {
            options.max_rows = rows;
        }
        if let Some(lines) = env_limit("SPARSH_MAX_LINES") {
            options.max_lines = lines;
        }
        options
    }
}

fn env_limit(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|limit| *limit > 0)
}

// ── Values as cell lines ─────────────────────────────────────────────────────

/// Scalars that fit on one line. `None` for containers and multi-line text.
fn leaf(value: &Value) -> Option<Line> {
    let number = Some(SemanticRole::DataNumber);
    let null = Some(SemanticRole::DataNull);
    Some(match value {
        Value::Void => Line::of(null, "null"),
        Value::Int(value) => Line::of(number, value.to_string()),
        Value::Float(value) => Line::of(number, value.to_string()),
        Value::Bool(value) => Line::of(Some(SemanticRole::DataBool), value.to_string()),
        Value::String(value) if value.contains('\n') => return None,
        Value::String(value) => Line::of(Some(SemanticRole::DataString), sanitize(value)),
        Value::Bytes(value) => Line::of(null, format!("bytes[{}]", value.len())),
        Value::Option(None) => Line::of(null, "null"),
        Value::Option(Some(inner)) => return leaf(inner),
        Value::Result(Ok(inner)) => return leaf(inner),
        Value::Result(Err(inner)) => Line::of(
            Some(SemanticRole::Error),
            format!("Err({})", crate::structured::plain_text(inner)),
        ),
        Value::Error {
            message,
            kind,
            code,
            ..
        } => Line::of(
            Some(SemanticRole::Error),
            sanitize(&format!("error[{kind}:{code}] {message}")),
        ),
        Value::Object(_) | Value::List(_) | Value::Map(_) | Value::Table(_) => return None,
        other => Line::of(null, sanitize(&crate::structured::plain_text(other))),
    })
}

fn summary(text: String) -> Line {
    Line::of(Some(SemanticRole::DataNull), text)
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn indent(line: Line, by: usize) -> Line {
    let mut out = Line::of(None, " ".repeat(by));
    out.extend(line);
    out
}

/// `[1, 2, 3]` for short lists of scalars.
fn inline_list(items: &[Value]) -> Option<Line> {
    if items.len() > MAX_INLINE_ITEMS {
        return None;
    }
    let mut line = Line::of(Some(SemanticRole::DataPunct), "[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            line.push(Some(SemanticRole::DataPunct), ", ");
        }
        line.extend(leaf(item)?);
    }
    line.push(Some(SemanticRole::DataPunct), "]");
    (line.width() <= MAX_INLINE_WIDTH).then_some(line)
}

fn inline_value(value: &Value) -> Option<Line> {
    match value {
        Value::List(items) if items.is_empty() => Some(summary("[]".into())),
        Value::List(items) => inline_list(items),
        Value::Object(fields) if fields.is_empty() => Some(summary("{}".into())),
        other => leaf(other),
    }
}

fn labelled(key: &str, value: Line) -> Line {
    let mut line = Line::of(Some(SemanticRole::DataKey), sanitize(key));
    line.push(Some(SemanticRole::DataPunct), ": ");
    line.extend(value);
    line
}

/// A value as the lines of one cell (or of a record's value column).
fn value_lines(value: &Value, depth: usize) -> Vec<Line> {
    match value {
        Value::String(text) if text.contains('\n') => text
            .lines()
            .map(|line| Line::of(Some(SemanticRole::DataString), sanitize(line)))
            .collect(),
        Value::Object(fields) => {
            if fields.is_empty() {
                return vec![summary("{}".into())];
            }
            if depth >= MAX_DEPTH {
                return vec![summary(format!(
                    "{{{}}}",
                    plural(fields.len(), "field", "fields")
                ))];
            }
            let mut entries = fields.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(name, _)| *name);
            entries_lines(
                entries.into_iter().map(|(key, value)| (key.clone(), value)),
                depth,
            )
        }
        Value::Map(entries) => {
            if depth >= MAX_DEPTH {
                return vec![summary(format!(
                    "{{{}}}",
                    plural(entries.len(), "entry", "entries")
                ))];
            }
            entries_lines(
                entries
                    .iter()
                    .map(|(key, value)| (crate::structured::plain_text(key), value)),
                depth,
            )
        }
        Value::List(items) => list_lines(items, depth),
        Value::Table(table) => list_lines(table.rows(), depth),
        other => vec![leaf(other).unwrap_or_default()],
    }
}

fn entries_lines<'a>(
    entries: impl Iterator<Item = (String, &'a Value)>,
    depth: usize,
) -> Vec<Line> {
    let mut out = Vec::new();
    for (key, value) in entries {
        if let Some(inline) = inline_value(value) {
            out.push(labelled(&key, inline));
            continue;
        }
        out.push(labelled(&key, Line::new()));
        out.extend(
            value_lines(value, depth + 1)
                .into_iter()
                .map(|line| indent(line, 2)),
        );
    }
    out
}

fn list_lines(items: &[Value], depth: usize) -> Vec<Line> {
    if items.is_empty() {
        return vec![summary("[]".into())];
    }
    if depth >= MAX_DEPTH {
        return vec![summary(format!(
            "[{}]",
            plural(items.len(), "item", "items")
        ))];
    }
    if let Some(inline) = inline_list(items) {
        return vec![inline];
    }
    let marker = Some(SemanticRole::DataPunct);
    let mut out = Vec::new();
    for item in items {
        let mut nested = value_lines(item, depth + 1).into_iter();
        let mut first = Line::of(marker, "- ");
        first.extend(nested.next().unwrap_or_default());
        out.push(first);
        out.extend(nested.map(|line| indent(line, 2)));
    }
    out
}

// ── Grid ─────────────────────────────────────────────────────────────────────

struct Cell {
    lines: Vec<Line>,
    numeric: bool,
}

impl Cell {
    fn of(value: &Value) -> Self {
        Self::from_lines(value_lines(value, 0))
    }

    fn text(role: Role, text: impl Into<String>) -> Self {
        Self::from_lines(vec![Line::of(role, sanitize(&text.into()))])
    }

    fn from_lines(lines: Vec<Line>) -> Self {
        let numeric = matches!(lines.as_slice(), [line] if line.sole_role() == Some(SemanticRole::DataNumber));
        Self { lines, numeric }
    }

    fn empty() -> Self {
        Self::from_lines(vec![Line::new()])
    }

    fn width(&self) -> usize {
        self.lines.iter().map(Line::width).max().unwrap_or(0)
    }
}

struct Column {
    header: Option<Line>,
    cells: Vec<Cell>,
    /// The `#` column: never squeezed, right-aligned.
    index: bool,
}

impl Column {
    fn natural_width(&self) -> usize {
        let header = self.header.as_ref().map_or(0, Line::width);
        self.cells
            .iter()
            .map(Cell::width)
            .max()
            .unwrap_or(0)
            .max(header)
            .max(1)
    }

    /// Numbers line up on the right, like Nushell.
    fn right_aligned(&self) -> bool {
        self.index
            || (self.cells.iter().any(|cell| cell.numeric)
                && self
                    .cells
                    .iter()
                    .all(|cell| cell.numeric || cell.width() == 0))
    }
}

fn border(left: char, middle: char, right: char, widths: &[usize]) -> Line {
    let role = Some(SemanticRole::TableBorder);
    let mut text = String::new();
    text.push(left);
    for (index, width) in widths.iter().enumerate() {
        text.push_str(&"─".repeat(width + 2));
        text.push(if index + 1 == widths.len() {
            right
        } else {
            middle
        });
    }
    Line::of(role, text)
}

fn pad(mut line: Line, width: usize, right: bool) -> Line {
    let fill = " ".repeat(width.saturating_sub(line.width()));
    if right {
        let mut out = Line::of(None, fill);
        out.extend(line);
        out
    } else {
        line.push(None, fill);
        line
    }
}

fn wrap_cell(lines: &[Line], width: usize) -> Vec<Line> {
    let mut wrapped = lines
        .iter()
        .flat_map(|line| line.wrap(width))
        .collect::<Vec<_>>();
    if wrapped.len() > MAX_CELL_LINES {
        wrapped.truncate(MAX_CELL_LINES);
        if let Some(last) = wrapped.pop() {
            wrapped.push(last.ellipsize(width, true));
        }
    }
    wrapped
}

fn row_line(pieces: Vec<Line>) -> Line {
    let bar = Some(SemanticRole::TableBorder);
    let mut line = Line::of(bar, "│");
    for piece in pieces {
        line.push(None, " ");
        line.extend(piece);
        line.push(None, " ");
        line.push(bar, "│");
    }
    line
}

/// Lays `columns` out within `width` columns. Returns the lines and how many
/// trailing columns had to be hidden.
fn draw_grid(mut columns: Vec<Column>, rows: usize, width: usize) -> (Vec<Line>, usize) {
    let total = columns.len();
    let natural = columns
        .iter()
        .map(Column::natural_width)
        .collect::<Vec<_>>();
    let mins = columns
        .iter()
        .zip(&natural)
        .map(|(column, natural)| {
            if column.index {
                *natural
            } else {
                (*natural).min(MIN_COLUMN)
            }
        })
        .collect::<Vec<_>>();

    // Keep as many leading columns as fit at their minimum width, reserving
    // room for a `…` column when some are dropped.
    let overhead = |count: usize| 3 * count + 1;
    let first_data = usize::from(columns.first().is_some_and(|column| column.index));
    let mut keep = total;
    while keep > first_data + 1 {
        let extra = if keep < total { 1 + 3 } else { 0 };
        if mins[..keep].iter().sum::<usize>() + overhead(keep) + extra <= width {
            break;
        }
        keep -= 1;
    }
    let hidden = total - keep;
    let mut widths = natural[..keep].to_vec();
    let mut mins = mins[..keep].to_vec();
    columns.truncate(keep);
    if hidden > 0 {
        columns.push(Column {
            header: Some(Line::of(Some(SemanticRole::DataNull), "…")),
            cells: (0..rows)
                .map(|_| Cell::text(Some(SemanticRole::DataNull), "…"))
                .collect(),
            index: false,
        });
        widths.push(1);
        mins.push(1);
    }

    let mut used = widths.iter().sum::<usize>() + overhead(widths.len());
    while used > width {
        let Some(widest) = (0..widths.len())
            .filter(|index| widths[*index] > mins[*index])
            .max_by_key(|index| widths[*index])
        else {
            break;
        };
        widths[widest] -= 1;
        used -= 1;
    }

    let has_header = columns.iter().any(|column| column.header.is_some());
    let mut lines = vec![border('╭', '┬', '╮', &widths)];
    if has_header {
        let header_pieces = columns
            .iter()
            .zip(&widths)
            .map(|(column, width)| {
                let header = column.header.clone().unwrap_or_default();
                let header = paint_header(header).ellipsize(*width, false);
                pad(header, *width, column.right_aligned())
            })
            .collect();
        lines.push(row_line(header_pieces));
        lines.push(border('├', '┼', '┤', &widths));
    }
    let aligned = columns
        .iter()
        .map(Column::right_aligned)
        .collect::<Vec<_>>();
    for row in 0..rows {
        let wrapped = columns
            .iter()
            .zip(&widths)
            .map(|(column, width)| wrap_cell(&column.cells[row].lines, *width))
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for physical in 0..height {
            let pieces = wrapped
                .iter()
                .zip(&widths)
                .zip(&aligned)
                .map(|((cell, width), right)| {
                    pad(
                        cell.get(physical).cloned().unwrap_or_default(),
                        *width,
                        *right,
                    )
                })
                .collect();
            lines.push(row_line(pieces));
        }
    }
    lines.push(border('╰', '┴', '╯', &widths));
    (lines, hidden)
}

/// Header text carries the header role; already-styled headers are kept.
fn paint_header(header: Line) -> Line {
    if header.sole_role().is_some() {
        return header;
    }
    Line::of(Some(SemanticRole::TableHeader), header.plain())
}

fn index_column(rows: usize, offset: usize) -> Column {
    Column {
        header: Some(Line::of(Some(SemanticRole::TableHeader), "#")),
        cells: (0..rows)
            .map(|row| Cell::text(Some(SemanticRole::TableIndex), (offset + row).to_string()))
            .collect(),
        index: true,
    }
}

fn header(name: &str) -> Option<Line> {
    Some(Line::of(Some(SemanticRole::TableHeader), sanitize(name)))
}

// ── Footers ──────────────────────────────────────────────────────────────────

fn dim(theme: &Theme, text: &str) -> String {
    theme.paint(SemanticRole::Secondary, text)
}

fn finish(
    mut lines: Vec<String>,
    theme: &Theme,
    count: usize,
    shown: usize,
    unit: (&str, &str),
    hidden_columns: usize,
) -> String {
    if hidden_columns > 0 {
        lines.push(dim(
            theme,
            &format!(
                "… {} hidden: widen the terminal or pick columns with select([...])",
                plural(hidden_columns, "column", "columns")
            ),
        ));
    }
    if shown < count {
        lines.push(dim(
            theme,
            &format!(
                "… {} more {} ({} total), full value in `_`",
                count - shown,
                if count - shown == 1 { unit.0 } else { unit.1 },
                count
            ),
        ));
    } else {
        lines.push(dim(theme, &plural(count, unit.0, unit.1)));
    }
    lines.join("\n")
}

fn paint_all(lines: &[Line], theme: &Theme) -> Vec<String> {
    lines.iter().map(|line| line.paint(theme)).collect()
}

// ── Views ────────────────────────────────────────────────────────────────────

pub(crate) fn render_table(table: &TableValue, theme: &Theme, options: &RenderOptions) -> String {
    let names = table
        .schema()
        .fields
        .iter()
        .map(|field| field.name.clone())
        .collect::<Vec<_>>();
    if names.is_empty() {
        return if table.is_empty() {
            theme.paint(SemanticRole::Secondary, "(empty table)")
        } else {
            format!("{} rows", table.len())
        };
    }
    records_view(table.rows(), &names, theme, options)
}

/// A table of `rows` (records, or scalars when a row is not a record) with the
/// given column order.
fn records_view(
    rows: &[Value],
    names: &[String],
    theme: &Theme,
    options: &RenderOptions,
) -> String {
    let shown = rows.len().min(options.max_rows);
    let mut columns = vec![index_column(shown, 0)];
    for name in names {
        columns.push(Column {
            header: header(name),
            cells: rows[..shown]
                .iter()
                .map(|row| match row {
                    Value::Object(fields) => fields.get(name).map_or_else(Cell::empty, Cell::of),
                    other if names.first() == Some(name) => Cell::of(other),
                    _ => Cell::empty(),
                })
                .collect(),
            index: false,
        });
    }
    let (lines, hidden) = draw_grid(columns, shown, options.width);
    finish(
        paint_all(&lines, theme),
        theme,
        rows.len(),
        shown,
        ("row", "rows"),
        hidden,
    )
}

fn list_view(items: &[Value], theme: &Theme, options: &RenderOptions) -> String {
    if items.is_empty() {
        return theme.paint(SemanticRole::Secondary, "(empty list)");
    }
    if items.iter().all(|item| matches!(item, Value::Object(_))) {
        let mut names = BTreeSet::new();
        for item in items {
            if let Value::Object(fields) = item {
                names.extend(fields.keys().cloned());
            }
        }
        return records_view(
            items,
            &names.into_iter().collect::<Vec<_>>(),
            theme,
            options,
        );
    }
    let shown = items.len().min(options.max_rows);
    let columns = vec![
        index_column(shown, 0),
        Column {
            header: header("value"),
            cells: items[..shown].iter().map(Cell::of).collect(),
            index: false,
        },
    ];
    let (lines, hidden) = draw_grid(columns, shown, options.width);
    finish(
        paint_all(&lines, theme),
        theme,
        items.len(),
        shown,
        ("item", "items"),
        hidden,
    )
}

fn record_view(
    fields: &indexmap::IndexMap<String, Value>,
    theme: &Theme,
    options: &RenderOptions,
) -> String {
    if fields.is_empty() {
        return theme.paint(SemanticRole::Secondary, "(empty record)");
    }
    let mut entries = fields.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(name, _)| *name);
    let shown = entries.len().min(options.max_rows);
    let columns = vec![
        Column {
            header: None,
            cells: entries[..shown]
                .iter()
                .map(|(name, _)| Cell::text(Some(SemanticRole::DataKey), name.as_str()))
                .collect(),
            index: false,
        },
        Column {
            header: None,
            cells: entries[..shown]
                .iter()
                .map(|(_, value)| Cell::of(value))
                .collect(),
            index: false,
        },
    ];
    let (lines, hidden) = draw_grid(columns, shown, options.width);
    finish(
        paint_all(&lines, theme),
        theme,
        entries.len(),
        shown,
        ("field", "fields"),
        hidden,
    )
}

/// Any value the way the prompt shows it: tables, records and lists as grids,
/// scalars as plain (colored) text.
pub(crate) fn render_value_view(value: &Value, theme: &Theme, options: &RenderOptions) -> String {
    match value {
        Value::Table(table) => render_table(table, theme, options),
        Value::List(items) => list_view(items, theme, options),
        Value::Object(fields) => record_view(fields, theme, options),
        Value::String(text) => text.clone(),
        other => match leaf(other) {
            Some(line) => line.paint(theme),
            None => join_lines(&value_lines(other, 0), theme),
        },
    }
}

/// A small fixed grid of text cells (used for schemas).
pub(crate) fn render_text_grid(
    headers: &[&str],
    rows: &[Vec<String>],
    theme: &Theme,
    options: &RenderOptions,
) -> String {
    let columns = headers
        .iter()
        .enumerate()
        .map(|(position, name)| Column {
            header: header(name),
            cells: rows
                .iter()
                .map(|row| Cell::text(None, row.get(position).cloned().unwrap_or_default()))
                .collect(),
            index: false,
        })
        .collect();
    let (lines, _) = draw_grid(columns, rows.len(), options.width);
    join_lines(&lines, theme)
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use spar::{TableValue, Value};
    use unicode_width::UnicodeWidthStr;

    use super::{render_table, render_value_view, RenderOptions};
    use crate::{SemanticRole, Theme};

    fn sample_table() -> TableValue {
        TableValue::from_records(vec![Value::Object(IndexMap::from([
            ("name".into(), Value::String("Obi".into())),
            ("age".into(), Value::Int(24)),
            ("active".into(), Value::Bool(true)),
            ("note".into(), Value::Option(None)),
        ]))])
        .unwrap()
    }

    fn is_grid_line(line: &str) -> bool {
        matches!(line.chars().next(), Some('╭' | '│' | '├' | '╰'))
    }

    #[test]
    fn plain_table_has_index_sorted_headers_and_no_ansi() {
        let output = render_table(&sample_table(), &Theme::plain(), &RenderOptions::new(100));
        assert!(output.contains('#'));
        let active = output.find("active").unwrap();
        let age = output.find("age").unwrap();
        let name = output.find("name").unwrap();
        let note = output.find("note").unwrap();
        assert!(active < age && age < name && name < note, "{output}");
        assert!(output.contains("│ 0 "), "{output}");
        assert!(!output.contains("\x1b["));
    }

    #[test]
    fn void_values_in_a_record_show_as_null_not_blank() {
        let record = Value::Object(IndexMap::from([
            ("id".into(), Value::Int(5)),
            ("meta".into(), Value::Void),
        ]));
        let output = render_value_view(&record, &Theme::plain(), &RenderOptions::new(60));
        assert!(output.contains("null"), "{output}");
    }

    #[test]
    fn colored_table_uses_semantic_roles_for_values() {
        let theme = Theme::colored();
        let output = render_table(&sample_table(), &theme, &RenderOptions::new(100));
        assert!(
            output.contains(&theme.paint(SemanticRole::TableHeader, "age")),
            "{output}"
        );
        assert!(
            output.contains(&theme.paint(SemanticRole::DataNumber, "24")),
            "{output}"
        );
        assert!(
            output.contains(&theme.paint(SemanticRole::DataString, "Obi")),
            "{output}"
        );
        assert!(
            output.contains(&theme.paint(SemanticRole::DataBool, "true")),
            "{output}"
        );
        assert!(
            output.contains(&theme.paint(SemanticRole::DataNull, "null")),
            "{output}"
        );
    }

    #[test]
    fn narrow_table_hides_columns_without_exceeding_width() {
        let options = RenderOptions {
            width: 32,
            max_rows: 50,
            max_lines: 300,
        };
        let output = render_table(&sample_table(), &Theme::plain(), &options);
        assert!(output.contains("hidden"), "{output}");
        for line in output.lines().filter(|line| is_grid_line(line)) {
            assert!(UnicodeWidthStr::width(line) <= 32, "{line:?}");
        }
    }

    #[test]
    fn wide_unicode_cells_keep_valid_width_and_utf8() {
        let table = TableValue::from_records(vec![Value::Object(IndexMap::from([(
            "name".into(),
            Value::String("猫🙂東京🙂猫".repeat(8)),
        )]))])
        .unwrap();
        let output = render_table(
            &table,
            &Theme::plain(),
            &RenderOptions {
                width: 28,
                max_rows: 50,
                max_lines: 300,
            },
        );
        assert!(output.is_char_boundary(output.len()));
        for line in output.lines().filter(|line| is_grid_line(line)) {
            assert!(UnicodeWidthStr::width(line) <= 28, "{line:?}");
        }
    }

    #[test]
    fn nested_values_are_readable_instead_of_one_long_dump() {
        let value = Value::Object(IndexMap::from([(
            "user".into(),
            Value::Object(IndexMap::from([
                ("name".into(), Value::String("Obi".into())),
                (
                    "tags".into(),
                    Value::List(vec![
                        Value::String("rust".into()),
                        Value::String("flutter".into()),
                    ]),
                ),
            ])),
        )]));
        let output = render_value_view(&value, &Theme::plain(), &RenderOptions::new(80));
        assert!(output.lines().count() > 3, "{output}");
        assert!(output.contains("user"));
        assert!(output.contains("tags"));
        assert!(output.contains("Obi"));
    }

    #[test]
    fn large_table_is_bounded_and_reports_remaining_rows() {
        let rows = (0..225)
            .map(|n| Value::Object(IndexMap::from([("n".into(), Value::Int(n))])))
            .collect();
        let table = TableValue::from_records(rows).unwrap();
        let output = render_table(
            &table,
            &Theme::plain(),
            &RenderOptions {
                width: 80,
                max_rows: 50,
                max_lines: 300,
            },
        );
        assert!(
            output.contains("… 175 more rows (225 total), full value in `_`"),
            "{output}"
        );
        assert!(!output.contains("│ 50 "), "{output}");
    }

    #[test]
    fn multiline_strings_keep_the_data_string_role() {
        let table = TableValue::from_records(vec![Value::Object(IndexMap::from([(
            "message".into(),
            Value::String("hello\nworld".into()),
        )]))])
        .unwrap();
        let theme = Theme::colored();
        let output = render_table(&table, &theme, &RenderOptions::new(80));
        assert!(
            output.contains(&theme.paint(SemanticRole::DataString, "hello")),
            "{output}"
        );
        assert!(
            output.contains(&theme.paint(SemanticRole::DataString, "world")),
            "{output}"
        );
    }
}
