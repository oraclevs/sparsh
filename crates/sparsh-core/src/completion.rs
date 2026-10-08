use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::command_args::{argument_kind, is_known_command, positional_index, ArgumentKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionRequest<'a> {
    pub line: &'a str,
    pub cursor: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub replacement: String,
    pub span: Range<usize>,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionContext {
    Command,
    Argument,
    Path,
    Directory,
    File,
    SparIdentifier,
    Import,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathCompletionMode {
    FilesAndDirectories,
    DirectoriesOnly,
    FilesOnly,
}

#[derive(Clone, Debug, Default)]
pub struct CompletionSnapshot {
    pub(crate) cwd: PathBuf,
    pub(crate) home: Option<PathBuf>,
    pub(crate) builtins: BTreeMap<String, String>,
    pub(crate) aliases: BTreeSet<String>,
    pub(crate) executables: BTreeSet<String>,
    pub(crate) spar_identifiers: BTreeSet<String>,
    pub(crate) spar_functions: BTreeMap<String, Vec<String>>,
}

impl CompletionSnapshot {
    #[cfg(test)]
    fn fixture() -> Self {
        Self {
            cwd: PathBuf::from("/work"),
            home: Some(PathBuf::from("/home/occ")),
            builtins: BTreeMap::from([
                ("cd".into(), "Change directory".into()),
                ("pwd".into(), "Print directory".into()),
            ]),
            aliases: BTreeSet::from(["gs".into()]),
            executables: BTreeSet::from(["git".into(), "grep".into()]),
            spar_identifiers: BTreeSet::from(["build".into(), "project".into()]),
            spar_functions: BTreeMap::from([("build".into(), vec!["profile".into()])]),
        }
    }
}

pub fn complete(
    snapshot: &CompletionSnapshot,
    request: CompletionRequest<'_>,
) -> Vec<CompletionItem> {
    let cursor = request.cursor.min(request.line.len());
    if !request.line.is_char_boundary(cursor) {
        return Vec::new();
    }
    if let Some(items) = complete_function_parameters(snapshot, request.line, cursor) {
        return items;
    }
    let (context, span) = classify(request.line, cursor);
    let token = &request.line[span.clone()];
    let mut items = match context {
        CompletionContext::Command => {
            let mut items = complete_commands(snapshot, token, span.clone());
            items.extend(complete_paths(
                snapshot,
                token,
                span,
                PathCompletionMode::FilesAndDirectories,
            ));
            items
        }
        CompletionContext::Path => complete_paths(
            snapshot,
            token,
            span,
            PathCompletionMode::FilesAndDirectories,
        ),
        CompletionContext::Directory => {
            complete_paths(snapshot, token, span, PathCompletionMode::DirectoriesOnly)
        }
        CompletionContext::File => {
            complete_paths(snapshot, token, span, PathCompletionMode::FilesOnly)
        }
        CompletionContext::SparIdentifier => complete_spar_identifiers(snapshot, token, span),
        CompletionContext::Import => complete_imports(snapshot, token, span),
        CompletionContext::Argument => {
            if statement_argument_kind(request.line, cursor) == Some(ArgumentKind::Nothing) {
                Vec::new()
            } else {
                complete_paths(
                    snapshot,
                    token,
                    span,
                    PathCompletionMode::FilesAndDirectories,
                )
            }
        }
    };
    items.sort_by(|left, right| {
        let left_exact = left.replacement == token;
        let right_exact = right.replacement == token;
        right_exact
            .cmp(&left_exact)
            .then_with(|| left.replacement.cmp(&right.replacement))
    });
    items.dedup_by(|left, right| left.replacement == right.replacement);
    items
}

pub fn classify(line: &str, cursor: usize) -> (CompletionContext, Range<usize>) {
    let cursor = cursor.min(line.len());
    let prefix = &line[..cursor];
    let start = active_token_start(prefix);
    let span = start..cursor;
    let token = &line[span.clone()];
    let trimmed = prefix.trim_start();

    if trimmed.starts_with("import ") || trimmed.starts_with("import pkg ") {
        return (CompletionContext::Import, span);
    }
    let statement = statement_before(line, start);
    let statement_is_command = statement
        .split_whitespace()
        .next()
        .is_some_and(is_known_command);
    // A Spar line such as `if x { cat ` ends in a shell statement once the
    // block opens; only an explicitly known command takes it back.
    let spar_line = looks_like_spar(trimmed)
        && !(statement.len() != trimmed.len() - token.len() && statement_is_command);
    if spar_line {
        return (CompletionContext::SparIdentifier, span);
    }

    let command_position = statement.trim().is_empty();
    if command_position {
        if path_like(token) {
            (CompletionContext::Path, span)
        } else {
            (CompletionContext::Command, span)
        }
    } else if path_like(token) {
        (CompletionContext::Path, span)
    } else {
        match statement_kind(statement) {
            Some(ArgumentKind::Directories) => (CompletionContext::Directory, span),
            Some(ArgumentKind::Files) => (CompletionContext::File, span),
            _ => (CompletionContext::Argument, span),
        }
    }
}

/// Kind of argument being typed at the end of `line[..cursor]`, if a command is present.
fn statement_argument_kind(line: &str, cursor: usize) -> Option<ArgumentKind> {
    let start = active_token_start(&line[..cursor]);
    let statement = statement_before(line, start);
    if statement.trim().is_empty() {
        return None;
    }
    statement_kind(statement)
}

/// `statement` is the text of the current statement before the active token.
fn statement_kind(statement: &str) -> Option<ArgumentKind> {
    let words = split_words(statement);
    let (command, args) = words.split_first()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(argument_kind(command, positional_index(&args)))
}

/// Text of the current statement that precedes byte offset `end`: starts after the
/// last unquoted `;`, `|`, `&`, `{` or `}` and drops a leading `~ ` shell marker.
fn statement_before(line: &str, end: usize) -> &str {
    let prefix = &line[..end];
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in prefix.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(active) => {
                if active == '"' && ch == '\\' {
                    escaped = true;
                } else if ch == active {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '\\' => escaped = true,
                ';' | '|' | '&' | '{' | '}' => start = index + ch.len_utf8(),
                _ => {}
            },
        }
    }
    let statement = prefix[start..].trim_start();
    match statement.strip_prefix('~') {
        Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
        _ => statement,
    }
}

/// Whitespace split that keeps quoted spans together (quotes stripped).
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote = None;
    for ch in text.chars() {
        match quote {
            Some(active) => {
                if ch == active {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(ch);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

fn active_token_start(prefix: &str) -> usize {
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;

    for (index, character) in prefix.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(active) => {
                if active == '"' && character == '\\' {
                    escaped = true;
                } else if character == active {
                    quote = None;
                }
            }
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                }
                character
                    if character.is_whitespace()
                        || matches!(
                            character,
                            '|' | '&'
                                | ';'
                                | '>'
                                | '<'
                                | '='
                                | '('
                                | ')'
                                | '{'
                                | '}'
                                | '['
                                | ']'
                                | ','
                        ) =>
                {
                    start = index + character.len_utf8();
                }
                _ => {}
            },
        }
    }
    start
}

fn looks_like_spar(trimmed: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "var ",
        "var mut ",
        "fn ",
        "function ",
        "private fn ",
        "private function ",
        "async fn ",
        "const ",
        "while ",
        "loop ",
        "functionGroup ",
        "struct ",
        "impl ",
        "type ",
        "enum ",
        "if ",
        "for ",
        "export ",
    ];
    PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix)) || starts_with_call_syntax(trimmed)
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

