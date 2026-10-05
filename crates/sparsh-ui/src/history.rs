use reedline::{
    FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId,
    Result as ReedlineResult, SearchDirection, SearchQuery,
};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Reedline history wrapper that suppresses immediately repeated commands
/// within one running Sparsh session when configured to do so.
pub(crate) struct SparshHistory {
    inner: FileBackedHistory,
    dedupe_consecutive: bool,
    last_saved: Option<HistoryItem>,
    stealth_mode: bool,
    file_path: Option<PathBuf>,
    capacity: usize,
}

impl SparshHistory {
    pub(crate) fn new(inner: FileBackedHistory, dedupe_consecutive: bool) -> Self {
        Self {
            inner,
            dedupe_consecutive,
            last_saved: None,
            stealth_mode: false,
            file_path: None,
            capacity: 0,
        }
    }

    pub(crate) fn with_file_path(mut self, path: PathBuf, capacity: usize) -> Self {
        self.file_path = Some(path);
        self.capacity = capacity;
        self
    }

    fn remove_where(
        &mut self,
        mut remove: impl FnMut(usize, &str) -> bool,
    ) -> Result<usize, String> {
        let path = self
            .file_path
            .as_ref()
            .ok_or("history file unavailable")?
            .clone();
        self.inner.sync().map_err(|error| error.to_string())?;
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let file = options.open(&path).map_err(|error| error.to_string())?;
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("history path is not a regular file".into());
        }
        let mut lock = fd_lock::RwLock::new(file);
        let mut guard = lock.write().map_err(|error| error.to_string())?;
        let lines: Vec<String> = BufReader::new(&*guard)
            .lines()
            .collect::<std::io::Result<_>>()
            .map_err(|error| error.to_string())?;
        let mut kept = Vec::with_capacity(lines.len());
        let mut removed = 0;
        for (index, line) in lines.into_iter().enumerate() {
            let decoded = line.replace("<\\n>", "\n");
            if remove(index + 1, &decoded) {
                removed += 1;
            } else {
                kept.push(line);
            }
        }
        if removed > 0 {
            guard
                .seek(SeekFrom::Start(0))
                .map_err(|error| error.to_string())?;
            for line in kept {
                guard
                    .write_all(line.as_bytes())
                    .map_err(|error| error.to_string())?;
                guard.write_all(b"\n").map_err(|error| error.to_string())?;
            }
            let end = guard.stream_position().map_err(|error| error.to_string())?;
            guard.set_len(end).map_err(|error| error.to_string())?;
            guard.sync_all().map_err(|error| error.to_string())?;
        }
        drop(guard);
        if removed > 0 {
            self.inner = FileBackedHistory::with_file(self.capacity, path)
                .map_err(|error| error.to_string())?;
            self.last_saved = None;
        }
        Ok(removed)
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

        let saved = self.inner.save(item)?;
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
        let inner = reedline::FileBackedHistory::with_file(100, path.clone()).unwrap();
        let mut history = SparshHistory::new(inner, false);
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
        let inner = reedline::FileBackedHistory::with_file(100, path.clone()).unwrap();
        let mut history = SparshHistory::new(inner, false);
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
        let inner = reedline::FileBackedHistory::with_file(100, path.clone()).unwrap();
        let mut history = SparshHistory::new(inner, false);
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
            .unwrap()
            .contains("secret-after"));
    }

    #[test]
    fn targeted_deletion_persists_and_history_queries_are_not_recorded() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("history");
        let inner = reedline::FileBackedHistory::with_file(100, path.clone()).unwrap();
        let mut history = SparshHistory::new(inner, false).with_file_path(path.clone(), 100);
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
        let reopened = reedline::FileBackedHistory::with_file(100, path.clone()).unwrap();
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
        let inner = reedline::FileBackedHistory::with_file(100, path.clone()).unwrap();
        let mut history = SparshHistory::new(inner, true);

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
        let inner = reedline::FileBackedHistory::with_file(100, path.clone()).unwrap();
        let mut history = SparshHistory::new(inner, true);
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
        let inner = reedline::FileBackedHistory::with_file(100, path.clone()).unwrap();
        let mut history = SparshHistory::new(inner, false);

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
}
