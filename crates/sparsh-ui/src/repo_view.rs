//! `repl`: edit the session's declarations (imports, variables, structs,
//! functions) as one Spar file, in Sparsh's editor or with `--editor` in the
//! editor you configured. The result replaces the live session only if the
//! whole file compiles.

use std::io::{self, BufRead, Write};
use std::process::Command;

use sparsh_core::ShellSession;

use crate::command_editor::{edit_spar_source, EditorAction};
use crate::{render_error, SemanticRole, Theme};

const EDITOR_HELP: &str = "repl --editor: no editor is configured.

Set one in your config, for example in ~/.sparsh/src/modules/enviroment.spar:

    SparshEnvironmentVariable(name: \"EDITOR\", value: some(value: \"nvim\")),

or for this session only:

    export EDITOR=nvim

Editors that need a flag to wait work too: export EDITOR=\"code --wait\"";

/// The editor command from the shell's environment (`EDITOR`, then `VISUAL`).
fn configured_editor(session: &ShellSession) -> Option<String> {
    let snapshot = session.ui_snapshot();
    ["EDITOR", "VISUAL"].into_iter().find_map(|name| {
        snapshot
            .environment_value(name)
            .map(|value| value.to_string_lossy().trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

pub(crate) fn run_repo(session: &mut ShellSession, theme: &Theme, use_editor: bool) -> io::Result<()> {
    let original = session.repo_source();
    let initial = if original.is_empty() {
        "// Nothing declared in this session yet. Add imports, variables, structs\n// and functions here.\n".to_string()
    } else {
        original.clone()
    };
    if use_editor {
        let Some(editor) = configured_editor(session) else {
            return note(theme, EDITOR_HELP);
        };
        return apply_loop(session, theme, &original, initial, |_, text| edit_externally(&editor, text));
    }
    apply_loop(session, theme, &original, initial, |session, text| {
        session.set_repo_editing(true);
        let snapshot = session.ui_snapshot();
        let edited = edit_spar_source(text, &snapshot, session);
        session.set_repo_editing(false);
        Ok(match edited? {
            Some(result) if result.action == EditorAction::Save => Some(result.text),
            _ => None,
        })
    })
}

/// Opens `text` in `editor` (a command line such as `nvim` or `code --wait`)
/// and returns what it was saved as.
fn edit_externally(editor: &str, text: &str) -> io::Result<Option<String>> {
    let mut words = editor.split_whitespace();
    let Some(program) = words.next() else {
        return Ok(None);
    };
    let file = tempfile::Builder::new().prefix("sparsh-repl-").suffix(".spar").tempfile()?;
    std::fs::write(file.path(), text)?;
    let status = Command::new(program).args(words).arg(file.path()).status();
    match status {
        Ok(status) if status.success() => Ok(Some(std::fs::read_to_string(file.path())?)),
        Ok(status) => {
            writeln!(io::stderr(), "repl: {program} exited with {status}; nothing applied")?;
            Ok(None)
        }
        Err(error) => {
            writeln!(io::stderr(), "repl: cannot run `{program}`: {error}")?;
            Ok(None)
        }
    }
}

fn apply_loop(
    session: &mut ShellSession,
    theme: &Theme,
    original: &str,
    mut text: String,
    mut edit: impl FnMut(&mut ShellSession, &str) -> io::Result<Option<String>>,
) -> io::Result<()> {
    loop {
        let Some(edited) = edit(session, &text)? else {
            return note(theme, "repl: closed, no changes");
        };
        text = edited;
        if text.trim() == original.trim() {
            return note(theme, "repl: no changes");
        }
        match session.apply_repo_source(&text) {
            Ok((added, removed)) => {
                let mut summary = String::from("repl: session updated");
                if !added.is_empty() {
                    summary.push_str(&format!("; added {}", added.join(", ")));
                }
                if !removed.is_empty() {
                    summary.push_str(&format!("; removed {}", removed.join(", ")));
                }
                return note(theme, &summary);
            }
            Err(error) => {
                let stderr = io::stderr();
                let mut stderr = stderr.lock();
                render_error(&error, Some(&text), theme, &mut stderr)?;
                writeln!(
                    stderr,
                    "{}",
                    theme.paint(
                        SemanticRole::Warning,
                        "repl: nothing was changed. Press Enter to fix it, or type q then Enter to discard."
                    )
                )?;
                let mut answer = String::new();
                io::stdin().lock().read_line(&mut answer)?;
                if answer.trim().eq_ignore_ascii_case("q") {
                    return note(theme, "repl: discarded, session unchanged");
                }
            }
        }
    }
}

fn note(theme: &Theme, message: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "{}", theme.paint(SemanticRole::Secondary, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_editor_message_has_a_working_example() {
        assert!(EDITOR_HELP.contains("export EDITOR=nvim"));
        assert!(EDITOR_HELP.contains("SparshEnvironmentVariable"));
    }

    #[test]
    fn external_editor_round_trips_text_through_a_temp_file() {
        // `true` leaves the file untouched.
        assert_eq!(edit_externally("true", "var a: int = 1;").unwrap().as_deref(), Some("var a: int = 1;"));
        assert_eq!(edit_externally("false", "x").unwrap(), None);
        assert_eq!(edit_externally("definitely-not-an-editor-zzz", "x").unwrap(), None);
    }
}
