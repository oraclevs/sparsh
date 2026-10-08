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
    pub kind: Option<ItemKind>,
}

// Several variants are reserved for the menu kind/detail task and unused until then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Command,
    Alias,
    Builtin,
    Function,
    Variable,
    Field,
    Method,
    Struct,
    Enum,
    EnumMember,
    Type,
    Keyword,
    Parameter,
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionContext {
    Command,
    Argument,
    Path,
    Directory,
    File,
    NoPaths,
    SparIdentifier,
    Import,
    /// After `receiver.` in Spar code.
    Member,
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
    /// Everything the Spar session has committed so far, as source text.
    pub(crate) session_source: String,
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
            session_source: String::new(),
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
    if let Some(items) = complete_import_names(snapshot, request.line, cursor) {
        return items;
    }
    if let Some(items) = complete_function_parameters(snapshot, request.line, cursor) {
        return items;
    }
    let (context, span) = member_context(snapshot, request.line, cursor)
        .unwrap_or_else(|| classify(request.line, cursor));
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
        CompletionContext::NoPaths => Vec::new(),
        CompletionContext::File => {
            complete_paths(snapshot, token, span, PathCompletionMode::FilesOnly)
        }
        CompletionContext::SparIdentifier => complete_spar_identifiers(snapshot, token, span),
        CompletionContext::Import => complete_imports(snapshot, token, span),
        CompletionContext::Member => complete_members_at(snapshot, request.line, cursor, span),
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

/// `Some((Member, span))` when the cursor follows `<identifier chain>.` (plus
/// an optional partial member name) in Spar code: a statement `repl_split`
/// calls Spar, or the inside of a `${ }` interpolation.
fn member_context(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
) -> Option<(CompletionContext, Range<usize>)> {
    let prefix = &line[..cursor];
    let start = prefix
        .rfind(|ch: char| !(ch == '_' || ch.is_alphanumeric()))
        .map_or(0, |index| index + prefix[index..].chars().next().map_or(1, char::len_utf8));
    let before = prefix[..start].strip_suffix('.')?;
    // The receiver is a name, or ends in `]` (an index step).
    let last = before.chars().next_back()?;
    if !(last == '_' || last == ']' || last.is_alphanumeric()) {
        return None;
    }
    if before.ends_with('.') {
        return None;
    }
    if !spar_member_statement(snapshot, prefix) {
        return None;
    }
    Some((CompletionContext::Member, start..cursor))
}

/// Whether the statement being typed is Spar (or the cursor is in `${ }`).
fn spar_member_statement(snapshot: &CompletionSnapshot, prefix: &str) -> bool {
    if let Some(open) = prefix.rfind("${") {
        if !prefix[open..].contains('}') {
            return true;
        }
    }
    let names: std::collections::HashSet<String> = snapshot
        .spar_identifiers
        .iter()
        .cloned()
        .chain(snapshot.spar_functions.keys().cloned())
        .collect();
    spar::repl_split::split_statements(prefix, &names)
        .last()
        .is_some_and(|statement| statement.kind == spar::repl_split::ReplKind::Spar)
}

fn complete_members_at(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
    span: Range<usize>,
) -> Vec<CompletionItem> {
    use spar::intel::IntelKind;
    let source = format!("{}\n{line}", snapshot.session_source);
    let offset = snapshot.session_source.len() + 1 + cursor;
    let request = spar::intel::IntelRequest {
        source: &source,
        offset,
        base_dir: &snapshot.cwd,
    };
    spar::intel::complete_members(&request)
        .into_iter()
        .map(|item| CompletionItem {
            replacement: if item.insert_text.is_empty() {
                item.label.clone()
            } else {
                item.insert_text.clone()
            },
            span: span.clone(),
            description: item.detail,
            kind: Some(match item.kind {
                IntelKind::Function => ItemKind::Function,
                IntelKind::Method => ItemKind::Method,
                IntelKind::Variable => ItemKind::Variable,
                IntelKind::Field => ItemKind::Field,
                IntelKind::Struct => ItemKind::Struct,
                IntelKind::Enum => ItemKind::Enum,
                IntelKind::EnumMember => ItemKind::EnumMember,
                IntelKind::Type | IntelKind::Module => ItemKind::Type,
                IntelKind::Keyword => ItemKind::Keyword,
                IntelKind::Parameter => ItemKind::Parameter,
            }),
        })
        .collect()
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
    let statement_is_command =
        parse_statement(statement).is_some_and(|(command, _)| is_known_command(&command));
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
    } else {
        let path = path_like(token);
        match statement_kind(statement) {
            Some(ArgumentKind::Directories) => (CompletionContext::Directory, span),
            Some(ArgumentKind::Files) => (CompletionContext::File, span),
            Some(ArgumentKind::Nothing) if path => (CompletionContext::NoPaths, span),
            _ if path => (CompletionContext::Path, span),
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

/// Command word (basename, env assignments skipped) and its arguments.
fn parse_statement(statement: &str) -> Option<(String, Vec<String>)> {
    let words = split_words(statement);
    let mut iter = words.into_iter().skip_while(|word| is_env_assignment(word));
    let command = iter.next()?;
    let command = command.rsplit('/').next().unwrap_or(&command).to_string();
    Some((command, iter.collect()))
}

fn is_env_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            let mut chars = name.chars();
            chars
                .next()
                .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
                && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// `>`, `>>`, `<`, `2>`, `&>`, `>&` ... with no target attached.
fn is_redirection_operator(word: &str) -> bool {
    let word = word.trim_start_matches(|ch: char| ch.is_ascii_digit());
    matches!(word, ">" | ">>" | "<" | "<<" | "&>" | "&>>" | ">&" | "<&")
}

/// `statement` is the text of the current statement before the active token.
fn statement_kind(statement: &str) -> Option<ArgumentKind> {
    let (command, args) = parse_statement(statement)?;
    if args
        .last()
        .is_some_and(|word| is_redirection_operator(word))
    {
        return Some(ArgumentKind::FilesAndDirectories);
    }
    let args: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|word| !word.contains(['>', '<']))
        .collect();
    Some(argument_kind(&command, positional_index(&args)))
}

/// Text of the current statement that precedes byte offset `end`: starts after the
/// last unquoted `;`, `|`, `&`, `{` or `}` and drops a leading `~ ` shell marker.
fn statement_before(line: &str, end: usize) -> &str {
    let prefix = &line[..end];
    let chars: Vec<(usize, char)> = prefix.char_indices().collect();
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;
    // One entry per open `{`: true when it is a block brace.
    let mut braces: Vec<bool> = Vec::new();
    for (position, &(index, ch)) in chars.iter().enumerate() {
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
                ';' | '|' => start = index + ch.len_utf8(),
                '&' => {
                    let before = position.checked_sub(1).map(|i| chars[i].1);
                    let after = chars.get(position + 1).map(|&(_, c)| c);
                    let redirect =
                        matches!(before, Some('>' | '<')) || matches!(after, Some('>' | '<'));
                    if !redirect {
                        start = index + ch.len_utf8();
                    }
                }
                '{' => {
                    let expansion = position > 0 && chars[position - 1].1 == '$';
                    let rest = &prefix[index + 1..];
                    let body = rest.split('}').next().unwrap_or(rest);
                    let block =
                        !expansion && (body.contains(char::is_whitespace) || body.contains(';'));
                    braces.push(block);
                    if block {
                        start = index + 1;
                    }
                }
                '}' => {
                    if braces.pop().unwrap_or(true) {
                        start = index + 1;
                    }
                }
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
            kind: None,
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
            kind: None,
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
                kind: None,
            });
        }
    }
    for name in &snapshot.aliases {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: name.clone(),
                span: span.clone(),
                description: Some("alias".into()),
                kind: None,
            });
        }
    }
    for name in &snapshot.executables {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: name.clone(),
                span: span.clone(),
                description: Some("external command".into()),
                kind: None,
            });
        }
    }
    for name in snapshot.spar_functions.keys() {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: format!("{name}("),
                span: span.clone(),
                description: Some("Spar function".into()),
                kind: None,
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
            kind: None,
        })
        .collect()
}

