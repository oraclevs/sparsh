use std::sync::{Arc, RwLock};

use reedline::{ValidationResult, Validator};
use sparsh_core::{input_completeness, EditorMode, InputCompleteness};

pub struct SparshValidator {
    mode: Arc<RwLock<EditorMode>>,
}

impl SparshValidator {
    pub fn new(mode: Arc<RwLock<EditorMode>>) -> Self {
        Self { mode }
    }
}

impl Validator for SparshValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        let mode = self
            .mode
            .read()
            .map(|mode| *mode)
            .unwrap_or(EditorMode::Normal);
        if mode == EditorMode::Repl {
            return match input_completeness(line) {
                InputCompleteness::Incomplete => ValidationResult::Incomplete,
                InputCompleteness::Complete if line.ends_with('\n') || line.trim().is_empty() => {
                    ValidationResult::Complete
                }
                InputCompleteness::Complete => ValidationResult::Incomplete,
            };
        }
        if !looks_like_spar_input(line) {
            return ValidationResult::Complete;
        }
        match input_completeness(line) {
            InputCompleteness::Complete => ValidationResult::Complete,
            InputCompleteness::Incomplete => ValidationResult::Incomplete,
        }
    }
}

fn looks_like_spar_input(line: &str) -> bool {
    let line = line.trim_start();
    if line.contains("|>") {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "var",
        "function",
        "private function",
        "functionGroup",
        "struct",
        "type",
        "enum",
        "import",
        "export",
        "dynamic",
        "async",
        "if",
        "for",
        "shell",
        "exec",
    ];
    PREFIXES.iter().any(|prefix| {
        line == *prefix
            || line
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    }) || starts_with_call_syntax(line)
}

fn starts_with_call_syntax(line: &str) -> bool {
    let mut chars = line.char_indices().peekable();
    let Some((_, first)) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_alphabetic()) {
        return false;
    }
    let mut end = first.len_utf8();
    while let Some(&(index, ch)) = chars.peek() {
        if ch == '_' || ch.is_alphanumeric() {
            end = index + ch.len_utf8();
            chars.next();
        } else {
            break;
        }
    }
    line[end..].trim_start().starts_with('(')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_complete(result: ValidationResult) {
        assert!(matches!(result, ValidationResult::Complete));
    }

    fn assert_incomplete(result: ValidationResult) {
        assert!(matches!(result, ValidationResult::Incomplete));
    }

    #[test]
    fn shell_commands_are_complete_but_open_spar_constructs_are_not() {
        let mode = Arc::new(RwLock::new(EditorMode::Normal));
        let validator = SparshValidator::new(mode);
        assert_complete(validator.validate("git status"));
        assert_complete(validator.validate("echo foo(bar)"));
        assert_complete(validator.validate("build()"));
        assert_incomplete(validator.validate("function build() -> int {"));
        assert_incomplete(validator.validate("shell {"));
        assert_incomplete(validator.validate("build("));
    }

    #[test]
    fn structured_pipeline_continuation_is_parser_driven_in_normal_mode() {
        let mode = Arc::new(RwLock::new(EditorMode::Normal));
        let validator = SparshValidator::new(mode);

        assert_incomplete(validator.validate("users |>"));
        assert_complete(validator.validate("users |> take(2)"));
        assert_complete(validator.validate("git log --oneline | head"));
    }

    #[test]
    fn repl_mode_uses_blank_line_as_block_submit() {
        let mode = Arc::new(RwLock::new(EditorMode::Repl));
        let validator = SparshValidator::new(mode);
        assert_incomplete(validator.validate("var a: int = 1;"));
        assert_complete(validator.validate("var a: int = 1;\n"));
    }
}