fn path_like(token: &str) -> bool {
    token.contains('/') || token.starts_with('.') || token.starts_with('~')
}

fn complete_function_parameters(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
) -> Option<Vec<CompletionItem>> {
    let prefix = &line[..cursor];
    let open = unmatched_call_open(prefix)?;
    let before_open = prefix[..open].trim_end();
    let name_start = before_open
        .char_indices()
        .rev()
        .find_map(|(index, ch)| {
            (!(ch == '_' || ch.is_alphanumeric())).then_some(index + ch.len_utf8())
        })
        .unwrap_or(0);
    let function = &before_open[name_start..];
    if function.is_empty()
        || !function
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_alphabetic())
    {
        return None;
    }
    let params = snapshot.spar_functions.get(function)?;
    if params.is_empty() {
        return Some(Vec::new());
    }

    let args = &prefix[open + 1..];
    let segment_start = args.rfind(',').map_or(0, |index| index + 1);
    let segment = &args[segment_start..];
    let leading = segment.len() - segment.trim_start().len();
    let active_start = open + 1 + segment_start + leading;
    let active = &prefix[active_start..];

    let mut used = BTreeSet::new();
    for completed in args[..segment_start].split(',') {
        if let Some((name, _)) = completed.split_once(':') {
            used.insert(name.trim());
        }
    }

    if active.trim().is_empty() && used.is_empty() {
        return Some(vec![CompletionItem {
            replacement: params
                .iter()
                .map(|name| format!("{name}: "))
                .collect::<Vec<_>>()
                .join(", "),
            span: cursor..cursor,
            description: Some("function parameters".into()),
        }]);
    }

    if active.contains(':') {
        return Some(Vec::new());
    }
    let typed = active.trim();
    let items = params
        .iter()
        .filter(|name| !used.contains(name.as_str()) && name.starts_with(typed))
        .map(|name| CompletionItem {
            replacement: format!("{name}: "),
            span: active_start..cursor,
            description: Some(format!("parameter of {function}")),
        })
        .collect();
    Some(items)
}

