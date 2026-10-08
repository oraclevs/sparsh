//! The hint line: the signature of the Spar call the cursor is in, otherwise
//! the usual history hint.

use std::sync::{Arc, RwLock};

use nu_ansi_term::{Color, Style};
use reedline::{DefaultHinter, Hinter, History};
use sparsh_core::{signature_hint_info, CompletionSnapshot, SignatureHint};

pub struct SparshHinter {
    snapshot: Arc<RwLock<CompletionSnapshot>>,
    history: DefaultHinter,
    /// True while the last hint was a signature, which must never be inserted
    /// into the buffer by the "accept hint" keys.
    showing_signature: bool,
}

impl SparshHinter {
    pub fn new(snapshot: Arc<RwLock<CompletionSnapshot>>) -> Self {
        Self { snapshot, history: DefaultHinter::default(), showing_signature: false }
    }
}

/// `  build(profile: str, release: bool = false)` with the active parameter in
/// bold when colors are on.
pub fn render_signature_hint(hint: &SignatureHint, use_ansi_coloring: bool) -> String {
    if !use_ansi_coloring {
        return format!("  {}", hint.text);
    }
    let dim = Style::new().fg(Color::DarkGray);
    let active = Style::new().fg(Color::LightCyan).bold();
    let range = &hint.active_range;
    format!(
        "  {}{}{}",
        dim.paint(&hint.text[..range.start]),
        active.paint(&hint.text[range.clone()]),
        dim.paint(&hint.text[range.end..]),
    )
}

impl Hinter for SparshHinter {
    fn handle(
        &mut self,
        line: &str,
        pos: usize,
        history: &dyn History,
        use_ansi_coloring: bool,
        cwd: &str,
    ) -> String {
        let signature = self
            .snapshot
            .read()
            .ok()
            .and_then(|snapshot| signature_hint_info(&snapshot, line, pos));
        self.showing_signature = signature.is_some();
        match signature {
            Some(hint) => render_signature_hint(&hint, use_ansi_coloring),
            None => self.history.handle(line, pos, history, use_ansi_coloring, cwd),
        }
    }

    fn complete_hint(&self) -> String {
        if self.showing_signature {
            String::new()
        } else {
            self.history.complete_hint()
        }
    }

    fn next_hint_token(&self) -> String {
        if self.showing_signature {
            String::new()
        } else {
            self.history.next_hint_token()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reedline::FileBackedHistory;

    fn snapshot() -> Arc<RwLock<CompletionSnapshot>> {
        Arc::new(RwLock::new(CompletionSnapshot::for_session(
            std::env::temp_dir(),
            "fn build(profile: str, release: bool = false) -> int { return 0; };",
            &[("build", &["profile", "release"])],
        )))
    }

    #[test]
    fn the_signature_is_the_hint_inside_a_call_and_is_never_accepted() {
        let history = FileBackedHistory::new(10).unwrap();
        let mut hinter = SparshHinter::new(snapshot());
        let line = "build(profile: ";
        let hint = hinter.handle(line, line.len(), &history, false, ".");
        assert_eq!(hint, "  build(profile: str, release: bool = false)");
        assert_eq!(hinter.complete_hint(), "");
        assert_eq!(hinter.next_hint_token(), "");
    }

    #[test]
    fn outside_a_call_the_history_hint_is_used() {
        let history = FileBackedHistory::new(10).unwrap();
        let mut hinter = SparshHinter::new(snapshot());
        assert_eq!(hinter.handle("build", 5, &history, false, "."), "");
        assert!(!hinter.showing_signature);
    }

    #[test]
    fn the_active_parameter_is_bold_when_colors_are_on() {
        let hint = SignatureHint { text: "f(a: int, b: str)".into(), active: 1, active_range: 10..16 };
        let plain = render_signature_hint(&hint, false);
        assert_eq!(plain, "  f(a: int, b: str)");
        let colored = render_signature_hint(&hint, true);
        assert!(colored.contains("\u{1b}["), "{colored:?}");
        assert!(colored.contains("b: str"));
    }
}
