//! Native structured `ls`: a directory listing as a table value.
//!
//! Interactive `ls` returns one row per entry, hidden files included, with
//! `name`, `type`, `size` (bytes) and `modified` (UTC, ISO 8601) columns. The
//! terminal renders that table with type colors, human sizes and relative
//! times; the value itself stays plain data, so `_` can be filtered and encoded.

use std::fs::{self, FileType, Metadata};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use spar::{TableValue, Value};

const COLUMN_ORDER: [&str; 8] = [
    "name", "type", "size", "modified", "mode", "user", "group", "target",
];

/// Parsed `ls [-a] [-l] [paths...]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListRequest {
    pub long: bool,
    pub paths: Vec<String>,
}

/// Recognizes a plain `ls` invocation. Anything else — pipes, redirects, globs,
/// quoting, unknown flags — returns `None` so the external `ls` handles it.
pub fn parse_request(input: &str) -> Option<ListRequest> {
    let mut words = input.split_whitespace();
    if words.next()? != "ls" {
        return None;
    }
    let mut request = ListRequest::default();
    let mut options_done = false;
    for word in words {
        if word.chars().any(|c| "|&;<>()$`*?[]{}\"'\\~!#".contains(c)) {
            return None;
        }
        if !options_done && word == "--" {
            options_done = true;
        } else if !options_done && word.starts_with("--") {
            match word {
                "--all" | "--almost-all" => {}
                "--long" => request.long = true,
                _ => return None,
            }
        } else if !options_done && word.starts_with('-') && word.len() > 1 {
            for flag in word[1..].chars() {
                match flag {
                    // Hidden entries are always listed.
                    'a' | 'A' => {}
                    'l' => request.long = true,
                    _ => return None,
                }
            }
        } else {
            request.paths.push(word.to_string());
        }
    }
    Some(request)
}

/// `ls [args] |> stages`: the listing followed by a value pipeline.
pub fn parse_value_pipeline(input: &str) -> Option<(ListRequest, &str)> {
    let (head, tail) = input.split_once("|>")?;
    let tail = tail.trim();
    if tail.is_empty() {
        return None;
    }
    Some((parse_request(head)?, tail))
}

/// `to yaml`, `to json`, ...: the format named by a lone `to FORMAT` stage.
pub fn encode_stage(stage: &str) -> Option<&str> {
    let mut words = stage.split_whitespace();
    let format = (words.next()? == "to").then(|| words.next()).flatten()?;
    words.next().is_none().then_some(format)
}

/// Lists the requested paths relative to `cwd`.
pub fn list(request: &ListRequest, cwd: &Path) -> Result<Value, String> {
    let targets = if request.paths.is_empty() {
        vec![".".to_string()]
    } else {
        request.paths.clone()
    };
    let mut entries = Vec::new();
    for target in &targets {
        let path = cwd.join(target);
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("ls: cannot access '{target}': {}", reason(&error)))?;
        if metadata.is_dir() {
            let read = fs::read_dir(&path)
                .map_err(|error| format!("ls: cannot open '{target}': {}", reason(&error)))?;
            for entry in read.flatten() {
                if let Ok(metadata) = fs::symlink_metadata(entry.path()) {
                    entries.push(Entry::new(
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.path(),
                        metadata,
                    ));
                }
            }
        } else if let Ok(metadata) = fs::symlink_metadata(&path) {
            entries.push(Entry::new(target.clone(), path, metadata));
        }
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    let rows = entries
        .into_iter()
        .map(|entry| entry.into_row(request.long))
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(Value::Table(TableValue::with_schema(
            Vec::new(),
            spar::Schema::default(),
        )));
    }
    let mut schema = spar::Schema::infer_records(&rows)
        .map_err(|error| format!("ls: cannot build table: {error:?}"))?;
    // Inference sorts columns by name; a listing reads name, type, size, modified.
    let rank = |name: &str| {
        COLUMN_ORDER
            .iter()
            .position(|column| *column == name)
            .unwrap_or(COLUMN_ORDER.len())
    };
    schema.fields.sort_by_key(|field| rank(&field.name));
    Ok(Value::Table(TableValue::with_schema(rows, schema)))
}

