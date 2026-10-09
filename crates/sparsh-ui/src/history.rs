use reedline::{
    FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId,
    Result as ReedlineResult, SearchDirection, SearchQuery,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// What a submitted line is, so later features can sort history by intent.
fn command_kind(command: &str) -> &'static str {
    let first = command.split_whitespace().next().unwrap_or("");
    match first {
        "cd" | "z" | "pushd" | "popd" => "dir",
        "fn" | "function" | "var" | "const" | "struct" | "enum" | "type" | "import" | "if"
        | "for" | "while" | "loop" | "return" => "spar",
        _ => "command",
    }
}

#[derive(Clone, Debug)]
struct CommandRecord {
    ts: u64,
    command: String,
    cwd: String,
    kind: String,
}

impl CommandRecord {
    fn to_line(&self) -> String {
        json!({"t": "cmd", "ts": self.ts, "cmd": self.command, "cwd": self.cwd, "kind": self.kind})
            .to_string()
    }
}

/// The structured history file: JSON Lines, one record per line.
/// `{"t":"cmd","ts":..,"cmd":..,"cwd":..,"kind":..}` is a submitted command and
/// `{"t":"dir","ts":..,"path":..,"n":..}` is a directory the shell entered.
/// A line that is not JSON is a command from the old plain-text format.
struct Store {
    path: PathBuf,
    capacity: usize,
    commands: VecDeque<CommandRecord>,
    /// path -> (visit count, last visit)
    directories: BTreeMap<PathBuf, (u64, u64)>,
    /// `z --set` short names
    aliases: BTreeMap<String, PathBuf>,
    cwd: String,
}

impl Store {
    fn load(path: PathBuf, capacity: usize) -> std::io::Result<Self> {
        let mut store = Self {
            path,
            capacity,
            commands: VecDeque::new(),
            directories: BTreeMap::new(),
            aliases: BTreeMap::new(),
            cwd: String::new(),
        };
        let file = match OpenOptions::new().read(true).open(&store.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(error) => return Err(error),
        };
        let mut legacy = false;
        let mut lines = 0usize;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            lines += 1;
            match serde_json::from_str::<Value>(&line) {
                Ok(value) if value.is_object() => store.apply(&value),
                _ => {
                    legacy = true;
                    let command = line.replace("<\\n>", "\n");
                    store.push_command(CommandRecord {
                        ts: 0,
                        kind: command_kind(&command).into(),
                        command,
                        cwd: String::new(),
                    });
                }
            }
        }
        // Convert an old plain file once, and keep a long-lived file small.
        if legacy || lines > store.capacity.saturating_mul(4).max(2000) {
            store.rewrite()?;
        }
        Ok(store)
    }

    fn apply(&mut self, value: &Value) {
        let ts = value["ts"].as_u64().unwrap_or(0);
        match value["t"].as_str() {
            Some("cmd") => {
                if let Some(command) = value["cmd"].as_str() {
                    self.push_command(CommandRecord {
                        ts,
                        command: command.to_string(),
                        cwd: value["cwd"].as_str().unwrap_or("").to_string(),
                        kind: value["kind"].as_str().unwrap_or("command").to_string(),
                    });
                }
            }
            Some("dir") => {
                if let Some(path) = value["path"].as_str() {
                    let entry = self.directories.entry(PathBuf::from(path)).or_insert((0, 0));
                    entry.0 += value["n"].as_u64().unwrap_or(1);
                    entry.1 = entry.1.max(ts);
                    self.cwd = path.to_string();
                }
            }
            Some("alias") => {
                if let (Some(name), Some(path)) = (value["name"].as_str(), value["path"].as_str()) {
                    self.aliases.insert(name.to_string(), PathBuf::from(path));
                }
            }
            Some("unalias") => {
                if let Some(name) = value["name"].as_str() {
                    self.aliases.remove(name);
                }
            }
            _ => {}
        }
    }

    fn push_command(&mut self, record: CommandRecord) {
        if self.capacity > 0 && self.commands.len() == self.capacity {
            self.commands.pop_front();
        }
        self.commands.push_back(record);
    }

    fn open_locked(&self, append: bool) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        if append {
            options.append(true).create(true);
        } else {
            options.read(true).write(true).create(true);
        }
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let file = options.open(&self.path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("history path is not a regular file"));
        }
        Ok(file)
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        let mut lock = fd_lock::RwLock::new(self.open_locked(true)?);
        let mut guard = lock.write()?;
        guard.write_all(format!("{line}\n").as_bytes())
    }

    fn rewrite(&self) -> std::io::Result<()> {
        let mut lock = fd_lock::RwLock::new(self.open_locked(false)?);
        let mut guard = lock.write()?;
        guard.seek(SeekFrom::Start(0))?;
        let mut text = String::new();
        for record in &self.commands {
            text.push_str(&record.to_line());
            text.push('\n');
        }
        for (path, (count, last)) in &self.directories {
            if let Some(path) = path.to_str() {
                text.push_str(&json!({"t": "dir", "ts": last, "path": path, "n": count}).to_string());
                text.push('\n');
            }
        }
        for (name, path) in &self.aliases {
            if let Some(path) = path.to_str() {
                text.push_str(&json!({"t": "alias", "name": name, "path": path}).to_string());
                text.push('\n');
            }
        }
        guard.write_all(text.as_bytes())?;
        let end = guard.stream_position()?;
        guard.set_len(end)?;
        guard.sync_all()
    }
}

