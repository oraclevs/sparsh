//! Colored, pretty-printed output for `... |> to FORMAT` when nothing consumes
//! the bytes and a terminal shows the result.
//!
//! The value shown is exactly what the serializer would write: JSON and the
//! other document formats treat a stream of one value as that value, and
//! several as a list.

use spar::Value;

use crate::data_view::{render_value_view, RenderOptions};
use crate::styled::{join_lines, sanitize, scalar_role, Line, Role};
use crate::{SemanticRole, Theme};

pub(crate) fn render_encoded(
    format: &str,
    value: &Value,
    theme: &Theme,
    options: &RenderOptions,
) -> String {
    let elements = match value {
        Value::Table(table) => table.rows().to_vec(),
        Value::List(items) => items.clone(),
        other => vec![other.clone()],
    };
    let (lines, total) = match format {
        "json" => {
            let document = match elements.len() {
                1 => elements[0].clone(),
                _ => Value::List(elements),
            };
            let mut emitter = Emitter::new(true, options.max_lines);
            emitter.write_value(&document, 0);
            emitter.finish()
        }
        "jsonl" => {
            let total = elements.len();
            let lines = elements
                .iter()
                .take(options.max_lines)
                .map(|element| {
                    let mut emitter = Emitter::new(false, 1);
                    emitter.write_value(element, 0);
                    emitter.finish().0.into_iter().next().unwrap_or_default()
                })
                .collect();
            (lines, total)
        }
        "yaml" | "toml" | "csv" | "tsv" | "lines" | "text" => {
            let registry = spar::StructuredFormatRegistry::builtin();
            let bytes = match registry.encode_values(format, &elements) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return format!(
                        "{}\n{}",
                        theme.paint(SemanticRole::Error, &error.to_string()),
                        render_value_view(value, theme, options)
                    )
                }
            };
            let text = String::from_utf8_lossy(&bytes);
            let all = text.lines().collect::<Vec<_>>();
            let lines = all
                .iter()
                .take(options.max_lines)
                .enumerate()
                .map(|(number, line)| colorize(format, line, number == 0))
                .collect();
            (lines, all.len())
        }
        other => {
            return format!(
                "{}\n{}",
                theme.paint(SemanticRole::Error, &format!("unknown format `{other}`")),
                render_value_view(value, theme, options)
            )
        }
    };

    let mut output = join_lines(&lines, theme);
    if total > lines.len() {
        output.push('\n');
        output.push_str(&theme.paint(
            SemanticRole::Secondary,
            &format!(
                "… {} more lines ({} total), full value in `_`",
                total - lines.len(),
                total
            ),
        ));
    }
    output
}

fn colorize(format: &str, line: &str, first: bool) -> Line {
    match format {
        "yaml" => yaml_line(line),
        "toml" => toml_line(line),
        "csv" => delimited_line(line, ',', first),
        "tsv" => delimited_line(line, '\t', first),
        _ => Line::of(None, sanitize(line)),
    }
}

// ── JSON ─────────────────────────────────────────────────────────────────────

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{:04x}", u32::from(character)));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

/// Writes a value as JSON, either indented over many lines or on one line
/// with spaces after `:` and `,`. Only the first `limit` lines are kept; the
/// rest are counted.
struct Emitter {
    pretty: bool,
    limit: usize,
    lines: Vec<Line>,
    total: usize,
    current: Line,
}

impl Emitter {
    fn new(pretty: bool, limit: usize) -> Self {
        Self {
            pretty,
            limit,
            lines: Vec::new(),
            total: 0,
            current: Line::new(),
        }
    }

    fn finish(mut self) -> (Vec<Line>, usize) {
        self.newline();
        (self.lines, self.total)
    }

    fn newline(&mut self) {
        let line = std::mem::take(&mut self.current);
        self.total += 1;
        if self.lines.len() < self.limit {
            self.lines.push(line);
        }
    }

    fn punct(&mut self, text: &str) {
        self.current.push(Some(SemanticRole::DataPunct), text);
    }

    fn scalar(&mut self, role: Role, text: impl Into<String>) {
        self.current.push(role, text);
    }

    fn pad(&mut self, depth: usize) {
        if self.pretty {
            self.current.push(None, "  ".repeat(depth));
        }
    }