/// Cached unfiltered export lists keyed by what the answer depends on.
type ExportKey = (
    bool,
    String,
    PathBuf,
    bool,
    Option<(std::time::SystemTime, u64)>,
);
const EXPORT_CACHE_LIMIT: usize = 16;

#[derive(Default)]
struct ExportState {
    cache: Vec<(ExportKey, Vec<spar::intel::ExportItem>)>,
    in_flight: bool,
}

#[cfg(not(test))]
fn with_export_state<R>(f: impl FnOnce(&mut ExportState) -> R) -> R {
    use std::sync::Mutex;
    static STATE: Mutex<ExportState> = Mutex::new(ExportState {
        cache: Vec::new(),
        in_flight: false,
    });
    f(&mut STATE.lock().unwrap_or_else(|e| e.into_inner()))
}

// Tests run in parallel threads; keep their state per thread.
#[cfg(test)]
fn with_export_state<R>(f: impl FnOnce(&mut ExportState) -> R) -> R {
    thread_local! {
        static STATE: std::cell::RefCell<ExportState> = std::cell::RefCell::new(ExportState::default());
    }
    STATE.with(|state| f(&mut state.borrow_mut()))
}

#[cfg(test)]
thread_local! {
    static EXPORT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Marks an export worker as running; released on every exit path (drop).
struct InFlight;

impl InFlight {
    fn acquire() -> Option<Self> {
        with_export_state(|state| {
            if state.in_flight {
                None
            } else {
                state.in_flight = true;
                Some(InFlight)
            }
        })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        with_export_state(|state| state.in_flight = false);
    }
}

fn cached_exports(
    target: &spar::intel::ImportTarget,
    cwd: &Path,
    type_only: bool,
    budget: std::time::Duration,
) -> Vec<spar::intel::ExportItem> {
    use spar::intel::ImportTarget;
    let (package, name) = match target {
        ImportTarget::File(name) => (false, name),
        ImportTarget::Package(name) => (true, name),
        ImportTarget::Missing => return Vec::new(),
    };
    let stamp = if package {
        None
    } else {
        std::fs::metadata(cwd.join(name))
            .ok()
            .and_then(|meta| Some((meta.modified().ok()?, meta.len())))
    };
    let key: ExportKey = (package, name.clone(), cwd.to_path_buf(), type_only, stamp);
    let hit = with_export_state(|state| {
        state
            .cache
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, items)| items.clone())
    });
    if let Some(items) = hit {
        return items;
    }
    let Some(_guard) = InFlight::acquire() else {
        return Vec::new();
    };
    #[cfg(test)]
    EXPORT_CALLS.with(|calls| calls.set(calls.get() + 1));
    match spar::intel::exports_of_with_budget(
        target,
        cwd,
        type_only,
        &std::collections::HashSet::new(),
        budget,
    ) {
        Ok(items) => {
            with_export_state(|state| {
                state.cache.retain(|(cached, _)| *cached != key);
                if state.cache.len() >= EXPORT_CACHE_LIMIT {
                    state.cache.remove(0);
                }
                state.cache.push((key, items.clone()));
            });
            items
        }
        Err(_) => Vec::new(),
    }
}