fn reason(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".into(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".into(),
        _ => error.to_string(),
    }
}

struct Entry {
    name: String,
    path: PathBuf,
    metadata: Metadata,
    is_dir: bool,
}

impl Entry {
    fn new(name: String, path: PathBuf, metadata: Metadata) -> Self {
        // A symlink to a directory sorts with directories but is still a link.
        let is_dir = metadata.is_dir()
            || (metadata.file_type().is_symlink()
                && fs::metadata(&path).is_ok_and(|target| target.is_dir()));
        Self {
            name,
            path,
            metadata,
            is_dir,
        }
    }

    fn into_row(self, long: bool) -> Value {
        let kind = kind_of(&self.metadata);
        // A directory's own metadata is its inode size (a few KB, however
        // much it holds), not what is stored under it. Nushell and `ls -la`
        // both show that inode size; users expect `du`'s total instead, so
        // that's what this reports. Symlinked directories are not descended
        // into, matching `du`'s default and avoiding cycles through them.
        let size = if kind == "dir" {
            directory_size(&self.path)
        } else {
            self.metadata.len()
        };
        let modified = self
            .metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| format_iso8601(elapsed.as_secs() as i64));
        let mut fields = IndexMap::new();
        fields.insert("name".to_string(), Value::String(self.name));
        fields.insert("type".to_string(), Value::String(kind.to_string()));
        fields.insert(
            "size".to_string(),
            Value::Int(i64::try_from(size).unwrap_or(i64::MAX)),
        );
        fields.insert(
            "modified".to_string(),
            modified.map_or(Value::Void, Value::String),
        );
        if long {
            fields.insert(
                "mode".to_string(),
                Value::String(mode_string(&self.metadata)),
            );
            let (user, group) = owner_ids(&self.metadata);
            fields.insert("user".to_string(), Value::String(user));
            fields.insert("group".to_string(), Value::String(group));
            fields.insert(
                "target".to_string(),
                if self.metadata.file_type().is_symlink() {
                    fs::read_link(&self.path).map_or(Value::Void, |target| {
                        Value::String(target.to_string_lossy().into_owned())
                    })
                } else {
                    Value::Void
                },
            );
        }
        Value::Object(fields)
    }
}

/// Total size of everything under `root`, like `du -sb`: apparent file sizes
/// (`metadata.len()`), not allocated blocks, summed depth-first. A directory
/// entry that cannot be read (permissions, or removed mid-walk) contributes
/// nothing rather than failing the whole listing. Symlinks are never
/// followed, so shared caches and loops through a parent are never counted or
/// walked twice.
fn directory_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

/// `dir`, `file`, `exe` (executable regular file), `symlink`, `fifo`,
/// `socket`, `block` or `char`.
fn kind_of(metadata: &Metadata) -> &'static str {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return "symlink";
    }
    if file_type.is_dir() {
        return "dir";
    }
    if let Some(special) = special_kind(&file_type) {
        return special;
    }
    if is_executable(metadata) {
        "exe"
    } else {
        "file"
    }
}

#[cfg(unix)]
fn special_kind(file_type: &FileType) -> Option<&'static str> {
    use std::os::unix::fs::FileTypeExt;
    if file_type.is_fifo() {
        Some("fifo")
    } else if file_type.is_socket() {
        Some("socket")
    } else if file_type.is_block_device() {
        Some("block")
    } else if file_type.is_char_device() {
        Some("char")
    } else {
        None
    }
}

#[cfg(not(unix))]
fn special_kind(_file_type: &FileType) -> Option<&'static str> {
    None
}

#[cfg(unix)]
fn is_executable(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &Metadata) -> bool {
    false
}

#[cfg(unix)]
fn mode_string(metadata: &Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode();
    let mut out = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        out.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        out.push(if bits & 0o1 != 0 { 'x' } else { '-' });
    }
    out
}

#[cfg(not(unix))]
fn mode_string(_metadata: &Metadata) -> String {
    String::new()
}

#[cfg(unix)]
fn owner_ids(metadata: &Metadata) -> (String, String) {
    use std::os::unix::fs::MetadataExt;
    (
        name_for(metadata.uid(), "/etc/passwd"),
        name_for(metadata.gid(), "/etc/group"),
    )
}

