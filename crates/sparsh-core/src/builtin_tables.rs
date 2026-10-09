//! Interactive builtins that list things show a table, like `ls`: the same
//! data the plain-text builtin prints, as rows a terminal can draw and `_`
//! can filter. Timestamps are stored ISO 8601 and drawn human-readable by the UI.

use indexmap::IndexMap;
use spar::{TableValue, Value};

pub(crate) type Row = Vec<(&'static str, Value)>;

pub(crate) fn text(value: impl Into<String>) -> Value {
    Value::String(value.into())
}

pub(crate) fn optional_text(value: &str) -> Value {
    if value.is_empty() { Value::Void } else { Value::String(value.to_string()) }
}

pub(crate) fn number(value: usize) -> Value {
    Value::Int(i64::try_from(value).unwrap_or(i64::MAX))
}

/// ISO 8601 for a stored unix time; `0` means unknown.
pub(crate) fn when(seconds: u64) -> Value {
    if seconds == 0 { Value::Void } else { Value::String(crate::listing::format_iso8601(seconds as i64)) }
}

/// A table whose columns keep the order the rows were written in.
pub(crate) fn table(rows: Vec<Row>, columns: &[&str]) -> Result<Value, String> {
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let mut fields = IndexMap::new();
            for (name, value) in row {
                fields.insert(name.to_string(), value);
            }
            Value::Object(fields.into())
        })
        .collect();
    if rows.is_empty() {
        return Ok(Value::Table(TableValue::with_schema(Vec::new(), spar::Schema::default()).into()));
    }
    let mut schema = spar::Schema::infer_records(&rows)
        .map_err(|error| format!("cannot build table: {error:?}"))?;
    let rank = |name: &str| columns.iter().position(|column| *column == name).unwrap_or(columns.len());
    schema.fields.sort_by_key(|field| rank(&field.name));
    Ok(Value::Table(TableValue::with_schema(rows, schema).into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_follow_the_requested_order_and_empty_tables_are_valid() {
        let rows = vec![vec![("b", text("x")), ("a", number(1))]];
        let Value::Table(table_value) = table(rows, &["a", "b"]).unwrap() else { panic!() };
        let names: Vec<_> = table_value.schema().fields.iter().map(|f| f.name.clone()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(table(Vec::new(), &["a"]).is_ok());
    }
}
