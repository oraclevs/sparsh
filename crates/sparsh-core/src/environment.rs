use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

#[derive(Debug, Clone)]
pub(crate) struct EnvironmentService {
    entries: BTreeMap<OsString, OsString>,
}

impl EnvironmentService {
    pub(crate) fn from_current() -> Self {
        Self {
            entries: std::env::vars_os().collect(),
        }
    }

    pub(crate) fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Self {
            entries: pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&OsStr> {
        self.entries.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    pub(crate) fn set_many(&mut self, entries: &[(String, String)]) -> Result<(), String> {
        validate_names(entries.iter().map(|(name, _)| name.as_str()))?;
        for (name, value) in entries {
            self.entries.insert(name.into(), value.into());
        }
        Ok(())
    }

    pub(crate) fn set_os(&mut self, name: &str, value: impl Into<OsString>) {
        self.entries.insert(name.into(), value.into());
    }

    pub(crate) fn ensure_many(&mut self, names: &[String]) -> Result<(), String> {
        validate_names(names.iter().map(String::as_str))?;
        for name in names {
            self.entries.entry(name.into()).or_default();
        }
        Ok(())
    }

    pub(crate) fn unset_many(&mut self, names: &[String]) -> Result<(), String> {
        validate_names(names.iter().map(String::as_str))?;
        for name in names {
            self.entries.remove(OsStr::new(name));
        }
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> Vec<(OsString, OsString)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    pub(crate) fn render(&self) -> String {
        let mut output = String::new();
        for (key, value) in &self.entries {
            output.push_str(&escape_os(key));
            output.push_str("='");
            output.push_str(&escape_os(value));
            output.push_str("'\n");
        }
        output
    }
}

fn validate_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    for name in names {
        if !valid_name(name) {
            return Err(format!("invalid environment name: `{name}`"));
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[cfg(unix)]
fn escape_os(value: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut escaped = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'\\' => escaped.push_str("\\\\"),
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            b'\'' => escaped.push_str("\\'"),
            0x20..=0x7e => escaped.push(*byte as char),
            _ => escaped.push_str(&format!("\\x{byte:02x}")),
        }
    }
    escaped
}

#[cfg(not(unix))]
fn escape_os(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::EnvironmentService;

    #[test]
    fn set_many_is_atomic_when_any_name_is_invalid() {
        let mut environment = EnvironmentService::from_pairs([("KEEP", "old")]);

        let error = environment
            .set_many(&[
                ("KEEP".to_string(), "new".to_string()),
                ("BAD-NAME".to_string(), "value".to_string()),
            ])
            .unwrap_err();

        assert!(error.contains("BAD-NAME"));
        assert_eq!(environment.get("KEEP"), Some(OsStr::new("old")));
    }

    #[test]
    fn unset_removes_name_from_child_snapshot() {
        let mut environment = EnvironmentService::from_pairs([("REMOVE", "yes")]);

        environment.unset_many(&["REMOVE".to_string()]).unwrap();

        assert!(!environment
            .snapshot()
            .iter()
            .any(|(key, _)| key == "REMOVE"));
    }

    #[test]
    fn unset_validation_is_atomic() {
        let mut environment = EnvironmentService::from_pairs([("FIRST", "1"), ("SECOND", "2")]);

        let error = environment
            .unset_many(&["FIRST".to_string(), "NOT-VALID".to_string()])
            .unwrap_err();

        assert!(error.contains("NOT-VALID"));
        assert_eq!(environment.get("FIRST"), Some(OsStr::new("1")));
    }

    #[test]
    fn listing_is_stable_and_escaped() {
        let environment = EnvironmentService::from_pairs([
            ("ZED", "last"),
            ("ALPHA", "line\nbreak"),
            ("QUOTE", "a'b"),
        ]);

        assert_eq!(
            environment.render(),
            "ALPHA='line\\nbreak'\nQUOTE='a\\'b'\nZED='last'\n"
        );
    }
}