fn unmatched_call_open(prefix: &str) -> Option<usize> {
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in prefix.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(active) = quote {
            if active == '"' && ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => stack.push(index),
            ')' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.last().copied()
}

fn complete_commands(
    snapshot: &CompletionSnapshot,
    prefix: &str,
    span: Range<usize>,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for (name, description) in &snapshot.builtins {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: name.clone(),
                span: span.clone(),
                description: Some(description.clone()),
            });
        }
    }
    for name in &snapshot.aliases {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: name.clone(),
                span: span.clone(),
                description: Some("alias".into()),
            });
        }
    }
    for name in &snapshot.executables {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: name.clone(),
                span: span.clone(),
                description: Some("external command".into()),
            });
        }
    }
    for name in snapshot.spar_functions.keys() {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: format!("{name}("),
                span: span.clone(),
                description: Some("Spar function".into()),
            });
        }
    }
    items
}

fn complete_spar_identifiers(
    snapshot: &CompletionSnapshot,
    prefix: &str,
    span: Range<usize>,
) -> Vec<CompletionItem> {
    snapshot
        .spar_identifiers
        .iter()
        .filter(|name| name.starts_with(prefix))
        .map(|name| CompletionItem {
            replacement: name.clone(),
            span: span.clone(),
            description: Some("Spar identifier".into()),
        })
        .collect()
}

fn complete_imports(
    snapshot: &CompletionSnapshot,
    prefix: &str,
    span: Range<usize>,
) -> Vec<CompletionItem> {
    if path_like(prefix) || prefix.is_empty() {
        complete_paths(
            snapshot,
            prefix,
            span,
            PathCompletionMode::FilesAndDirectories,
        )
    } else {
        complete_spar_identifiers(snapshot, prefix, span)
    }
}