    fn object(&mut self, entries: Vec<(String, &Value)>, depth: usize) {
        if entries.is_empty() {
            return self.punct("{}");
        }
        self.punct("{");
        if self.pretty {
            self.newline();
        }
        let count = entries.len();
        for (index, (key, value)) in entries.into_iter().enumerate() {
            self.pad(depth + 1);
            self.scalar(Some(SemanticRole::DataKey), json_string(&key));
            self.punct(": ");
            self.write_value(value, depth + 1);
            if index + 1 < count {
                self.punct(if self.pretty { "," } else { ", " });
            }
            if self.pretty {
                self.newline();
            }
        }
        self.pad(depth);
        self.punct("}");
    }

    fn array(&mut self, items: &[Value], depth: usize) {
        if items.is_empty() {
            return self.punct("[]");
        }
        self.punct("[");
        if self.pretty {
            self.newline();
        }
        for (index, item) in items.iter().enumerate() {
            self.pad(depth + 1);
            self.write_value(item, depth + 1);
            if index + 1 < items.len() {
                self.punct(if self.pretty { "," } else { ", " });
            }
            if self.pretty {
                self.newline();
            }
        }
        self.pad(depth);
        self.punct("]");
    }

    fn write_value(&mut self, value: &Value, depth: usize) {
        match value {
            Value::Object(fields) => {
                let mut entries = fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value))
                    .collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                self.object(entries, depth);
            }
            Value::Map(entries) => {
                let entries = entries
                    .iter()
                    .map(|(key, value)| (crate::structured::plain_text(key), value))
                    .collect();
                self.object(entries, depth);
            }
            Value::List(items) => self.array(items, depth),
            Value::Table(table) => self.array(table.rows(), depth),
            Value::Int(value) => self.scalar(Some(SemanticRole::DataNumber), value.to_string()),
            Value::Float(value) if value.is_finite() => {
                self.scalar(Some(SemanticRole::DataNumber), value.to_string());
            }
            Value::Float(_) | Value::Void | Value::Option(None) => {
                self.scalar(Some(SemanticRole::DataNull), "null");
            }
            Value::Bool(value) => self.scalar(Some(SemanticRole::DataBool), value.to_string()),
            Value::String(text) => {
                self.scalar(Some(SemanticRole::DataString), json_string(text));
            }
            Value::Option(Some(inner)) | Value::Result(Ok(inner)) => {
                self.write_value(inner, depth);
            }
            other => self.scalar(
                Some(SemanticRole::DataString),
                json_string(&crate::structured::plain_text(other)),
            ),
        }
    }
}

// ── YAML / TOML / CSV ────────────────────────────────────────────────────────

/// Byte offset of the first unquoted `needle` for which `accept` holds.
fn find_unquoted(text: &str, needle: char, accept: impl Fn(&str) -> bool) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (offset, character) in text.char_indices() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => {}
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character == needle && accept(&text[offset + 1..]) => return Some(offset),
            None => {}
        }
    }
    None
}

/// Colors a scalar or inline collection (`"x"`, `42`, `[1, "a"]`, `{k: v}`).
fn inline(text: &str) -> Line {
    let mut line = Line::new();
    let characters = text.char_indices().collect::<Vec<_>>();
    let mut index = 0;
    while index < characters.len() {
        let (start, character) = characters[index];
        if character == '"' || character == '\'' {
            let mut end = index + 1;
            while end < characters.len() && characters[end].1 != character {
                if characters[end].1 == '\\' {
                    end += 1;
                }
                end += 1;
            }
            let stop = characters
                .get(end + 1)
                .map_or(text.len(), |(offset, _)| *offset);
            line.push(Some(SemanticRole::DataString), &text[start..stop]);
            index = end + 1;
        } else if matches!(character, '[' | ']' | '{' | '}' | ',' | ':' | '=') {
            line.push(Some(SemanticRole::DataPunct), character.to_string());
            index += 1;
        } else if character.is_whitespace() {
            line.push(None, character.to_string());
            index += 1;
        } else {
            let mut end = index;
            while end < characters.len()
                && !characters[end].1.is_whitespace()
                && !matches!(characters[end].1, '[' | ']' | '{' | '}' | ',')
            {
                end += 1;
            }
            let stop = characters
                .get(end)
                .map_or(text.len(), |(offset, _)| *offset);
            let token = &text[start..stop];
            line.push(Some(scalar_role(token)), token);
            index = end;
        }
    }
    line
}

