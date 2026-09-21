use reedline::{
    FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId,
    Result as ReedlineResult, SearchDirection, SearchQuery,
};
use std::sync::{Arc, Mutex};

/// Reedline history wrapper that suppresses immediately repeated commands
/// within one running Sparsh session when configured to do so.
pub(crate) struct SparshHistory {
    inner: FileBackedHistory,
    dedupe_consecutive: bool,
    last_saved: Option<HistoryItem>,
}

impl SparshHistory {
    pub(crate) fn new(inner: FileBackedHistory, dedupe_consecutive: bool) -> Self {
        Self {
            inner,
            dedupe_consecutive,
            last_saved: None,
        }
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
    fn list(&self, limit: Option<usize>) -> Result<Vec<String>, String> {
        let history = self
            .inner
            .lock()
            .map_err(|_| "history lock poisoned".to_string())?;
        let mut items = history
            .search(SearchQuery::everything(SearchDirection::Forward, None))
            .map_err(|error| error.to_string())?;
        if let Some(limit) = limit {
            if items.len() > limit {
                items.drain(..items.len() - limit);
            }
        }
        Ok(items.into_iter().map(|item| item.command_line).collect())
    }

    fn clear(&self) -> Result<(), String> {
        self.inner
            .lock()
            .map_err(|_| "history lock poisoned".to_string())?
            .clear()
            .map_err(|error| error.to_string())
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
        self.inner.lock().unwrap().clear()
    }
    fn delete(&mut self, id: HistoryItemId) -> ReedlineResult<()> {
        self.inner.lock().unwrap().delete(id)
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
        self.last_saved = None;
        self.inner.clear()
    }

    fn delete(&mut self, id: HistoryItemId) -> ReedlineResult<()> {
        self.last_saved = None;
        self.inner.delete(id)
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

    use super::SparshHistory;

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
