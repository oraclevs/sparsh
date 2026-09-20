use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use sparsh_core::{input_completeness, InputCompleteness, ShellUiSnapshot};

use crate::command_editor::{edit_text, EditorAction, EditorResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteDecision {
    Execute,
    Edit,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PasteEditOutcome {
    Execute(String),
    Save(String),
    Cancel,
}

pub fn is_multiline_paste_candidate(source: &str) -> bool {
    source.contains('\n') || source.contains('\r')
}

/// Break a reviewed multiline paste into the submissions Sparsh would have
/// received if the user entered the same text interactively line by line.
/// Structurally incomplete Spar constructs remain grouped until they become
/// complete, so function/if/shell blocks are not torn apart at newlines.
pub fn multiline_submissions(source: &str) -> Vec<String> {
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let mut submissions = Vec::new();
    let mut pending = String::new();

    for line in normalized.lines() {
        if pending.is_empty() && line.trim().is_empty() {
            continue;
        }
        if !pending.is_empty() {
            pending.push('\n');
        }
        pending.push_str(line);

        if !pending.trim().is_empty()
            && input_completeness(&pending) == InputCompleteness::Complete
        {
            submissions.push(std::mem::take(&mut pending));
        }
    }

    if !pending.trim().is_empty() {
        submissions.push(pending);
    }

    submissions
}

pub fn review_multiline_paste(
    source: &str,
    snapshot: &ShellUiSnapshot,
) -> io::Result<Option<String>> {
    let stderr = io::stderr();
    let mut err = stderr.lock();
    writeln!(err, "\nSparsh paste review ({} lines):", source.lines().count().max(1))?;
    writeln!(err, "---")?;
    writeln!(err, "{source}")?;
    writeln!(err, "---")?;
    write!(err, "[e]xecute, [d]edit, [c]ancel: ")?;
    err.flush()?;
    drop(err);

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    match parse_decision(&answer) {
        PasteDecision::Execute => Ok(Some(source.to_string())),
        PasteDecision::Cancel => Ok(None),
        PasteDecision::Edit => match edit_paste(source, snapshot)? {
            PasteEditOutcome::Execute(source) => Ok(Some(source)),
            PasteEditOutcome::Save(source) => {
                let path = save_paste_draft(&source)?;
                writeln!(io::stderr().lock(), "saved Sparsh draft: {}", path.display())?;
                Ok(None)
            }
            PasteEditOutcome::Cancel => Ok(None),
        },
    }
}

fn parse_decision(input: &str) -> PasteDecision {
    match input.trim().to_ascii_lowercase().as_str() {
        "e" | "execute" | "y" | "yes" => PasteDecision::Execute,
        "d" | "edit" => PasteDecision::Edit,
        _ => PasteDecision::Cancel,
    }
}

fn edit_paste(source: &str, snapshot: &ShellUiSnapshot) -> io::Result<PasteEditOutcome> {
    Ok(classify_editor_result(edit_text(source, true, snapshot)?))
}

fn classify_editor_result(result: Option<EditorResult>) -> PasteEditOutcome {
    match result {
        Some(EditorResult {
            text,
            action: EditorAction::Execute,
        }) => PasteEditOutcome::Execute(text),
        Some(EditorResult {
            text,
            action: EditorAction::Save,
        }) => PasteEditOutcome::Save(text),
        Some(EditorResult {
            action: EditorAction::Cancel,
            ..
        })
        | None => PasteEditOutcome::Cancel,
    }
}

fn save_paste_draft(source: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".sparsh")
        .join("drafts")
        .join("last-paste.spar");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, source)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_candidate_keeps_the_entire_block_intact() {
        let source = "var a: int = 1;\na";
        assert!(is_multiline_paste_candidate(source));
        assert_eq!(source.lines().collect::<Vec<_>>(), ["var a: int = 1;", "a"]);
    }

    #[test]
    fn paste_decisions_are_conservative_by_default() {
        assert_eq!(parse_decision("execute"), PasteDecision::Execute);
        assert_eq!(parse_decision("edit"), PasteDecision::Edit);
        assert_eq!(parse_decision("anything else"), PasteDecision::Cancel);
    }

    #[test]
    fn multiline_shell_paste_becomes_sequential_submissions() {
        let source = "\ncd spar/\ncargo check\ncargo test formatter::tests\ncargo install --path . --force\n";

        assert_eq!(
            multiline_submissions(source),
            vec![
                "cd spar/".to_string(),
                "cargo check".to_string(),
                "cargo test formatter::tests".to_string(),
                "cargo install --path . --force".to_string(),
            ]
        );
    }

    #[test]
    fn incomplete_spar_construct_stays_grouped_inside_multiline_paste() {
        let source = "function build() -> int {\n    return 0;\n};\nbuild()";

        assert_eq!(
            multiline_submissions(source),
            vec![
                "function build() -> int {\n    return 0;\n};".to_string(),
                "build()".to_string(),
            ]
        );
    }

    #[test]
    fn shell_block_stays_grouped_inside_multiline_paste() {
        let source = "shell {\n    echo one;\n    echo two;\n}\npwd";

        assert_eq!(
            multiline_submissions(source),
            vec![
                "shell {\n    echo one;\n    echo two;\n}".to_string(),
                "pwd".to_string(),
            ]
        );
    }

    #[test]
    fn owned_editor_distinguishes_execute_save_and_cancel() {
        assert_eq!(
            classify_editor_result(Some(EditorResult {
                text: "echo run".into(),
                action: EditorAction::Execute,
            })),
            PasteEditOutcome::Execute("echo run".into())
        );
        assert_eq!(
            classify_editor_result(Some(EditorResult {
                text: "echo draft".into(),
                action: EditorAction::Save,
            })),
            PasteEditOutcome::Save("echo draft".into())
        );
        assert_eq!(classify_editor_result(None), PasteEditOutcome::Cancel);
    }
}