fn yaml_line(line: &str) -> Line {
    let body = line.trim_start_matches(' ');
    let mut out = Line::of(None, &line[..line.len() - body.len()]);
    if body.starts_with('#') || body == "---" || body == "..." {
        out.push(Some(SemanticRole::DataPunct), body);
        return out;
    }
    let mut rest = body;
    loop {
        if let Some(after) = rest.strip_prefix("- ") {
            out.push(Some(SemanticRole::DataPunct), "- ");
            rest = after.trim_start_matches(' ');
        } else if rest == "-" {
            out.push(Some(SemanticRole::DataPunct), "-");
            return out;
        } else {
            break;
        }
    }
    match find_unquoted(rest, ':', |after| {
        after.is_empty() || after.starts_with(' ')
    }) {
        Some(offset) if offset > 0 => {
            out.push(Some(SemanticRole::DataKey), &rest[..offset]);
            out.push(Some(SemanticRole::DataPunct), ":");
            out.extend(inline(&rest[offset + 1..]));
        }
        _ => out.extend(inline(rest)),
    }
    out
}

fn toml_line(line: &str) -> Line {
    let body = line.trim_start();
    if body.starts_with('[') && find_unquoted(body, '=', |_| true).is_none() {
        return Line::of(Some(SemanticRole::TableHeader), sanitize(line));
    }
    let mut out = Line::of(None, &line[..line.len() - body.len()]);
    if body.starts_with('#') {
        out.push(Some(SemanticRole::DataPunct), body);
        return out;
    }
    match find_unquoted(body, '=', |_| true) {
        Some(offset) => {
            out.push(Some(SemanticRole::DataKey), body[..offset].trim_end());
            out.push(None, " ");
            out.push(Some(SemanticRole::DataPunct), "=");
            out.extend(inline(&body[offset + 1..]));
        }
        None => out.extend(inline(body)),
    }
    out
}