/// Names exported by the target of a Spar `import { | } from "x"` statement.
/// `None` when the cursor is not between the braces of a Spar import, so the
/// normal classifier handles the line. Failures and timeouts give no items.
fn complete_import_names(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
) -> Option<Vec<CompletionItem>> {
    let import = spar::intel::import_context(line, cursor)?;
    let exports = cached_exports(
        &import.target,
        &snapshot.cwd,
        import.type_only,
        std::time::Duration::from_millis(500),
    );
    let mut items: Vec<CompletionItem> = exports
        .into_iter()
        .filter(|export| {
            export.name.starts_with(&import.typed) && !import.already.contains(&export.name)
        })
        .map(|export| CompletionItem {
            span: import.replace_start..cursor,
            description: export.detail,
            kind: Some(import_item_kind(export.kind)),
            replacement: export.name,
        })
        .collect();
    items.sort_by(|left, right| left.replacement.cmp(&right.replacement));
    items.dedup_by(|left, right| left.replacement == right.replacement);
    Some(items)
}

fn import_item_kind(kind: spar::intel::ExportKind) -> ItemKind {
    use spar::intel::ExportKind;
    match kind {
        ExportKind::Function | ExportKind::Callable | ExportKind::Group => ItemKind::Function,
        ExportKind::Variable => ItemKind::Variable,
        ExportKind::Struct => ItemKind::Struct,
        ExportKind::Enum => ItemKind::Enum,
        ExportKind::Type => ItemKind::Type,
    }
}