fn complete_paths(
    snapshot: &CompletionSnapshot,
    token: &str,
    span: Range<usize>,
    mode: PathCompletionMode,
) -> Vec<CompletionItem> {
    let (quote, path_token) = completion_path_token(token);
    if path_token == "~" {
        return snapshot.home.as_ref().map_or_else(Vec::new, |_| {
            vec![CompletionItem {
                replacement: quote_completion("~/", quote, true),
                span,
                description: Some("home directory".into()),
            }]
        });
    }
    let (display_dir, typed_name) = split_path_token(path_token);
    let search_dir = expand_search_dir(&display_dir, &snapshot.cwd, snapshot.home.as_deref());
    let Ok(entries) = std::fs::read_dir(search_dir) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(&typed_name) {
            continue;
        }
        // `Path::is_dir` follows symlinks, which is what an interactive `cd`
        // completion needs: a symlink to a directory is still a valid target.
        let is_dir = entry.path().is_dir();
        if mode == PathCompletionMode::DirectoriesOnly && !is_dir {
            continue;
        }
        if mode == PathCompletionMode::FilesOnly && is_dir {
            continue;
        }
        let mut replacement = format!("{display_dir}{name}");
        if is_dir {
            replacement.push('/');
        }
        items.push(CompletionItem {
            replacement: quote_completion(&replacement, quote, is_dir),
            span: span.clone(),
            description: Some(if is_dir { "directory" } else { "file" }.into()),
        });
    }
    items
}

fn completion_path_token(token: &str) -> (Option<char>, &str) {
    let Some(first) = token.chars().next() else {
        return (None, token);
    };
    if !matches!(first, '\'' | '"') {
        return (None, token);
    }
    let body = &token[first.len_utf8()..];
    let body = body.strip_suffix(first).unwrap_or(body);
    (Some(first), body)
}

fn quote_completion(path: &str, existing_quote: Option<char>, is_dir: bool) -> String {
    match existing_quote {
        Some('\'') => {
            // Single quotes cannot contain a literal single quote without
            // ending the quote. Use a complete double-quoted replacement in
            // that rare case; otherwise preserve the user's quote style.
            if path.contains('\'') {
                format!("\"{}\"", escape_double_quoted(path))
            } else if is_dir {
                format!("'{path}")
            } else {
                format!("'{path}'")
            }
        }
        Some('"') => {
            if is_dir {
                format!("\"{}", escape_double_quoted(path))
            } else {
                format!("\"{}\"", escape_double_quoted(path))
            }
        }
        _ if path
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '\'' | '"')) =>
        {
            if is_dir {
                format!("\"{}", escape_double_quoted(path))
            } else {
                format!("\"{}\"", escape_double_quoted(path))
            }
        }
        _ => path.to_string(),
    }
}

fn escape_double_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn split_path_token(token: &str) -> (String, String) {
    match token.rfind('/') {
        Some(index) => (token[..=index].to_string(), token[index + 1..].to_string()),
        None => (String::new(), token.to_string()),
    }
}

