use std::sync::{Arc, RwLock};

use reedline::{Completer, Span, Suggestion};
use sparsh_core::{complete, CompletionRequest, CompletionSnapshot};

pub struct SparshCompleter {
    snapshot: Arc<RwLock<CompletionSnapshot>>,
}

impl SparshCompleter {
    pub fn new(snapshot: Arc<RwLock<CompletionSnapshot>>) -> Self {
        Self { snapshot }
    }
}

impl Completer for SparshCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let Ok(snapshot) = self.snapshot.read() else {
            return Vec::new();
        };
        complete(&snapshot, CompletionRequest { line, cursor: pos })
            .into_iter()
            .map(|item| Suggestion {
                value: item.replacement,
                description: item.description,
                span: Span {
                    start: item.span.start,
                    end: item.span.end,
                },
                ..Suggestion::default()
            })
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