fn delimited_line(line: &str, delimiter: char, header: bool) -> Line {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (offset, character) in line.char_indices() {
        if character == '"' {
            quoted = !quoted;
        } else if character == delimiter && !quoted {
            fields.push(&line[start..offset]);
            start = offset + character.len_utf8();
        }
    }
    fields.push(&line[start..]);

    let mut out = Line::new();
    for (index, field) in fields.into_iter().enumerate() {
        if index > 0 {
            out.push(Some(SemanticRole::DataPunct), delimiter.to_string());
        }
        let role = if header {
            Some(SemanticRole::TableHeader)
        } else if field.is_empty() {
            None
        } else if field.starts_with('"') {
            Some(SemanticRole::DataString)
        } else {
            Some(scalar_role(field))
        };
        out.push(role, sanitize(field));
    }
    out
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use spar::Value;

    use super::render_encoded;
    use crate::data_view::RenderOptions;
    use crate::{SemanticRole, Theme};

    fn record(name: &str, age: i64) -> Value {
        Value::Object(IndexMap::from([
            ("name".into(), Value::String(name.into())),
            ("age".into(), Value::Int(age)),
        ]))
    }

    #[test]
    fn json_is_pretty_and_semantically_colored() {
        let value = Value::Object(IndexMap::from([
            ("name".into(), Value::String("Obi".into())),
            ("age".into(), Value::Int(24)),
            ("active".into(), Value::Bool(true)),
            ("missing".into(), Value::Option(None)),
        ]));
        let options = RenderOptions::new(100);

        let plain = render_encoded("json", &value, &Theme::plain(), &options);
        assert!(plain.contains("\n  \"active\": true"), "{plain}");
        assert!(plain.contains("\n  \"age\": 24"), "{plain}");
        assert!(!plain.contains("\x1b["));

        let theme = Theme::colored();
        let colored = render_encoded("json", &value, &theme, &options);
        assert!(
            colored.contains(&theme.paint(SemanticRole::DataKey, "\"name\"")),
            "{colored}"
        );
        assert!(
            colored.contains(&theme.paint(SemanticRole::DataString, "\"Obi\"")),
            "{colored}"
        );
        assert!(
            colored.contains(&theme.paint(SemanticRole::DataNumber, "24")),
            "{colored}"
        );
        assert!(
            colored.contains(&theme.paint(SemanticRole::DataBool, "true")),
            "{colored}"
        );
        assert!(
            colored.contains(&theme.paint(SemanticRole::DataNull, "null")),
            "{colored}"
        );
    }

    #[test]
    fn jsonl_stays_one_json_value_per_physical_line() {
        let value = Value::List(vec![record("Obi", 24), record("Ada", 31)]);
        let output = render_encoded("jsonl", &value, &Theme::plain(), &RenderOptions::new(100));
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{output}");
        assert!(
            lines
                .iter()
                .all(|line| line.starts_with('{') && line.ends_with('}')),
            "{output}"
        );
    }

    #[test]
    fn yaml_and_toml_color_keys_and_typed_scalars() {
        let value = Value::Object(IndexMap::from([
            ("active".into(), Value::Bool(true)),
            ("age".into(), Value::Int(24)),
            ("name".into(), Value::String("Obi".into())),
        ]));
        let options = RenderOptions::new(100);

        for format in ["yaml", "toml"] {
            let plain = render_encoded(format, &value, &Theme::plain(), &options);
            assert!(plain.contains("name"), "{format}: {plain}");
            assert!(plain.contains("Obi"), "{format}: {plain}");
            assert!(plain.contains("24"), "{format}: {plain}");
            assert!(plain.contains("true"), "{format}: {plain}");
            assert!(!plain.contains("\x1b["), "{format}: {plain}");

            let theme = Theme::colored();
            let colored = render_encoded(format, &value, &theme, &options);
            assert!(
                colored.contains(&theme.paint(SemanticRole::DataKey, "name")),
                "{format}: {colored}"
            );
            assert!(colored.contains("\x1b["), "{format}: {colored}");
        }
    }

    #[test]
    fn csv_and_tsv_color_headers_and_keep_delimiters_visible() {
        let value = Value::List(vec![
            Value::Object(IndexMap::from([
                ("age".into(), Value::Int(24)),
                ("name".into(), Value::String("Obi".into())),
                ("team".into(), Value::String("core".into())),
            ])),
            Value::Object(IndexMap::from([
                ("age".into(), Value::Int(31)),
                ("name".into(), Value::String("Ada".into())),
                ("team".into(), Value::String("ops".into())),
            ])),
        ]);
        let options = RenderOptions::new(100);

        for (format, delimiter) in [("csv", ','), ("tsv", '\t')] {
            let plain = render_encoded(format, &value, &Theme::plain(), &options);
            let header = plain.lines().next().expect("header row");
            assert!(
                header.contains("age") && header.contains("name") && header.contains("team"),
                "{format}: {header}"
            );
            assert!(header.contains(delimiter), "{format}: {header}");
            assert!(
                plain.contains("Obi") && plain.contains("Ada"),
                "{format}: {plain}"
            );

            let theme = Theme::colored();
            let colored = render_encoded(format, &value, &theme, &options);
            for header in ["age", "name", "team"] {
                assert!(
                    colored.contains(&theme.paint(SemanticRole::TableHeader, header)),
                    "{format}: missing colored header {header}: {colored}"
                );
            }
            assert!(colored.contains(delimiter), "{format}: {colored}");
        }
    }

    #[test]
    fn default_preview_bounds_a_two_hundred_line_json_document() {
        let value = Value::List(
            (0..75)
                .map(|n| Value::Object(IndexMap::from([("n".into(), Value::Int(n))])))
                .collect(),
        );
        let options = RenderOptions::new(100);
        let output = render_encoded("json", &value, &Theme::plain(), &options);

        assert!(output.contains("more lines"), "{output}");
        assert!(output.contains("full value in `_`"), "{output}");
        assert!(output.lines().count() <= options.max_lines + 1, "{output}");
    }

    #[test]
    fn encoded_document_has_a_bounded_line_preview() {
        let value = Value::List(
            (0..200)
                .map(|n| Value::Object(IndexMap::from([("n".into(), Value::Int(n))])))
                .collect(),
        );
        let output = render_encoded(
            "json",
            &value,
            &Theme::plain(),
            &RenderOptions {
                width: 100,
                max_rows: 50,
                max_lines: 20,
            },
        );
        assert!(output.contains("more lines"), "{output}");
        assert!(output.contains("full value in `_`"), "{output}");
        assert!(output.lines().count() <= 21, "{output}");
    }
}