#[cfg(not(unix))]
fn owner_ids(_metadata: &Metadata) -> (String, String) {
    (String::new(), String::new())
}

/// Resolves a numeric id through a `name:x:id:` database, falling back to the
/// number itself.
#[cfg(unix)]
fn name_for(id: u32, database: &str) -> String {
    fs::read_to_string(database)
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let mut parts = line.split(':');
                let name = parts.next()?;
                let found = parts.nth(1)?;
                (found == id.to_string()).then(|| name.to_string())
            })
        })
        .unwrap_or_else(|| id.to_string())
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ` for seconds since the Unix epoch.
pub fn format_iso8601(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Seconds since the Unix epoch for a `YYYY-MM-DDTHH:MM:SSZ` string.
pub fn parse_iso8601(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20 || bytes[10] != b'T' || bytes[19] != b'Z' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// "just now", "5 minutes ago", "3 days ago", "2 years ago".
pub fn relative_age(then: i64, now: i64) -> String {
    let elapsed = now - then;
    if elapsed < 0 {
        return "in the future".into();
    }
    let units: [(i64, &str); 6] = [
        (365 * 86_400, "year"),
        (30 * 86_400, "month"),
        (7 * 86_400, "week"),
        (86_400, "day"),
        (3600, "hour"),
        (60, "minute"),
    ];
    for (size, name) in units {
        if elapsed >= size {
            let count = elapsed / size;
            return format!("{count} {name}{} ago", if count == 1 { "" } else { "s" });
        }
    }
    if elapsed < 10 {
        "just now".into()
    } else {
        format!("{elapsed} seconds ago")
    }
}

/// Howard Hinnant's days-to-civil-date algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Current time, for callers that render relative ages.
pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(row: &Value, key: &str) -> String {
        match row {
            Value::Object(fields) => match &fields[key] {
                Value::String(value) => value.clone(),
                other => panic!("{key} is not a string: {other:?}"),
            },
            other => panic!("row is not a record: {other:?}"),
        }
    }

    fn rows(value: Value) -> Vec<Value> {
        match value {
            Value::Table(table) => table.into_rows(),
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn parses_plain_invocations_only() {
        assert_eq!(parse_request("ls"), Some(ListRequest::default()));
        assert_eq!(
            parse_request("ls -la src docs"),
            Some(ListRequest {
                long: true,
                paths: vec!["src".into(), "docs".into()]
            })
        );
        assert_eq!(parse_request("ls -a").unwrap().long, false);
        assert_eq!(parse_request("ls | wc -l"), None);
        assert_eq!(parse_request("ls *.rs"), None);
        assert_eq!(parse_request("ls --color=auto"), None);
        assert_eq!(parse_request("ls -R"), None);
        assert_eq!(parse_request("lsblk"), None);
        assert_eq!(parse_request("ll"), None);
    }

    #[test]
    fn value_pipelines_split_the_listing_from_its_stages() {
        let (request, tail) = parse_value_pipeline("ls -l src |> to yaml").unwrap();
        assert!(request.long);
        assert_eq!(request.paths, ["src"]);
        assert_eq!(tail, "to yaml");
        assert_eq!(encode_stage(tail), Some("yaml"));
        assert_eq!(encode_stage("take(2)"), None);
        assert_eq!(encode_stage("to yaml extra"), None);
        assert!(parse_value_pipeline("ls |>").is_none());
        assert!(parse_value_pipeline("cat x |> take(1)").is_none());
    }

    #[test]
    fn iso8601_round_trips_and_ages_read_naturally() {
        for seconds in [0, 951_782_400, 1_790_016_745] {
            assert_eq!(parse_iso8601(&format_iso8601(seconds)), Some(seconds));
        }
        assert_eq!(parse_iso8601("yesterday"), None);
        assert_eq!(relative_age(100, 103), "just now");
        assert_eq!(relative_age(0, 59), "59 seconds ago");
        assert_eq!(relative_age(0, 60), "1 minute ago");
        assert_eq!(relative_age(0, 5 * 86_400 + 5), "5 days ago");
        assert_eq!(relative_age(0, 14 * 86_400), "2 weeks ago");
        assert_eq!(relative_age(0, 800 * 86_400), "2 years ago");
        assert_eq!(relative_age(10, 0), "in the future");
    }

    #[test]
    fn iso8601_matches_known_instants() {
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_iso8601(1_790_016_745), "2026-09-21T18:52:25Z");
    }

    #[test]
    fn columns_read_name_type_size_modified() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), "a").unwrap();
        let Value::Table(table) = list(&ListRequest::default(), dir.path()).unwrap() else {
            panic!("table expected")
        };
        let names = table
            .schema()
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["name", "type", "size", "modified"]);
    }

    #[test]
    fn directory_size_is_the_total_of_everything_under_it() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), "12345").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b"), "1234567").unwrap();
        fs::create_dir(dir.path().join("sub/deeper")).unwrap();
        fs::write(dir.path().join("sub/deeper/c"), "12").unwrap();
        assert_eq!(directory_size(dir.path()), 5 + 7 + 2);

        let Value::Table(table) = list(&ListRequest::default(), dir.path()).unwrap() else {
            panic!("table expected")
        };
        let row = table
            .rows()
            .iter()
            .find(|row| text(row, "name") == "sub")
            .unwrap();
        let Value::Object(fields) = row else {
            panic!("record expected")
        };
        assert_eq!(fields["size"], Value::Int(9));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_reports_its_own_link_size_not_its_targets() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        fs::write(dir.path().join("real/big"), "0123456789").unwrap();
        symlink("real", dir.path().join("link")).unwrap();

        let Value::Table(table) = list(&ListRequest::default(), dir.path()).unwrap() else {
            panic!("table expected")
        };
        let link = table
            .rows()
            .iter()
            .find(|row| text(row, "name") == "link")
            .unwrap();
        assert_eq!(text(link, "type"), "symlink");
        let Value::Object(fields) = link else {
            panic!("record expected")
        };
        // A symlink's own size (the length of the path it stores), not 10.
        assert!(matches!(fields["size"], Value::Int(n) if n < 10));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cycle_inside_a_directory_does_not_hang_or_double_count() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real"), "12345").unwrap();
        symlink(dir.path(), dir.path().join("self")).unwrap();
        assert_eq!(directory_size(dir.path()), 5);
    }

    #[cfg(unix)]
    #[test]
    fn lists_hidden_entries_and_tells_kinds_apart() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("assets")).unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();
        fs::write(root.join(".hidden"), "x").unwrap();
        fs::write(root.join("run.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("README.md", root.join("link")).unwrap();

        let listing = rows(list(&ListRequest::default(), root).unwrap());
        let kinds = listing
            .iter()
            .map(|row| (text(row, "name"), text(row, "type")))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                ("assets".to_string(), "dir".to_string()),
                (".hidden".to_string(), "file".to_string()),
                ("link".to_string(), "symlink".to_string()),
                ("README.md".to_string(), "file".to_string()),
                ("run.sh".to_string(), "exe".to_string()),
            ]
        );
        let Value::Object(readme) = &listing[3] else {
            panic!("record expected")
        };
        assert_eq!(readme["size"], Value::Int(5));
    }

    #[cfg(unix)]
    #[test]
    fn long_listing_adds_mode_owner_and_link_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        symlink("a.txt", dir.path().join("b")).unwrap();
        let request = ListRequest {
            long: true,
            paths: Vec::new(),
        };
        let listing = rows(list(&request, dir.path()).unwrap());
        assert_eq!(text(&listing[1], "target"), "a.txt");
        assert_eq!(text(&listing[0], "mode").len(), 9);
        assert!(!text(&listing[0], "user").is_empty());
    }

    #[test]
    fn missing_path_reports_an_ls_style_error() {
        let dir = tempfile::tempdir().unwrap();
        let request = ListRequest {
            long: false,
            paths: vec!["nope".into()],
        };
        assert_eq!(
            list(&request, dir.path()).unwrap_err(),
            "ls: cannot access 'nope': No such file or directory"
        );
    }

    #[test]
    fn empty_directory_is_an_empty_table() {
        let dir = tempfile::tempdir().unwrap();
        assert!(rows(list(&ListRequest::default(), dir.path()).unwrap()).is_empty());
    }
}