/// Reedline history wrapper that suppresses immediately repeated commands
/// within one running Sparsh session when configured to do so. Reedline's own
/// in-memory list serves navigation and search; `Store` keeps it on disk.
pub(crate) struct SparshHistory {
    inner: FileBackedHistory,
    dedupe_consecutive: bool,
    last_saved: Option<HistoryItem>,
    stealth_mode: bool,
    store: Store,
}

impl SparshHistory {
    pub(crate) fn open(
        path: PathBuf,
        capacity: usize,
        dedupe_consecutive: bool,
    ) -> std::io::Result<Self> {
        let store = Store::load(path, capacity)?;
        let mut inner =
            FileBackedHistory::new(capacity).map_err(|error| std::io::Error::other(error.to_string()))?;
        for record in &store.commands {
            let _ = inner.save(HistoryItem::from_command_line(record.command.clone()));
        }
        Ok(Self {
            inner,
            dedupe_consecutive,
            last_saved: None,
            stealth_mode: false,
            store,
        })
    }

    fn remove_where(
        &mut self,
        mut remove: impl FnMut(usize, &str) -> bool,
    ) -> Result<usize, String> {
        let before = self.store.commands.len();
        let mut index = 0;
        self.store.commands.retain(|record| {
            index += 1;
            !remove(index, &record.command)
        });
        let removed = before - self.store.commands.len();
        if removed > 0 {
            // Clearing every command also forgets where they were run.
            if self.store.commands.is_empty() {
                self.store.directories.clear();
            }
            self.store.rewrite().map_err(|error| error.to_string())?;
            let mut inner = FileBackedHistory::new(self.store.capacity)
                .map_err(|error| error.to_string())?;
            for record in &self.store.commands {
                let _ = inner.save(HistoryItem::from_command_line(record.command.clone()));
            }
            self.inner = inner;
            self.last_saved = None;
        }
        Ok(removed)
    }