fn complete_imports(
    snapshot: &CompletionSnapshot,
    prefix: &str,
    span: Range<usize>,
) -> Vec<CompletionItem> {
    if path_like(prefix) || prefix.is_empty() || prefix.starts_with(['"', '\'']) {
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
                kind: None,
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
            kind: None,
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

    fn fixture_names(line: &str) -> Vec<String> {
        complete_in_fixture_dir(line)
            .into_iter()
            .map(|i| i.replacement)
            .collect()
    }

    fn only_files(line: &str) {
        let names = fixture_names(line);
        assert!(
            names.iter().any(|n| n.ends_with("a.txt")),
            "{line}: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("dir1")),
            "{line}: {names:?}"
        );
    }

    #[test]
    fn path_like_tokens_still_follow_the_command_kind() {
        only_files("cat ./");
        only_files("cat ./a");
        let names = fixture_names("cd ./");
        assert!(names.iter().any(|n| n.starts_with("./dir1")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("a.txt")), "{names:?}");
        assert!(fixture_names("echo ./x").is_empty());
        assert!(fixture_names("echo ./").is_empty());
        assert_eq!(classify("cat src/", 8).0, CompletionContext::File);
        assert_eq!(classify("cat /etc/", 9).0, CompletionContext::File);
        assert_eq!(classify("cat ~/", 6).0, CompletionContext::File);
        assert_eq!(classify("cd /usr/", 8).0, CompletionContext::Directory);
        assert_eq!(classify("cd ~/", 5).0, CompletionContext::Directory);
        assert_eq!(classify("ls ~/", 5).0, CompletionContext::Path);
    }

    #[test]
    fn cd_and_cat_with_home_prefix_filter_by_kind() {
        let home = tempfile::TempDir::new().unwrap();
        std::fs::write(home.path().join("h.txt"), "x").unwrap();
        std::fs::create_dir(home.path().join("hdir")).unwrap();
        let snapshot = CompletionSnapshot {
            cwd: home.path().to_path_buf(),
            home: Some(home.path().to_path_buf()),
            ..CompletionSnapshot::default()
        };
        let names = |line: &str| -> Vec<String> {
            complete(
                &snapshot,
                CompletionRequest {
                    line,
                    cursor: line.len(),
                },
            )
            .into_iter()
            .map(|i| i.replacement)
            .collect()
        };
        let cd = names("cd ~/");
        assert!(cd.contains(&"~/hdir/".to_string()) && !cd.iter().any(|n| n.contains("h.txt")));
        let cat = names("cat ~/");
        assert!(cat.contains(&"~/h.txt".to_string()) && !cat.iter().any(|n| n.contains("hdir")));
    }

    #[test]
    fn dir_prefixed_arguments_follow_kind() {
        let names = fixture_names("cd dir1/");
        assert!(names.is_empty() || names.iter().all(|n| n.ends_with('/')));
        assert_eq!(classify("cd dir1/", 8).0, CompletionContext::Directory);
        assert_eq!(classify("cat dir1/", 9).0, CompletionContext::File);
    }

    #[test]
    fn braces_in_shell_words_do_not_restart_the_statement() {
        only_files("cat {a,b} ");
        assert_eq!(classify("cat ${HOME}/", 12).0, CompletionContext::File);
        assert_eq!(classify("ls {a,b} x", 10).0, CompletionContext::Argument);
        only_files("if true { cat ");
    }

    #[test]
    fn ampersand_in_redirections_is_not_a_separator() {
        only_files("cat a 2>&1 ");
        only_files("cat a >&2 ");
        only_files("cat a &> out ");
    }

    #[test]
    fn redirection_targets_accept_files_and_directories() {
        let names = fixture_names("cat > ");
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.iter().any(|n| n.starts_with("dir1")));
        let names = fixture_names("cd 2> ");
        assert!(names.contains(&"a.txt".to_string()));
    }

    #[test]
    fn env_prefix_and_command_basename_are_understood() {
        only_files("FOO=bar cat ");
        only_files("A=1 B=2 /bin/cat ");
        only_files("./cat ");
        let names = fixture_names("cd ");
        assert!(!names.contains(&"a.txt".to_string()));
    }

    #[test]
    fn extended_table_entries() {
        for c in [
            "sha1sum",
            "sha512sum",
            "b3sum",
            "nl",
            "tac",
            "strings",
            "file",
            "xxd",
            "od",
        ] {
            only_files(&format!("{c} "));
        }
        for c in ["man", "alias", "export", "unset", "history"] {
            assert!(fixture_names(&format!("{c} ")).is_empty(), "{c}");
        }
    }

    fn temp_dir_with(name: &str, contents: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(name), contents).unwrap();
        dir
    }

    fn snapshot_in(dir: &Path) -> CompletionSnapshot {
        CompletionSnapshot {
            cwd: dir.to_path_buf(),
            home: None,
            ..CompletionSnapshot::default()
        }
    }

    fn complete_at(snapshot: &CompletionSnapshot, line: &str, cursor: usize) -> Vec<CompletionItem> {
        complete(snapshot, CompletionRequest { line, cursor })
    }

    fn import_names(snapshot: &CompletionSnapshot, line: &str, cursor: usize) -> Vec<String> {
        complete_at(snapshot, line, cursor)
            .into_iter()
            .map(|item| item.replacement)
            .collect()
    }

    const LIB: &str = "export var port: int = 80;\nfunction greet(name: str) -> str { return name; };\nvar hidden: int = 1;\n";

    #[test]
    fn import_braces_list_exports_of_the_target_file() {
        let dir = temp_dir_with("lib.spar", LIB);
        let line = "import { ";
        let full = format!("{line}}} from \"lib.spar\";");
        let items = complete_at(&snapshot_in(dir.path()), &full, line.len());
        let names: Vec<_> = items.iter().map(|i| i.replacement.as_str()).collect();
        assert!(names.contains(&"port") && names.contains(&"greet"), "{names:?}");
        assert!(!names.contains(&"hidden"), "{names:?}");
        let port = items.iter().find(|i| i.replacement == "port").unwrap();
        assert_eq!(port.kind, Some(ItemKind::Variable));
        assert!(port.description.as_deref().is_some_and(|d| d.contains("int")));
        let greet = items.iter().find(|i| i.replacement == "greet").unwrap();
        assert_eq!(greet.kind, Some(ItemKind::Function));
        assert_eq!(greet.span, line.len()..line.len());
    }

    #[test]
    fn import_braces_filter_by_typed_prefix_and_skip_listed_names() {
        let dir = temp_dir_with(
            "lib.spar",
            "export var port: int = 80;\nexport var pool: int = 1;\n",
        );
        let full = "import { port, po } from \"lib.spar\";";
        let names = import_names(&snapshot_in(dir.path()), full, "import { port, po".len());
        assert_eq!(names, vec!["pool"]);
    }

    #[test]
    fn import_braces_with_missing_target_return_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snapshot_in(dir.path());
        assert!(import_names(&snap, "import { } from \"nope.spar\";", "import { ".len()).is_empty());
        assert!(import_names(&snap, "import { ", "import { ".len()).is_empty());
        std::fs::create_dir(dir.path().join("d")).unwrap();
        assert!(import_names(&snap, "import { } from \"d\";", "import { ".len()).is_empty());
    }

    #[test]
    fn import_braces_work_across_lines_and_for_packages() {
        let dir = temp_dir_with("lib.spar", LIB);
        let snap = snapshot_in(dir.path());
        let full = "var a: int = 1;\nimport {\n  gr\n} from \"lib.spar\";";
        let cursor = full.find("gr").unwrap() + 2;
        assert_eq!(import_names(&snap, full, cursor), vec!["greet"]);
        let pkg = "import pkg { } from \"std/fs\";";
        assert!(!import_names(&snap, pkg, "import pkg { ".len()).is_empty());
    }

    #[test]
    fn import_path_after_from_still_completes_files() {
        let dir = temp_dir_with("lib.spar", LIB);
        let snap = snapshot_in(dir.path());
        let line = "import { port } from \"li";
        assert_eq!(import_names(&snap, line, line.len()), vec!["\"lib.spar\""]);
        let line = "import \"li";
        assert_eq!(import_names(&snap, line, line.len()), vec!["\"lib.spar\""]);
        let line = "import { port } from \"./li";
        assert_eq!(import_names(&snap, line, line.len()), vec!["\"./lib.spar\""]);
    }

    #[test]
    fn ordinary_lines_mentioning_import_do_not_trigger_name_completion() {
        let dir = temp_dir_with("lib.spar", LIB);
        let snap = snapshot_in(dir.path());
        for line in [
            "echo import { ",
            "~ import { ",
            "ls | import { ",
            "import -window root { ",
        ] {
            let names = import_names(&snap, line, line.len());
            assert!(!names.contains(&"port".to_string()), "{line}: {names:?}");
            assert!(!names.contains(&"greet".to_string()), "{line}: {names:?}");
        }
        let full = "echo import { } from \"lib.spar\"";
        let names = import_names(&snap, full, "echo import { ".len());
        assert!(!names.contains(&"port".to_string()), "{names:?}");
    }

    #[test]
    fn existing_producers_have_no_kind() {
        let items = complete_at(&CompletionSnapshot::fixture(), "p", 1);
        assert!(!items.is_empty());
        assert!(items.iter().all(|item| item.kind.is_none()));
    }

    fn calls() -> usize {
        EXPORT_CALLS.with(|c| c.get())
    }

    fn pkg_names(line: &str, cursor: usize, dir: &Path) -> Vec<String> {
        import_names(&snapshot_in(dir), line, cursor)
    }

    #[test]
    fn repeated_and_longer_prefixes_reuse_the_cached_export_list() {
        let dir = temp_dir_with("lib.spar", LIB);
        let snap = snapshot_in(dir.path());
        let before = calls();
        let l1 = "import { } from \"lib.spar\";";
        assert_eq!(import_names(&snap, l1, 9).len(), 2);
        assert_eq!(import_names(&snap, l1, 9).len(), 2);
        let l2 = "import { gr } from \"lib.spar\";";
        assert_eq!(import_names(&snap, l2, 11), vec!["greet"]);
        let l3 = "import { gre } from \"lib.spar\";";
        assert_eq!(import_names(&snap, l3, 12), vec!["greet"]);
        assert_eq!(calls() - before, 1);
    }

    #[test]
    fn editing_the_target_invalidates_the_cache() {
        let dir = temp_dir_with("lib.spar", LIB);
        let snap = snapshot_in(dir.path());
        let line = "import { } from \"lib.spar\";";
        assert_eq!(import_names(&snap, line, 9).len(), 2);
        std::fs::write(
            dir.path().join("lib.spar"),
            format!("{LIB}export var extra_name: int = 3;\n"),
        )
        .unwrap();
        let names = import_names(&snap, line, 9);
        assert!(names.contains(&"extra_name".to_string()), "{names:?}");
    }

    #[test]
    fn a_timeout_is_not_cached_and_a_busy_worker_returns_nothing() {
        let dir = temp_dir_with("lib.spar", LIB);
        let target = spar::intel::ImportTarget::File("lib.spar".into());
        let timed_out = cached_exports(&target, dir.path(), false, std::time::Duration::ZERO);
        assert!(timed_out.is_empty());
        with_export_state(|s| assert!(s.cache.is_empty() && !s.in_flight));
        assert_eq!(cached_exports(&target, dir.path(), false, std::time::Duration::from_millis(500)).len(), 2);
        // A running worker blocks a new uncached lookup but not a cached one.
        let guard = InFlight::acquire().unwrap();
        let other = spar::intel::ImportTarget::File("other.spar".into());
        let before = calls();
        assert!(cached_exports(&other, dir.path(), false, std::time::Duration::from_millis(500)).is_empty());
        assert_eq!(calls(), before);
        assert_eq!(cached_exports(&target, dir.path(), false, std::time::Duration::from_millis(500)).len(), 2);
        drop(guard);
        with_export_state(|s| assert!(!s.in_flight));
    }

    #[test]
    fn the_cache_is_bounded() {
        let dir = temp_dir_with("lib.spar", LIB);
        for n in 0..(EXPORT_CACHE_LIMIT + 5) {
            std::fs::write(dir.path().join(format!("m{n}.spar")), "export var a: int = 1;\n").unwrap();
            let line = format!("import {{ }} from \"m{n}.spar\";");
            assert_eq!(pkg_names(&line, 9, dir.path()), vec!["a"]);
        }
        with_export_state(|s| assert!(s.cache.len() <= EXPORT_CACHE_LIMIT));
    }

    #[test]
    fn parse_error_cyclic_and_huge_libraries_do_not_panic() {
        let dir = temp_dir_with("bad.spar", "export var ok: int = 1;\nexport var = = ;\n");
        let names = pkg_names("import { } from \"bad.spar\";", 9, dir.path());
        assert!(names.is_empty() || names.contains(&"ok".to_string()), "{names:?}");
        std::fs::write(dir.path().join("a.spar"), "import { y } from \"b.spar\";\nexport var x: int = 1;\n").unwrap();
        std::fs::write(dir.path().join("b.spar"), "import { x } from \"a.spar\";\nexport var y: int = 1;\n").unwrap();
        let names = pkg_names("import { } from \"a.spar\";", 9, dir.path());
        assert!(names.is_empty() || names.contains(&"x".to_string()), "{names:?}");
        let big: String = (0..2000).map(|i| format!("export var name{i}: int = {i};\n")).collect();
        std::fs::write(dir.path().join("big.spar"), big).unwrap();
        let started = std::time::Instant::now();
        let names = pkg_names("import { } from \"big.spar\";", 9, dir.path());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(names.is_empty() || names.len() == 2000, "{}", names.len());
    }

    #[test]
    fn cursor_inside_a_multibyte_character_gives_nothing() {
        let dir = temp_dir_with("lib.spar", LIB);
        let line = "import { \u{e9}x } from \"lib.spar\";";
        let mid = "import { ".len() + 1;
        assert!(!line.is_char_boundary(mid));
        assert!(pkg_names(line, mid, dir.path()).is_empty());
    }

    #[test]
    fn import_detection_edge_cases() {
        let dir = temp_dir_with("lib.spar", LIB);
        let fires = |line: &str, cursor: usize| pkg_names(line, cursor, dir.path()).contains(&"port".to_string());
        let full = "import{ } from \"lib.spar\";";
        assert!(fires(full, "import{ ".len()));
        let after = "import { port } from \"lib.spar\";";
        assert!(!fires(after, after.len()));
        assert!(!fires(after, "import { port }".len()));
        let a = "x = 1; import { } from \"lib.spar\";";
        assert!(fires(a, "x = 1; import { ".len()));
        let b = "echo a; import { } from \"lib.spar\";";
        assert!(fires(b, "echo a; import { ".len()));
        let c = "echo import { x } from \"lib.spar\"";
        assert!(!fires(c, "echo import { ".len()));
        let d = "~ echo import { } from \"lib.spar\"";
        assert!(!fires(d, "~ echo import { ".len()));
    }

    #[test]
    fn std_fs_package_lists_a_known_export() {
        let dir = tempfile::tempdir().unwrap();
        let line = "import pkg { wr } from \"std/fs\";";
        let names = pkg_names(line, "import pkg { wr".len(), dir.path());
        assert!(names.contains(&"writeText".to_string()), "{names:?}");
    }

    // ── Member completion ──────────────────────────────────────────────────

    const SESSION: &str = "struct P { name: str = \"\"; port: int = 0; };\nvar p: P = P();";

    fn member_snapshot(session: &str) -> CompletionSnapshot {
        let mut snapshot = CompletionSnapshot::default();
        snapshot.cwd = std::env::temp_dir();
        snapshot.session_source = session.to_string();
        snapshot.spar_identifiers = BTreeSet::from(["P".into(), "p".into(), "s".into(), "q".into()]);
        snapshot
    }

    fn member_items(session: &str, line: &str) -> Vec<CompletionItem> {
        complete(
            &member_snapshot(session),
            CompletionRequest { line, cursor: line.len() },
        )
    }

    fn member_names(session: &str, line: &str) -> Vec<String> {
        member_items(session, line)
            .into_iter()
            .map(|item| item.replacement)
            .collect()
    }

    #[test]
    fn dot_after_a_spar_variable_offers_its_fields() {
        let names = member_names(SESSION, "p.");
        assert!(names.contains(&"name".to_string()), "{names:?}");
        assert!(names.contains(&"port".to_string()), "{names:?}");
        let item = member_items(SESSION, "p.")
            .into_iter()
            .find(|item| item.replacement == "port")
            .unwrap();
        assert_eq!(item.kind, Some(ItemKind::Field));
        assert_eq!(item.description.as_deref(), Some("int"));
        assert_eq!(item.span, 2..2);
    }

    #[test]
    fn partial_member_name_filters_and_spans_the_partial() {
        let items = member_items(SESSION, "p.na");
        assert_eq!(
            items.iter().map(|i| i.replacement.as_str()).collect::<Vec<_>>(),
            vec!["name"]
        );
        assert_eq!(items[0].span, 2..4);
    }

    #[test]
    fn methods_are_labelled_as_methods() {
        let items = member_items("var s: str = \"x\";", "s.len");
        let method = items.iter().find(|item| item.replacement == "length").unwrap();
        assert_eq!(method.kind, Some(ItemKind::Method));
    }

    #[test]
    fn members_work_inside_larger_spar_statements_and_interpolation() {
        assert!(member_names(SESSION, "var n = p.").contains(&"name".to_string()));
        assert!(member_names(SESSION, "if p.").contains(&"port".to_string()));
        let line = "echo ${p.";
        assert!(member_names(SESSION, line).contains(&"name".to_string()));
    }

    #[test]
    fn dot_on_a_command_line_offers_no_members() {
        let names = member_names(SESSION, "echo p.");
        assert!(!names.contains(&"name".to_string()) && !names.contains(&"port".to_string()), "{names:?}");
        assert!(member_names(SESSION, "ls -l p.").iter().all(|n| n != "name"));
    }

    #[test]
    fn multi_line_session_source_keeps_offsets_aligned() {
        let session = format!(
            "// header \u{e9}\nfn helper() -> int {{\n    return 1;\n}};\n\n{SESSION}\nvar q: int = 2;"
        );
        let names = member_names(&session, "p.po");
        assert_eq!(names, vec!["port"]);
        let line = "var r = p.";
        assert!(member_names(&session, line).contains(&"name".to_string()));
    }

    #[test]
    fn empty_session_and_unknown_receivers_are_empty() {
        assert!(member_names("", "p.").is_empty());
        assert!(member_names(SESSION, "nothing.").is_empty());
    }

    #[test]
    fn cursor_in_the_middle_of_a_multibyte_line_does_not_panic() {
        let line = "var \u{e9} = p.na";
        for cursor in 0..=line.len() {
            let _ = complete(
                &member_snapshot(SESSION),
                CompletionRequest { line, cursor },
            );
        }
    }
}
