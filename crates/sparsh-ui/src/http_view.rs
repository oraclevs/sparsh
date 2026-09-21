//! How the prompt shows an `HttpResponse` from `std/http`: a status line, then
//! the body decoded as JSON (a table or tree) when it is JSON, otherwise as text.

use indexmap::IndexMap;

use spar::Value;

use crate::data_view::{render_value_view, RenderOptions};
use crate::styled::sanitize;
use crate::{SemanticRole, Theme};

/// A record with exactly the fields of `std/http`'s `HttpResponse`.
pub(crate) fn is_http_response(fields: &IndexMap<String, Value>) -> bool {
    fields.len() == 3
        && matches!(fields.get("status"), Some(Value::Int(_)))
        && matches!(fields.get("body"), Some(Value::String(_)))
        && matches!(fields.get("contentType"), Some(Value::String(_)))
}

fn reason(status: i64) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

fn status_role(status: i64) -> SemanticRole {
    match status {
        200..=299 => SemanticRole::Success,
        300..=399 => SemanticRole::Warning,
        _ => SemanticRole::Failure,
    }
}

/// The body as a JSON value, when the content type says JSON or the text
/// looks like a JSON document that parses.
fn decode_json(body: &str, content_type: &str) -> Option<Value> {
    let trimmed = body.trim_start();
    let looks_json = content_type.to_ascii_lowercase().contains("json")
        || trimmed.starts_with('{')
        || trimmed.starts_with('[');
    if !looks_json {
        return None;
    }
    let mut values = spar::StructuredFormatRegistry::builtin()
        .decode_bytes("json", body.as_bytes())
        .ok()?;
    (values.len() == 1).then(|| values.remove(0))
}

pub(crate) fn render_http_response(
    fields: &IndexMap<String, Value>,
    theme: &Theme,
    options: &RenderOptions,
) -> String {
    let (Some(Value::Int(status)), Some(Value::String(body)), Some(Value::String(content_type))) = (
        fields.get("status"),
        fields.get("body"),
        fields.get("contentType"),
    ) else {
        return String::new();
    };

    let mut heading = theme.paint(
        status_role(*status),
        format!("{status} {}", reason(*status)).trim_end(),
    );
    if !content_type.is_empty() {
        heading.push_str(&theme.paint(
            SemanticRole::Secondary,
            &format!("  ·  {}", sanitize(content_type)),
        ));
    }

    if body.trim().is_empty() {
        return format!(
            "{heading}\n{}",
            theme.paint(SemanticRole::Secondary, "(empty body)")
        );
    }

    if let Some(json) = decode_json(body, content_type) {
        return format!(
            "{heading}\n{}\n{}",
            render_value_view(&json, theme, options),
            theme.paint(
                SemanticRole::Secondary,
                "body decoded from JSON, raw text in `_.body`"
            )
        );
    }

    let lines = body.lines().collect::<Vec<_>>();
    let shown = lines.len().min(options.max_lines);
    let mut text = lines[..shown]
        .iter()
        .map(|line| sanitize(line))
        .collect::<Vec<_>>()
        .join("\n");
    if shown < lines.len() {
        text.push('\n');
        text.push_str(&theme.paint(
            SemanticRole::Secondary,
            &format!(
                "… {} more lines ({} total), full body in `_.body`",
                lines.len() - shown,
                lines.len()
            ),
        ));
    }
    format!("{heading}\n{text}")
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use spar::Value;

    use super::{is_http_response, render_http_response};
    use crate::data_view::RenderOptions;
    use crate::Theme;

    fn response(status: i64, content_type: &str, body: &str) -> IndexMap<String, Value> {
        IndexMap::from([
            ("status".into(), Value::Int(status)),
            ("contentType".into(), Value::String(content_type.into())),
            ("body".into(), Value::String(body.into())),
        ])
    }

    #[test]
    fn only_the_exact_http_response_shape_is_recognized() {
        assert!(is_http_response(&response(200, "", "")));
        let mut extra = response(200, "", "");
        extra.insert("headers".into(), Value::Int(1));
        assert!(!is_http_response(&extra));
        assert!(!is_http_response(&IndexMap::from([(
            "status".into(),
            Value::Int(1)
        )])));
    }

    #[test]
    fn json_bodies_render_as_a_record_under_a_status_line() {
        let fields = response(
            200,
            "application/json; charset=utf-8",
            r#"{"name":"ditto","id":132,"types":[{"slot":1}]}"#,
        );
        let output = render_http_response(&fields, &Theme::plain(), &RenderOptions::new(80));

        assert!(
            output.starts_with("200 OK  ·  application/json; charset=utf-8\n"),
            "{output}"
        );
        assert!(output.contains("│ name"), "{output}");
        assert!(output.contains("ditto"), "{output}");
        assert!(output.contains("raw text in `_.body`"), "{output}");
    }

    #[test]
    fn json_arrays_render_as_a_table() {
        let fields = response(200, "application/json", r#"[{"a":1},{"a":2}]"#);
        let output = render_http_response(&fields, &Theme::plain(), &RenderOptions::new(80));
        assert!(output.contains("│ # │ a │"), "{output}");
    }

    #[test]
    fn html_and_text_bodies_are_shown_as_text_and_bounded() {
        let body = (0..10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut options = RenderOptions::new(80);
        options.max_lines = 3;
        let output = render_http_response(
            &response(200, "text/html", &body),
            &Theme::plain(),
            &options,
        );

        assert!(output.contains("line 2"), "{output}");
        assert!(!output.contains("line 3"), "{output}");
        assert!(
            output.contains("… 7 more lines (10 total), full body in `_.body`"),
            "{output}"
        );
    }

    #[test]
    fn errors_and_empty_bodies_are_labelled() {
        let output = render_http_response(
            &response(404, "", ""),
            &Theme::plain(),
            &RenderOptions::new(80),
        );
        assert_eq!(output, "404 Not Found\n(empty body)");

        let colored = render_http_response(
            &response(500, "", "oops"),
            &Theme::colored(),
            &RenderOptions::new(80),
        );
        assert!(colored.contains("\x1b["), "{colored:?}");
    }

    #[test]
    fn a_json_content_type_with_a_broken_body_falls_back_to_text() {
        let output = render_http_response(
            &response(200, "application/json", "{not json"),
            &Theme::plain(),
            &RenderOptions::new(80),
        );
        assert!(output.ends_with("{not json"), "{output}");
    }
}