    fn record_dir(&mut self, path: &Path) -> Result<(), String> {
        if self.stealth_mode {
            return Ok(());
        }
        let Some(text) = path.to_str() else {
            return Ok(());
        };
        let now = unix_now();
        let entry = self.store.directories.entry(path.to_path_buf()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 = now;
        self.store.cwd = text.to_string();
        self.store
            .append(&json!({"t": "dir", "ts": now, "path": text, "n": 1}).to_string())
            .map_err(|error| error.to_string())
    }

    fn dir_aliases(&self) -> Vec<sparsh_core::DirAlias> {
        self.store.aliases.iter().map(|(name, path)| (name.clone(), path.clone())).collect()
    }

    fn set_dir_alias(&mut self, name: &str, path: &Path) -> Result<(), String> {
        let Some(text) = path.to_str() else {
            return Err("path is not valid UTF-8".into());
        };
        self.store.aliases.insert(name.to_string(), path.to_path_buf());
        self.store
            .append(&json!({"t": "alias", "name": name, "path": text}).to_string())
            .map_err(|error| error.to_string())
    }

    fn remove_dir_alias(&mut self, name: &str) -> Result<bool, String> {
        if self.store.aliases.remove(name).is_none() {
            return Ok(false);
        }
        self.store
            .append(&json!({"t": "unalias", "name": name}).to_string())
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    fn directories(&self) -> Vec<sparsh_core::DirVisit> {
        self.store
            .directories
            .iter()
            .map(|(path, (count, last))| sparsh_core::DirVisit {
                path: path.clone(),
                count: *count,
                last: *last,
            })
            .collect()
    }
}

#[derive(Clone)]
pub(crate) struct SharedHistory {
    inner: Arc<Mutex<SparshHistory>>,
}

impl SharedHistory {
    pub(crate) fn new(history: SparshHistory) -> Self {
        Self {
            inner: Arc::new(Mutex::new(history)),
        }
    }
}

impl sparsh_core::HistoryAccess for SharedHistory {
    fn stealth_mode(&self) -> Result<bool, String> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .stealth_mode)
    }

    fn set_stealth_mode(&self, enabled: bool) -> Result<(), String> {
        self.inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .stealth_mode = enabled;
        Ok(())
    }

    fn list(&self, limit: Option<usize>) -> Result<Vec<(usize, String)>, String> {
        let history = self.inner.lock().map_err(|_| "history lock poisoned")?;
        let mut items = history
            .search(SearchQuery::everything(SearchDirection::Forward, None))
            .map_err(|error| error.to_string())?;
        if let Some(limit) = limit {
            if items.len() > limit {
                items.drain(..items.len() - limit);
            }
        }
        Ok(items
            .into_iter()
            .filter_map(|item| item.id.map(|id| (id.0 as usize + 1, item.command_line)))
            .collect())
    }

    fn records(&self, limit: Option<usize>) -> Result<Vec<sparsh_core::HistoryRecord>, String> {
        let history = self.inner.lock().map_err(|_| "history lock poisoned")?;
        let mut records: Vec<_> = history
            .store
            .commands
            .iter()
            .enumerate()
            .map(|(index, record)| sparsh_core::HistoryRecord {
                line: index + 1,
                time: record.ts,
                command: record.command.clone(),
                directory: record.cwd.clone(),
                kind: record.kind.clone(),
            })
            .collect();
        if let Some(limit) = limit {
            if records.len() > limit {
                records.drain(..records.len() - limit);
            }
        }
        Ok(records)
    }

    fn delete_line(&self, line: usize) -> Result<usize, String> {
        self.inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .remove_where(|number, _| number == line)
    }

    fn delete_matching(&self, text: &str, exact: bool) -> Result<usize, String> {
        self.inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .remove_where(|_, command| {
                if exact {
                    command == text
                } else {
                    command.contains(text)
                }
            })
    }

    fn clear(&self) -> Result<(), String> {
        self.inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .remove_where(|_, _| true)
            .map(|_| ())
    }

    fn record_dir(&self, path: &std::path::Path) -> Result<(), String> {
        self.inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .record_dir(path)
    }

    fn dir_aliases(&self) -> Result<Vec<sparsh_core::DirAlias>, String> {
        Ok(self.inner.lock().map_err(|_| "history lock poisoned")?.dir_aliases())
    }

    fn set_dir_alias(&self, name: &str, path: &std::path::Path) -> Result<(), String> {
        self.inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .set_dir_alias(name, path)
    }