fn expand_search_dir(display: &str, cwd: &Path, home: Option<&Path>) -> PathBuf {
    if display.is_empty() {
        return cwd.to_path_buf();
    }
    if display == "~/" {
        return home.unwrap_or(cwd).to_path_buf();
    }
    if let Some(rest) = display.strip_prefix("~/") {
        return home.unwrap_or(cwd).join(rest);
    }
    let path = Path::new(display);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_distinguishes_command_path_import_and_spar_contexts() {
        assert_eq!(classify("gi", 2).0, CompletionContext::Command);
        assert_eq!(classify("cd sr", 5).0, CompletionContext::Directory);
        assert_eq!(classify("./to", 4).0, CompletionContext::Path);
        assert_eq!(classify("import pkg { bu", 15).0, CompletionContext::Import);
        assert_eq!(classify("build(", 6).0, CompletionContext::SparIdentifier);
        assert_eq!(classify("fn greet", 9).0, CompletionContext::SparIdentifier);
        assert_eq!(
            classify("const LIM", 9).0,
            CompletionContext::SparIdentifier
        );
        assert_eq!(
            classify("while count", 11).0,
            CompletionContext::SparIdentifier
        );
        assert_eq!(classify("echo foo(bar)", 13).0, CompletionContext::Argument);
    }

    #[test]
    fn function_name_completion_offers_call_form_without_changing_bare_command_semantics() {
        let mut snapshot = CompletionSnapshot::fixture();
        snapshot
            .spar_functions
            .insert("create".into(), vec!["name".into(), "path".into()]);

        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "cre",
                cursor: 3,
            },
        );

        assert!(items.iter().any(|item| {
            item.replacement == "create(" && item.description.as_deref() == Some("Spar function")
        }));
    }

    #[test]
    fn function_call_tab_completion_inserts_named_argument_labels() {
        let mut snapshot = CompletionSnapshot::fixture();
        snapshot
            .spar_functions
            .insert("create".into(), vec!["name".into(), "path".into()]);

        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "create(",
                cursor: 7,
            },
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].replacement, "name: , path: ");
        assert_eq!(items[0].span, 7..7);
        assert_eq!(items[0].description.as_deref(), Some("function parameters"));
    }

    #[test]
    fn function_parameter_completion_filters_already_typed_prefix() {
        let mut snapshot = CompletionSnapshot::fixture();
        snapshot
            .spar_functions
            .insert("create".into(), vec!["name".into(), "path".into()]);

        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "create(na",
                cursor: 9,
            },
        );

        assert!(items.iter().any(|item| item.replacement == "name: "));
        assert!(items.iter().all(|item| item.replacement != "path: "));
    }

    #[test]
    fn command_completion_combines_sources_and_deduplicates() {
        let snapshot = CompletionSnapshot::fixture();
        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "g",
                cursor: 1,
            },
        );
        let names = items
            .into_iter()
            .map(|item| item.replacement)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["git", "grep", "gs"]);
    }

    #[test]
    fn spar_completion_preserves_only_active_token_span() {
        let snapshot = CompletionSnapshot::fixture();
        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "var result = bu",
                cursor: 15,
            },
        );
        assert_eq!(items[0].replacement, "build");
        assert_eq!(items[0].span, 13..15);
    }
    #[test]
    fn ordinary_command_arguments_offer_filesystem_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("Projects")).unwrap();
        let snapshot = CompletionSnapshot {
            cwd: temp.path().to_path_buf(),
            home: None,
            ..CompletionSnapshot::default()
        };

        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "ls Pro",
                cursor: 6,
            },
        );

        assert!(items.iter().any(|item| item.replacement == "Projects/"));
    }

    #[test]
    fn cd_arguments_offer_paths_without_dot_or_slash_prefix() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("Projects")).unwrap();
        let snapshot = CompletionSnapshot {
            cwd: temp.path().to_path_buf(),
            home: None,
            ..CompletionSnapshot::default()
        };

        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "cd Pro",
                cursor: 6,
            },
        );

        assert!(items.iter().any(|item| item.replacement == "Projects/"));
    }

    #[test]
    fn cd_completion_excludes_files_and_keeps_directories() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("Projects")).unwrap();
        std::fs::write(temp.path().join("Project.toml"), "fixture").unwrap();
        let snapshot = CompletionSnapshot {
            cwd: temp.path().to_path_buf(),
            home: None,
            ..CompletionSnapshot::default()
        };

        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "cd Pro",
                cursor: 6,
            },
        );

        assert!(items.iter().any(|item| item.replacement == "Projects/"));
        assert!(items.iter().all(|item| item.replacement != "Project.toml"));
        assert!(items
            .iter()
            .all(|item| item.description.as_deref() == Some("directory")));
    }

    #[test]
    fn filesystem_completion_quotes_names_with_spaces() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("Project Files")).unwrap();
        let snapshot = CompletionSnapshot {
            cwd: temp.path().to_path_buf(),
            home: None,
            ..CompletionSnapshot::default()
        };

        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "ls Pro",
                cursor: 6,
            },
        );

        assert!(items
            .iter()
            .any(|item| item.replacement == "\"Project Files/"));
    }

    #[test]
    fn whitespace_inside_open_quotes_stays_in_the_active_completion_token() {
        let line = "ls \"Project Fi";
        let (context, span) = classify(line, line.len());
        assert_eq!(context, CompletionContext::Argument);
        assert_eq!(&line[span], "\"Project Fi");
    }

    #[test]
    fn command_position_after_pipe_completes_commands() {
        let snapshot = CompletionSnapshot::fixture();
        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "printf x | gr",
                cursor: 13,
            },
        );

        assert!(items.iter().any(|item| item.replacement == "grep"));
    }

    #[test]
    fn command_position_can_offer_matching_directory_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("Projects")).unwrap();
        let snapshot = CompletionSnapshot {
            cwd: temp.path().to_path_buf(),
            home: None,
            ..CompletionSnapshot::default()
        };

        let items = complete(
            &snapshot,
            CompletionRequest {
                line: "Pro",
                cursor: 3,
            },
        );

        assert!(items.iter().any(|item| item.replacement == "Projects/"));
    }

    fn complete_in_fixture_dir(line: &str) -> Vec<CompletionItem> {
        use std::os::unix::fs::symlink;
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("a.txt"), "x").unwrap();
        std::fs::write(temp.path().join("b.rs"), "x").unwrap();
        std::fs::create_dir(temp.path().join("dir1")).unwrap();
        symlink(temp.path().join("dir1"), temp.path().join("link_dir")).unwrap();
        symlink(temp.path().join("a.txt"), temp.path().join("link_file")).unwrap();
        let snapshot = CompletionSnapshot {
            cwd: temp.path().to_path_buf(),
            home: None,
            ..CompletionSnapshot::default()
        };
        complete(
            &snapshot,
            CompletionRequest {
                line,
                cursor: line.len(),
            },
        )
    }

    #[test]
    fn cat_offers_files_only() {
        let items = complete_in_fixture_dir("cat ");
        let names: Vec<_> = items.iter().map(|i| i.replacement.as_str()).collect();
        assert!(names.contains(&"a.txt") && names.contains(&"link_file"));
        assert!(!names
            .iter()
            .any(|n| n.starts_with("dir1") || n.starts_with("link_dir")));
    }

    #[test]
    fn cd_offers_directories_only() {
        let items = complete_in_fixture_dir("cd ");
        let names: Vec<_> = items.iter().map(|i| i.replacement.as_str()).collect();
        assert!(
            names.iter().any(|n| n.starts_with("dir1"))
                && names.iter().any(|n| n.starts_with("link_dir"))
        );
        assert!(!names.contains(&"a.txt"));
    }

    #[test]
    fn context_restarts_after_separators_and_marker() {
        for line in [
            "echo x; cat ",
            "true && cat ",
            "ls | cat ",
            "~ cat ",
            "if true { cat ",
            "{ cat ",
            "} ; cat ",
        ] {
            let names: Vec<_> = complete_in_fixture_dir(line)
                .into_iter()
                .map(|i| i.replacement)
                .collect();
            assert!(names.contains(&"a.txt".to_string()), "{line}");
            assert!(!names.iter().any(|n| n.starts_with("dir1")), "{line}");
        }
    }

    #[test]
    fn options_are_skipped_when_finding_the_argument() {
        let names: Vec<_> = complete_in_fixture_dir("cat -n ")
            .into_iter()
            .map(|i| i.replacement)
            .collect();
        assert!(
            names.contains(&"a.txt".to_string()) && !names.iter().any(|n| n.starts_with("dir1"))
        );
    }

    #[test]
    fn quoted_and_home_arguments_keep_working() {
        let names: Vec<_> = complete_in_fixture_dir("cat \"a")
            .into_iter()
            .map(|i| i.replacement)
            .collect();
        assert!(names.iter().any(|n| n.contains("a.txt")));
        let names: Vec<_> = complete_in_fixture_dir("cd \"di")
            .into_iter()
            .map(|i| i.replacement)
            .collect();
        assert!(names.iter().any(|n| n.contains("dir1")));
    }

    #[test]
    fn echo_offers_no_paths() {
        assert!(complete_in_fixture_dir("echo ").is_empty());
    }

    #[test]
    fn quoted_separators_do_not_restart_the_statement() {
        let names: Vec<_> = complete_in_fixture_dir("cat \"x;y\" ")
            .into_iter()
            .map(|i| i.replacement)
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("dir1")));
    }
}
