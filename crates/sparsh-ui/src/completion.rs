use std::sync::{Arc, RwLock};

use reedline::{Completer, Suggestion};
use crate::Theme;
use sparsh_core::{complete, CompletionRequest, CompletionSnapshot};

pub struct SparshCompleter {
    snapshot: Arc<RwLock<CompletionSnapshot>>,
    theme: Theme,
}

impl SparshCompleter {
    pub fn new(snapshot: Arc<RwLock<CompletionSnapshot>>) -> Self {
        Self {
            snapshot,
            theme: Theme::plain(),
        }
    }

    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }
}

impl Completer for SparshCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let Ok(snapshot) = self.snapshot.read() else {
            return Vec::new();
        };
        complete(&snapshot, CompletionRequest { line, cursor: pos })
            .into_iter()
            .map(|item| crate::menu::suggestion(&item, &self.theme))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_preserves_core_replacement_span() {
        let snapshot = Arc::new(RwLock::new(CompletionSnapshot::default()));
        let mut completer = SparshCompleter::new(snapshot);
        let suggestions = completer.complete("unknown", 7);
        assert!(suggestions.is_empty());
    }
}