    fn remove_dir_alias(&self, name: &str) -> Result<bool, String> {
        self.inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .remove_dir_alias(name)
    }

    fn directories(&self) -> Result<Vec<sparsh_core::DirVisit>, String> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| "history lock poisoned")?
            .directories())
    }
}

impl History for SharedHistory {
    fn save(&mut self, item: HistoryItem) -> ReedlineResult<HistoryItem> {
        self.inner.lock().unwrap().save(item)
    }
    fn load(&self, id: HistoryItemId) -> ReedlineResult<HistoryItem> {
        self.inner.lock().unwrap().load(id)
    }
    fn count(&self, query: SearchQuery) -> ReedlineResult<i64> {
        self.inner.lock().unwrap().count(query)
    }
    fn search(&self, query: SearchQuery) -> ReedlineResult<Vec<HistoryItem>> {
        self.inner.lock().unwrap().search(query)
    }
    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> ReedlineResult<()> {
        self.inner.lock().unwrap().update(id, updater)
    }
    fn clear(&mut self) -> ReedlineResult<()> {
        self.inner
            .lock()
            .unwrap()
            .remove_where(|_, _| true)
            .map(|_| ())
            .map_err(|error| std::io::Error::other(error).into())
    }
    fn delete(&mut self, id: HistoryItemId) -> ReedlineResult<()> {
        self.inner
            .lock()
            .unwrap()
            .remove_where(|number, _| number == id.0 as usize + 1)
            .map(|_| ())
            .map_err(|error| std::io::Error::other(error).into())
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.inner.lock().unwrap().sync()
    }
    fn session(&self) -> Option<HistorySessionId> {
        self.inner.lock().unwrap().session()
    }
}

impl History for SparshHistory {
    fn save(&mut self, item: HistoryItem) -> ReedlineResult<HistoryItem> {
        // Reedline saves before returning the submitted line to the shell.
        // A paste is one history item even when the shell runs it line by line.
        let mode_command = item.command_line.lines().any(|line| {
            let command = line.trim().trim_end_matches(';').trim();
            command.split_whitespace().next() == Some("history")
                || matches!(
                    command,
                    "stealth" | "stealth on" | "stealth off" | "stealth status"
                )
        });
        if self.stealth_mode || mode_command {
            return Ok(item);
        }
        if self.dedupe_consecutive
            && item.id.is_none()
            && self
                .last_saved
                .as_ref()
                .is_some_and(|previous| previous.command_line == item.command_line)
        {
            return Ok(self.last_saved.as_ref().expect("checked above").clone());
        }

        let command = item.command_line.clone();
        let saved = self.inner.save(item)?;
        // Reedline returns no id for an empty or repeated line; those are not recorded.
        if saved.id.is_some() {
            let record = CommandRecord {
                ts: unix_now(),
                kind: command_kind(&command).to_string(),
                cwd: self.store.cwd.clone(),
                command,
            };
            let line = record.to_line();
            self.store.push_command(record);
            self.store.append(&line).map_err(reedline::ReedlineError::from)?;
        }
        self.last_saved = Some(saved.clone());
        Ok(saved)
    }

    fn load(&self, id: HistoryItemId) -> ReedlineResult<HistoryItem> {
        self.inner.load(id)
    }

    fn count(&self, query: SearchQuery) -> ReedlineResult<i64> {
        self.inner.count(query)
    }

    fn search(&self, query: SearchQuery) -> ReedlineResult<Vec<HistoryItem>> {
        self.inner.search(query)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> ReedlineResult<()> {
        self.last_saved = None;
        self.inner.update(id, updater)
    }

    fn clear(&mut self) -> ReedlineResult<()> {
        self.remove_where(|_, _| true)
            .map(|_| ())
            .map_err(|error| std::io::Error::other(error).into())
    }

    fn delete(&mut self, id: HistoryItemId) -> ReedlineResult<()> {
        self.remove_where(|number, _| number == id.0 as usize + 1)
            .map(|_| ())
            .map_err(|error| std::io::Error::other(error).into())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.inner.sync()
    }

    fn session(&self) -> Option<HistorySessionId> {
        self.inner.session()
    }
}

#[cfg(test)]
mod tests {
    use reedline::{History, HistoryItem};
    use tempfile::TempDir;

