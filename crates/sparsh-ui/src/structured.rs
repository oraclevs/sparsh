use std::fmt::Write as _;

use spar::{InteractivePresentation, InteractiveRuntimeValue, Schema, Value};

use crate::data_view::{render_table, render_text_grid, render_value_view, RenderOptions};
use crate::encoded::render_encoded;
use crate::http_view::{is_http_response, render_http_response};
use crate::{SemanticRole, Theme};

/// Script output (`-c`, piped stdin) is data, not UI: tables and schemas come
/// out as JSON Lines and other structured values as one JSON document, with no
/// borders, colors, or row counts.
pub(crate) fn render_structured_plain(value: &InteractiveRuntimeValue) -> String {
    let registry = spar::StructuredFormatRegistry::builtin();
    let (format, values): (&str, Vec<Value>) = match &value.value {
        Value::Table(table) => ("jsonl", table.rows().to_vec()),
        Value::Schema(schema) => (
            "jsonl",
            schema
                .fields
                .iter()
                .map(|field| {
                    Value::Object(
                        [
                            ("field".to_string(), Value::String(field.name.clone())),
                            ("type".to_string(), Value::String(field.ty.display_name())),
                            ("optional".to_string(), Value::Bool(field.optional)),
                        ]
                        .into_iter()
                        .collect(),
                    )
                })
                .collect(),
        ),
        other => ("json", vec![other.clone()]),
    };
    match registry.encode_values(format, &values) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).trim_end().to_string(),
        Err(_) => plain_text(&value.value),
    }
}

pub(crate) fn render_structured_value(
    value: &InteractiveRuntimeValue,
    theme: &Theme,
    options: &RenderOptions,
) -> String {
    match value.presentation {
        InteractivePresentation::Schema => render_schema_value(&value.value, theme, options),
        InteractivePresentation::Inspect => render_inspect(&value.value, theme, options),
        InteractivePresentation::Encoded(format) => {
            render_encoded(format, &value.value, theme, options)
        }
        InteractivePresentation::Pipeline | InteractivePresentation::Value => match &value.value {
            Value::Schema(schema) => render_schema(schema, theme, options),
            Value::Object(fields) if is_http_response(fields) => {
                render_http_response(fields, theme, options)
            }
            other => render_value_view(other, theme, options),
        },
    }
}

fn render_schema_value(value: &Value, theme: &Theme, options: &RenderOptions) -> String {
    match value {
        Value::Schema(schema) => render_schema(schema, theme, options),
        other => format!(
            "{}\n{}",
            theme.paint(SemanticRole::Secondary, "schema"),
            plain_text(other)
        ),
    }
}

fn render_schema(schema: &Schema, theme: &Theme, options: &RenderOptions) -> String {
    let rows = schema
        .fields
        .iter()
        .map(|field| {
            vec![
                field.name.clone(),
                field.ty.display_name(),
                if field.optional {
                    "yes".into()
                } else {
                    "no".into()
                },
            ]
        })
        .collect::<Vec<_>>();
    let mut output = String::new();
    let _ = writeln!(output, "{}", theme.paint(SemanticRole::Secondary, "Schema"));
    let _ = writeln!(
        output,
        "{}",
        render_text_grid(&["field", "type", "optional"], &rows, theme, options)
    );
    let _ = write!(
        output,
        "{}",
        theme.paint(
            SemanticRole::Secondary,
            &format!(
                "{} field{}",
                schema.fields.len(),
                if schema.fields.len() == 1 { "" } else { "s" }
            )
        )
    );
    output
}

fn render_inspect(value: &Value, theme: &Theme, options: &RenderOptions) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "{}",
        theme.paint(SemanticRole::Secondary, "Inspect")
    );
    let _ = writeln!(output, "type: {}", value.type_name());
    match value {
        Value::Table(table) => {
            let _ = writeln!(output, "rows: {}", table.len());
            let _ = writeln!(output, "columns: {}", table.schema().fields.len());
            let _ = writeln!(output, "schema:");
            for field in &table.schema().fields {
                let _ = writeln!(
                    output,
                    "  {}: {}{}",
                    field.name,
                    field.ty.display_name(),
                    if field.optional { "?" } else { "" }
                );
            }
            let _ = writeln!(output, "value:");
            output.push_str(&render_table(table, theme, options));
        }
        Value::List(values) => {
            let _ = writeln!(output, "length: {}", values.len());
            if values.iter().all(|value| matches!(value, Value::Object(_))) {
                if let Ok(schema) = Schema::infer_records(values) {
                    let _ = writeln!(output, "schema:");
                    for field in &schema.fields {
                        let _ = writeln!(
                            output,
                            "  {}: {}{}",
                            field.name,
                            field.ty.display_name(),
                            if field.optional { "?" } else { "" }
                        );
                    }
                }
            }
            let _ = write!(output, "value: {}", plain_text(value));
        }
        other => {
            let _ = write!(output, "value: {}", plain_text(other));
        }
    }
    output
}

/// One-line text for a value, used where no layout is available (inspect,
/// nested summaries, error fallbacks).
pub(crate) fn plain_text(value: &Value) -> String {
    match value {
        Value::Void => "void".into(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Bytes(value) => format!("bytes[{}]", value.len()),
        Value::List(values) => format!(
            "[{}]",
            values.iter().map(plain_text).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(fields) => {
            let mut fields = fields.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(name, _)| *name);
            format!(
                "{{{}}}",
                fields
                    .into_iter()
                    .map(|(name, value)| format!("{name}: {}", plain_text(value)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Map(entries) => format!(
            "Map{{{}}}",
            entries
                .iter()
                .map(|(key, value)| format!("{}: {}", plain_text(key), plain_text(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Option(Some(value)) => format!("Some({})", plain_text(value)),
        Value::Option(None) => "None".into(),
        Value::Result(Ok(value)) => format!("Ok({})", plain_text(value)),
        Value::Result(Err(value)) => format!("Err({})", plain_text(value)),
        Value::Table(table) => format!("Table<{} rows>", table.len()),
        Value::Schema(schema) => format!(
            "Schema{{{}}}",
            schema
                .fields
                .iter()
                .map(|field| format!(
                    "{}: {}{}",
                    field.name,
                    field.ty.display_name(),
                    if field.optional { "?" } else { "" }
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Error {
            message,
            kind,
            code,
            ..
        } => format!("error[{kind}:{code}] {message}"),
        Value::Shell(_) => "<shell plan>".into(),
        Value::MixedShell(_) => "<mixed shell plan>".into(),
        Value::ShellProgram(_) => "<shell program>".into(),
        Value::Promise(_) => "<promise>".into(),
        Value::Resource(_) => "<resource>".into(),
        Value::Closure(_) | Value::Function(_) => "<function>".into(),
    }
}
