//! The hint line: the signature of the Spar call the cursor is in, otherwise
//! the usual history hint.

use std::sync::{Arc, RwLock};

use reedline::{DefaultHinter, Hinter, History};
use sparsh_core::{signature_hint_info, CompletionSnapshot, SignatureHint};

use crate::{SemanticRole, Theme};

pub struct SparshHinter {
    snapshot: Arc<RwLock<CompletionSnapshot>>,
    history: DefaultHinter,
    theme: Theme,
    /// True while the last hint was a signature, which must never be inserted
    /// into the buffer by the "accept hint" keys.
    showing_signature: bool,
}

impl SparshHinter {
    pub fn new(snapshot: Arc<RwLock<CompletionSnapshot>>, theme: Theme) -> Self {
        // The history suggestion is dim gray (xterm 243): readable if you look, but
        // clearly not typed text, so a backspace visibly removes what was there.
        Self {
            snapshot,
            history: DefaultHinter::default().with_style(theme.style(SemanticRole::HintHistory)),
            theme,
            showing_signature: false,
        }
    }
}

/// `  build(profile: str, release: bool = false)` with the active parameter in
/// bold when colors are on.
pub fn render_signature_hint(hint: &SignatureHint, theme: &Theme) -> String {
    if !theme.enabled() {
        return format!("  {}", hint.text);
    }
    let dim = theme.style(SemanticRole::HintSignature);
    let active = theme.style(SemanticRole::HintActive);
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
            Some(hint) => {
                let plain = Theme::plain();
                render_signature_hint(&hint, if use_ansi_coloring { &self.theme } else { &plain })
            },
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
        let mut hinter = SparshHinter::new(snapshot(), Theme::plain());
        let line = "build(profile: ";
        let hint = hinter.handle(line, line.len(), &history, false, ".");
        assert_eq!(hint, "  build(profile: str, release: bool = false)");
        assert_eq!(hinter.complete_hint(), "");
        assert_eq!(hinter.next_hint_token(), "");
    }

    #[test]
    fn outside_a_call_the_history_hint_is_used() {
        let history = FileBackedHistory::new(10).unwrap();
        let mut hinter = SparshHinter::new(snapshot(), Theme::plain());
        assert_eq!(hinter.handle("build", 5, &history, false, "."), "");
        assert!(!hinter.showing_signature);
    }

    #[test]
    fn the_active_parameter_is_bold_when_colors_are_on() {
        let hint = SignatureHint { text: "f(a: int, b: str)".into(), active: 1, active_range: 10..16 };
        let plain = render_signature_hint(&hint, &Theme::plain());
        assert_eq!(plain, "  f(a: int, b: str)");
        let colored = render_signature_hint(&hint, &Theme::colored());
        assert!(colored.contains("\u{1b}["), "{colored:?}");
        assert!(colored.contains("b: str"));
    }
}