    use super::{SharedHistory, SparshHistory};
    use sparsh_core::HistoryAccess;

    #[test]
    fn stealth_excludes_every_private_line_and_both_mode_switches() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, false).unwrap();
        for command in [
            "echo public-before",
            "stealth on",
            "echo secret-token",
            "var password: str = \"secret-token\";",
            "stealth status",
            "stealth off",
            "echo public-after",
        ] {
            history
                .save(HistoryItem::from_command_line(command))
                .unwrap();
            if command == "stealth on" {
                history.stealth_mode = true;
            } else if command == "stealth off" {
                history.stealth_mode = false;
            }
        }
        history.sync().unwrap();
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(contents.contains("public-before"));
        assert!(contents.contains("public-after"));
        assert!(!contents.contains("secret-token"));
        assert!(!contents.contains("stealth"));
        assert_eq!(history.count_all().unwrap(), 2);
    }

    #[test]
    fn paste_entering_stealth_is_never_written_to_history() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, false).unwrap();
        history
            .save(HistoryItem::from_command_line(
                "stealth on\nvar secret: str = \"sensitive\";",
            ))
            .unwrap();
        history.stealth_mode = true;
        history
            .save(HistoryItem::from_command_line("echo also-sensitive"))
            .unwrap();
        history
            .save(HistoryItem::from_command_line("stealth off"))
            .unwrap();
        history.stealth_mode = false;
        history
            .save(HistoryItem::from_command_line("echo visible"))
            .unwrap();
        history.sync().unwrap();
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(contents.contains("echo visible"));
        assert!(!contents.contains("sensitive"));
        assert!(!contents.contains("stealth"));
        assert_eq!(history.count_all().unwrap(), 1);
    }

    #[test]
    fn quoted_stealth_off_cannot_disable_private_history() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, false).unwrap();
        history.stealth_mode = true;
        history
            .save(HistoryItem::from_command_line(
                "var message: str = \"hello\nstealth off\nworld\";",
            ))
            .unwrap();
        assert!(history.stealth_mode);
        history
            .save(HistoryItem::from_command_line("echo secret-after"))
            .unwrap();
        history.sync().unwrap();
        assert!(!std::fs::read_to_string(path)
            .unwrap_or_default()
            .contains("secret-after"));
    }

    #[test]
    fn targeted_deletion_persists_and_history_queries_are_not_recorded() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, false).unwrap();
        for command in [
            "echo public-one",
            "echo api-key-secret",
            "history --search api-key-secret",
            "echo public-two",
            "echo api-key-secret",
        ] {
            history
                .save(HistoryItem::from_command_line(command))
                .unwrap();
        }
        history.sync().unwrap();
        let shared = SharedHistory::new(history);
        assert_eq!(shared.list(Some(2)).unwrap()[0].0, 3);
        assert_eq!(shared.delete_line(2).unwrap(), 1);
        assert_eq!(shared.delete_matching("api-key-secret", false).unwrap(), 1);
        assert_eq!(shared.delete_matching("missing", false).unwrap(), 0);
        assert_eq!(shared.list(None).unwrap().len(), 2);
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("api-key-secret"));
        drop(shared);
        let reopened = SparshHistory::open(path.clone(), 100, false).unwrap();
        let commands: Vec<_> = reopened
            .search(reedline::SearchQuery::everything(
                reedline::SearchDirection::Forward,
                None,
            ))
            .unwrap()
            .into_iter()
            .map(|item| item.command_line)
            .collect();
        assert_eq!(commands, ["echo public-one", "echo public-two"]);
    }

    #[test]
    fn consecutive_duplicates_are_not_inserted_twice_when_enabled() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, true).unwrap();

        history
            .save(HistoryItem::from_command_line("echo one"))
            .unwrap();
        history
            .save(HistoryItem::from_command_line("echo one"))
            .unwrap();
        history.sync().unwrap();

        assert_eq!(history.count_all().unwrap(), 1);
    }

    #[test]
    fn multiline_submission_is_persisted_as_one_history_item() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, true).unwrap();
        let submission = "users |>\n    take(2) |>\n    inspect()";

        history
            .save(HistoryItem::from_command_line(submission))
            .unwrap();
        history.sync().unwrap();

        assert_eq!(history.count_all().unwrap(), 1);
        let items = history
            .search(reedline::SearchQuery::everything(
                reedline::SearchDirection::Forward,
                None,
            ))
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].command_line, submission);
    }

    #[test]
    fn duplicate_filter_can_be_disabled() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, false).unwrap();

        // reedline's file backend always drops an entry identical to the
        // previous one, so exercise the wrapper's own filter with repeats that
        // are not adjacent.
        for command in ["echo one", "echo two", "echo one"] {
            history
                .save(HistoryItem::from_command_line(command))
                .unwrap();
        }
        history.sync().unwrap();

        assert_eq!(history.count_all().unwrap(), 3);
    }

    #[test]
    fn history_is_json_lines_with_kind_and_directory_records() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, false).unwrap();
        history.record_dir(temp.path()).unwrap();
        history.save(HistoryItem::from_command_line("cd /tmp")).unwrap();
        history.save(HistoryItem::from_command_line("echo \"a\nb\"")).unwrap();
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
        let reopened = SparshHistory::open(path, 100, false).unwrap();
        assert_eq!(reopened.count_all().unwrap(), 2);
        assert_eq!(reopened.store.commands[0].kind, "dir");
        assert_eq!(reopened.store.commands[0].cwd, temp.path().to_str().unwrap());
        assert_eq!(reopened.store.commands[1].command, "echo \"a\nb\"");
        assert_eq!(reopened.directories()[0].count, 1);
    }

    #[test]
    fn plain_text_history_is_converted_once_and_kept() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        std::fs::write(&path, "ls\ncd src\necho a<\\n>b\n").unwrap();
        let history = SparshHistory::open(path.clone(), 100, false).unwrap();
        let commands: Vec<_> = history.store.commands.iter().map(|r| r.command.clone()).collect();
        assert_eq!(commands, ["ls", "cd src", "echo a\nb"]);
        assert!(std::fs::read_to_string(&path).unwrap().lines().all(|l| l.starts_with('{')));
    }

    #[test]
    fn stealth_skips_directories_and_clearing_forgets_them() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, false).unwrap();
        history.stealth_mode = true;
        history.record_dir(temp.path()).unwrap();
        assert!(history.directories().is_empty());
        history.stealth_mode = false;
        history.record_dir(temp.path()).unwrap();
        history.save(HistoryItem::from_command_line("echo x")).unwrap();
        history.remove_where(|_, _| true).unwrap();
        assert!(history.directories().is_empty());
        assert!(std::fs::read_to_string(path).unwrap().trim().is_empty());
    }

    #[test]
    fn short_names_persist_and_survive_clearing_history() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let mut history = SparshHistory::open(path.clone(), 100, false).unwrap();
        history.set_dir_alias("work", temp.path()).unwrap();
        history.set_dir_alias("tmp", temp.path()).unwrap();
        assert!(history.remove_dir_alias("tmp").unwrap());
        assert!(!history.remove_dir_alias("tmp").unwrap());
        history.save(HistoryItem::from_command_line("echo x")).unwrap();
        history.remove_where(|_, _| true).unwrap();
        let reopened = SparshHistory::open(path, 100, false).unwrap();
        assert_eq!(reopened.dir_aliases(), vec![("work".to_string(), temp.path().to_path_buf())]);
    }
}
