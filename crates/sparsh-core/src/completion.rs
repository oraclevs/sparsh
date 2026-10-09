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
    /// After `z`: directories from the visit history, best first.
    Frecent,
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
    /// Names of `async` functions: what `await` can be followed by.
    pub(crate) async_functions: BTreeSet<String>,
    /// Directories the shell has entered, for `z` completion.
    pub(crate) visited_dirs: Vec<crate::DirVisit>,
    /// Short names from `z --set`.
    pub(crate) dir_aliases: Vec<crate::DirAlias>,
    /// Everything the Spar session has committed so far, as source text.
    pub(crate) session_source: String,
    /// Registered theme names (and descriptions), for `theme set`.
    pub(crate) theme_names: Vec<(String, Option<String>)>,
}

impl CompletionSnapshot {
    /// A snapshot that knows only a Spar session: its source and the names of
    /// the functions it declares. For embedders and tests.
    pub fn for_session(cwd: PathBuf, session_source: &str, functions: &[(&str, &[&str])]) -> Self {
        let mut snapshot = Self { cwd, ..Self::default() };
        snapshot.session_source = session_source.to_string();
        for (name, params) in functions {
            snapshot.spar_identifiers.insert((*name).to_string());
            snapshot
                .spar_functions
                .insert((*name).to_string(), params.iter().map(|p| (*p).to_string()).collect());
        }
        snapshot
    }

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
            visited_dirs: Vec::new(),
            dir_aliases: Vec::new(),
            async_functions: BTreeSet::new(),
            theme_names: Vec::new(),
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
    if let Some(items) = complete_theme_arguments(snapshot, request.line, cursor) {
        return items;
    }
    if let Some(items) = complete_pipeline_position(snapshot, request.line, cursor) {
        return items;
    }
    if let Some(items) = complete_function_parameters(snapshot, request.line, cursor) {
        return items;
    }
    let (context, span) = member_context(snapshot, request.line, cursor)
        .or_else(|| interpolation_context(request.line, cursor))
        .unwrap_or_else(|| classify(request.line, cursor));
    let token = &request.line[span.clone()];
    let keep_order = context == CompletionContext::Frecent;
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
        CompletionContext::Frecent => complete_frecent(snapshot, request.line, token, span),
        CompletionContext::NoPaths => Vec::new(),
        CompletionContext::File => {
            complete_paths(snapshot, token, span, PathCompletionMode::FilesOnly)
        }
        CompletionContext::SparIdentifier => {
            complete_scope_names(snapshot, request.line, cursor, token, span)
        }
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
    if !keep_order {
        items.sort_by(|left, right| {
        let left_exact = left.replacement == token;
        let right_exact = right.replacement == token;
        right_exact
            .cmp(&left_exact)
            .then_with(|| left.replacement.cmp(&right.replacement))
        });
    }
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
        .rfind(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .map_or(0, |index| index + prefix[index..].chars().next().map_or(1, char::len_utf8));
    let before = prefix[..start].strip_suffix('.')?;
    // The receiver is a name, or ends in `]` (an index step).
    let last = before.chars().next_back()?;
    // Spar identifiers are ASCII (see the lexer), so `p.n\u{e9}` is no member access.
    if !(last == '_' || last == ']' || last.is_ascii_alphanumeric()) {
        return None;
    }
    if before.ends_with('.') {
        return None;
    }
    // `3.` is a number being typed, not a receiver.
    let receiver_start = before
        .rfind(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .map_or(0, |index| index + before[index..].chars().next().map_or(1, char::len_utf8));
    if receiver_start < before.len() && before[receiver_start..].bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if !spar_member_statement(snapshot, prefix) {
        return None;
    }
    Some((CompletionContext::Member, start..cursor))
}

/// Inside an open `${ }` of a command line the word at the cursor is a Spar
/// name, whatever the surrounding command is.
fn interpolation_context(line: &str, cursor: usize) -> Option<(CompletionContext, Range<usize>)> {
    let prefix = &line[..cursor];
    let open = prefix.rfind("${")?;
    if prefix[open..].contains('}') {
        return None;
    }
    let start = prefix
        .rfind(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .map_or(0, |index| index + prefix[index..].chars().next().map_or(1, char::len_utf8));
    Some((CompletionContext::SparIdentifier, start..cursor))
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

fn item_kind(kind: spar::intel::IntelKind) -> ItemKind {
    use spar::intel::IntelKind;
    match kind {
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
    }
}

fn complete_members_at(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
    span: Range<usize>,
) -> Vec<CompletionItem> {
    let Some(analysis) = session_analysis(&snapshot.session_source, &snapshot.cwd) else {
        return Vec::new();
    };
    let source = format!("{}\n{line}", snapshot.session_source);
    let offset = snapshot.session_source.len() + 1 + cursor;
    spar::intel::complete_members_with(&analysis, &source, offset)
        .into_iter()
        .map(|item| CompletionItem {
            replacement: if item.insert_text.is_empty() {
                item.label.clone()
            } else {
                item.insert_text.clone()
            },
            span: span.clone(),
            description: item.detail,
            kind: Some(item_kind(item.kind)),
        })
        .collect()
}

/// The session source is compiled once and reused while it stays the same.
const MEMBER_CACHE_LIMIT: usize = 4;
type MemberKey = (u64, usize, PathBuf);

#[derive(Default)]
struct MemberCache {
    entries: Vec<(MemberKey, std::sync::Arc<spar::intel::MemberAnalysis>)>,
}

#[cfg(not(test))]
fn with_member_cache<R>(f: impl FnOnce(&mut MemberCache) -> R) -> R {
    use std::sync::Mutex;
    static STATE: Mutex<MemberCache> = Mutex::new(MemberCache { entries: Vec::new() });
    f(&mut STATE.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(test)]
fn with_member_cache<R>(f: impl FnOnce(&mut MemberCache) -> R) -> R {
    thread_local! {
        static STATE: std::cell::RefCell<MemberCache> = std::cell::RefCell::new(MemberCache::default());
    }
    STATE.with(|state| f(&mut state.borrow_mut()))
}

#[cfg(test)]
thread_local! {
    static MEMBER_ANALYSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn session_analysis(source: &str, base_dir: &Path) -> Option<std::sync::Arc<spar::intel::MemberAnalysis>> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    let key: MemberKey = (hasher.finish(), source.len(), base_dir.to_path_buf());
    if let Some(hit) = with_member_cache(|cache| {
        cache.entries.iter().find(|(k, _)| *k == key).map(|(_, a)| std::sync::Arc::clone(a))
    }) {
        return Some(hit);
    }
    #[cfg(test)]
    MEMBER_ANALYSES.with(|count| count.set(count.get() + 1));
    let analysis = std::sync::Arc::new(spar::intel::analyze_session(source, base_dir)?);
    with_member_cache(|cache| {
        cache.entries.retain(|(k, _)| *k != key);
        if cache.entries.len() >= MEMBER_CACHE_LIMIT {
            cache.entries.remove(0);
        }
        cache.entries.push((key, std::sync::Arc::clone(&analysis)));
    });
    Some(analysis)
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
            Some(ArgumentKind::Frecent) => (CompletionContext::Frecent, span),
            Some(ArgumentKind::Files) => (CompletionContext::File, span),
            Some(ArgumentKind::Nothing) if path => (CompletionContext::NoPaths, span),
            _ if path => (CompletionContext::Path, span),
            _ => (CompletionContext::Argument, span),
        }
    }
}

/// Completion by what precedes the word: after `await` the async functions,
/// after `|>` the functions that can be a stage, after `| from` / `|> from`
/// the decoders, after `| to` / `|> to` the encoders. `None` elsewhere.
fn complete_pipeline_position(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
) -> Option<Vec<CompletionItem>> {
    let prefix = &line[..cursor];
    let start = active_token_start(prefix);
    let token = &prefix[start..];
    let before = prefix[..start].trim_end();
    // Only when whitespace separates the word from what precedes it.
    if before.len() == prefix[..start].len() && !before.is_empty() {
        return None;
    }
    let span = start..cursor;
    let last_word = before
        .rsplit(|c: char| c.is_whitespace() || c == '|' || c == '>')
        .next()
        .unwrap_or("");
    let function_items = |names: Vec<&String>, tag: &str| -> Vec<CompletionItem> {
        names
            .into_iter()
            .filter(|name| name.starts_with(token) && !name.starts_with("Sparsh"))
            .map(|name| {
                let params = snapshot.spar_functions.get(name).map(|p| p.join(", ")).unwrap_or_default();
                CompletionItem {
                    replacement: name.clone(),
                    span: span.clone(),
                    description: Some(format!("{tag}({params})")),
                    kind: Some(ItemKind::Function),
                }
            })
            .collect()
    };
    let after_pipe = |text: &str| text.trim_end().ends_with('|') || text.trim_end().ends_with("|>");
    let items = if last_word == "await" && before.ends_with("await") {
        function_items(snapshot.async_functions.iter().collect(), "async ")
    } else if before.ends_with("|>") {
        function_items(snapshot.spar_functions.keys().collect(), "")
    } else if (last_word == "from" || last_word == "to") && before.ends_with(last_word)
        && after_pipe(&before[..before.len() - last_word.len()])
    {
        let mut formats: Vec<(String, String)> = Vec::new();
        if last_word == "from" {
            for descriptor in spar::StructuredInputRegistry::builtin().descriptors() {
                formats.push((descriptor.name.clone(), descriptor.description.clone()));
            }
        } else {
            for descriptor in spar::StructuredFormatRegistry::builtin().descriptors() {
                formats.push((descriptor.name().to_string(), "encode as this format".to_string()));
            }
        }
        formats.sort();
        formats.dedup_by(|a, b| a.0 == b.0);
        formats
            .into_iter()
            .filter(|(name, _)| name.starts_with(token))
            .map(|(name, description)| CompletionItem {
                replacement: name,
                span: span.clone(),
                description: Some(description),
                kind: Some(ItemKind::Keyword),
            })
            .collect()
    } else {
        return None;
    };
    (!items.is_empty()).then_some(items)
}

/// `z` completion: remembered directories matching every word typed so far,
/// best first. The replacement covers all of `z`'s words, so the result is a
/// plain path that `z` jumps to directly.
fn complete_frecent(
    snapshot: &CompletionSnapshot,
    line: &str,
    token: &str,
    span: Range<usize>,
) -> Vec<CompletionItem> {
    if token.starts_with('-') {
        return [
            ("--set", "give a directory a short name"),
            ("--unset", "remove a short name"),
            ("--list", "list short names"),
        ]
        .into_iter()
        .filter(|(flag, _)| flag.starts_with(token))
        .map(|(flag, help)| CompletionItem {
            replacement: flag.into(),
            span: span.clone(),
            description: Some(help.into()),
            kind: Some(ItemKind::Keyword),
        })
        .collect();
    }
    let statement = statement_before(line, span.start);
    let mut words: Vec<String> = parse_statement(statement)
        .map(|(_, args)| args.into_iter().filter(|word| !word.starts_with('-')).collect())
        .unwrap_or_default();
    let path_only = words.is_empty() && path_like(token);
    let path_span = span.clone();
    let from = if words.is_empty() {
        span.start
    } else {
        let base = statement.as_ptr() as usize - line.as_ptr() as usize;
        let lead = statement.len() - statement.trim_start().len();
        let after_command = statement[lead..]
            .find(char::is_whitespace)
            .map_or(statement.len(), |index| lead + index);
        let rest = &statement[after_command..];
        base + after_command + (rest.len() - rest.trim_start().len())
    };
    // A path already inserted by an earlier Tab narrows by prefix instead of by name.
    let prefix = if token.starts_with('/') || token.starts_with('~') {
        let expanded = match (token.strip_prefix('~'), snapshot.home.as_deref()) {
            (Some(rest), Some(home)) => format!("{}{rest}", home.display()),
            _ => token.to_string(),
        };
        words.clear();
        Some(expanded)
    } else {
        if !token.is_empty() {
            words.push(token.to_string());
        }
        None
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut ranked = crate::zdir::rank(
        &snapshot.visited_dirs,
        &words,
        now,
        // The menu lists every known directory, the current one included;
        // only a jump skips it.
        None,
        &snapshot.dir_aliases,
    );
    // Aliased directories are always offered, even before they were visited.
    if words.is_empty() && prefix.is_none() {
        for (_, path) in &snapshot.dir_aliases {
            if !ranked.contains(path) {
                ranked.insert(0, path.clone());
            }
        }
    }
    let items: Vec<CompletionItem> = ranked
        .into_iter()
        .filter(|path| prefix.as_ref().map_or(true, |prefix| path.to_string_lossy().starts_with(prefix.as_str())))
        .filter(|path| path.is_dir())
        .take(200)
        .map(|path| {
            let shown = match snapshot.home.as_deref().and_then(|home| path.strip_prefix(home).ok()) {
                Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
                Some(rest) => format!("~/{}", rest.display()),
                None => path.display().to_string(),
            };
            // Two columns in the menu: a short name to type, and where it goes.
            // An ambiguous folder name falls back to the full path.
            let name = match &prefix {
                Some(_) => None,
                None => crate::zdir::short_name(&path, &snapshot.dir_aliases, &snapshot.visited_dirs),
            };
            CompletionItem {
                replacement: quote_completion(name.as_deref().unwrap_or(&shown), None, false),
                span: from..span.end,
                description: Some(shown),
                kind: Some(ItemKind::Directory),
            }
        })
        .collect();
    // Nothing remembered under that path: offer the directories on disk instead.
    if items.is_empty() && path_only {
        return complete_paths(snapshot, token, path_span, PathCompletionMode::DirectoriesOnly);
    }
    items
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
                '}' if braces.pop().unwrap_or(true) => {
                    start = index + 1;
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
    let signature = call_signature(snapshot, line, cursor).filter(|info| info.name == function);
    let params: Vec<String> = match snapshot.spar_functions.get(function) {
        Some(names) => names.clone(),
        None => signature.as_ref()?.params.iter().map(|p| p.name.clone()).collect(),
    };
    if params.is_empty() {
        return Some(Vec::new());
    }
    let describe = |name: &str| -> String {
        signature
            .as_ref()
            .and_then(|info| info.params.iter().find(|p| p.name == name))
            .map(|p| match &p.default {
                Some(default) => format!("{} = {default}", p.ty),
                None => p.ty.clone(),
            })
            .unwrap_or_else(|| format!("parameter of {function}"))
    };

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
            kind: Some(ItemKind::Parameter),
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
            description: Some(describe(name)),
            kind: Some(ItemKind::Parameter),
        })
        .collect();
    Some(items)
}

/// The signature of the Spar call the cursor is inside, from the cached
/// session analysis. `None` outside a call, in a command line, or for a
/// callee the session does not know.
fn call_signature(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
) -> Option<spar::intel::SignatureInfo> {
    let mut cursor = cursor.min(line.len());
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let prefix = &line[..cursor];
    unmatched_call_open(prefix)?;
    if !spar_member_statement(snapshot, prefix) {
        return None;
    }
    let analysis = session_analysis(&snapshot.session_source, &snapshot.cwd)?;
    let source = format!("{}\n{line}", snapshot.session_source);
    let offset = snapshot.session_source.len() + 1 + cursor;
    spar::intel::signature_at_with(&analysis, &source, offset)
}

/// A call signature for display and the parameter the cursor is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHint {
    /// `build(profile: str, release: bool = false)`.
    pub text: String,
    /// Index of the active parameter.
    pub active: usize,
    /// Byte range of the active parameter inside `text`.
    pub active_range: Range<usize>,
}

/// The signature of the call around the cursor with the active parameter
/// located, or `None` when the cursor is not inside a known call.
pub fn signature_hint_info(snapshot: &CompletionSnapshot, line: &str, cursor: usize) -> Option<SignatureHint> {
    let info = call_signature(snapshot, line, cursor)?;
    let prefix = if info.is_async { "async " } else { "" };
    let mut text = format!("{prefix}{}(", info.name);
    let mut active_range = text.len()..text.len();
    for (index, param) in info.params.iter().enumerate() {
        if index > 0 {
            text.push_str(", ");
        }
        let start = text.len();
        text.push_str(&param.label());
        if index == info.active_param {
            active_range = start..text.len();
        }
    }
    text.push(')');
    Some(SignatureHint { text, active: info.active_param, active_range })
}

/// `build(profile: str, release: bool = false)` for the call around the
/// cursor; see [`signature_hint_info`] for the active parameter.
pub fn signature_hint(snapshot: &CompletionSnapshot, line: &str, cursor: usize) -> Option<String> {
    signature_hint_info(snapshot, line, cursor).map(|hint| hint.text)
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
                kind: Some(ItemKind::Builtin),
            });
        }
    }
    for name in &snapshot.aliases {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: name.clone(),
                span: span.clone(),
                description: Some("alias".into()),
                kind: Some(ItemKind::Alias),
            });
        }
    }
    for name in &snapshot.executables {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: name.clone(),
                span: span.clone(),
                description: Some("external command".into()),
                kind: Some(ItemKind::Command),
            });
        }
    }
    for name in snapshot.spar_functions.keys() {
        if name.starts_with(prefix) {
            items.push(CompletionItem {
                replacement: format!("{name}("),
                span: span.clone(),
                description: Some("Spar function".into()),
                kind: Some(ItemKind::Function),
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
            kind: Some(ItemKind::Variable),
        })
        .collect()
}

/// Names visible at the cursor (from the cached session analysis) merged with
/// the snapshot identifiers; the scope items win on a shared label.
fn complete_scope_names(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
    prefix: &str,
    span: Range<usize>,
) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = Vec::new();
    if let Some(analysis) = session_analysis(&snapshot.session_source, &snapshot.cwd) {
        let source = format!("{}\n{line}", snapshot.session_source);
        let offset = snapshot.session_source.len() + 1 + cursor;
        items = spar::intel::complete_scope_with(&analysis, &source, offset)
            .into_iter()
            .map(|item| CompletionItem {
                replacement: if item.insert_text.is_empty() {
                    item.label.clone()
                } else {
                    item.insert_text.clone()
                },
                span: span.clone(),
                description: item.detail,
                kind: Some(item_kind(item.kind)),
            })
            .collect();
    }
    let known: std::collections::HashSet<String> =
        items.iter().map(|item| item.replacement.clone()).collect();
    items.extend(
        complete_spar_identifiers(snapshot, prefix, span)
            .into_iter()
            .filter(|item| !known.contains(&item.replacement)),
    );
    items
}

/// What an export list depends on: (package, name, cwd, type_only).
type ExportKey = (bool, String, PathBuf, bool);
/// Resolved file, modification time and length of an export list's source.
type ExportStamp = Option<(PathBuf, std::time::SystemTime, u64)>;
const EXPORT_CACHE_LIMIT: usize = 16;
/// Slow targets get this long in the background worker; the prompt never waits for it.
const EXPORT_WORKER_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
/// Transitively imported files are not stamped, so entries also expire.
#[cfg(not(test))]
const EXPORT_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_EXPORT_WORKERS: usize = 4;

struct ExportEntry {
    key: ExportKey,
    stamp: ExportStamp,
    stored: std::time::Instant,
    items: Vec<spar::intel::ExportItem>,
}

#[derive(Default)]
struct ExportState {
    cache: Vec<ExportEntry>,
    in_flight: Vec<ExportKey>,
}

type SharedExportState = std::sync::Arc<std::sync::Mutex<ExportState>>;

#[cfg(not(test))]
fn export_state() -> SharedExportState {
    static STATE: std::sync::OnceLock<SharedExportState> = std::sync::OnceLock::new();
    STATE.get_or_init(Default::default).clone()
}

// Tests run in parallel threads; keep their state per thread (workers get a clone).
#[cfg(test)]
fn export_state() -> SharedExportState {
    thread_local! {
        static STATE: SharedExportState = Default::default();
    }
    STATE.with(Clone::clone)
}

fn with_export_state<R>(f: impl FnOnce(&mut ExportState) -> R) -> R {
    let state = export_state();
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

#[cfg(not(test))]
fn export_ttl() -> std::time::Duration {
    EXPORT_TTL
}

#[cfg(test)]
thread_local! {
    static EXPORT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EXPORT_TTL_OVERRIDE: std::cell::Cell<Option<std::time::Duration>> = const { std::cell::Cell::new(None) };
    static EXPORT_WORKER_HOOK: std::cell::RefCell<Option<std::sync::Arc<dyn Fn() + Send + Sync>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn export_ttl() -> std::time::Duration {
    EXPORT_TTL_OVERRIDE.with(|ttl| ttl.get()).unwrap_or(std::time::Duration::from_secs(5))
}

/// Marks an export worker as running for one key. Owned (and dropped) by the
/// worker thread, so it is released on success, error and panic alike.
struct InFlight {
    state: SharedExportState,
    key: ExportKey,
}

impl InFlight {
    fn acquire(key: &ExportKey) -> Option<Self> {
        let state = export_state();
        {
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.in_flight.contains(key) || guard.in_flight.len() >= MAX_EXPORT_WORKERS {
                return None;
            }
            guard.in_flight.push(key.clone());
        }
        Some(InFlight { state, key: key.clone() })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = guard.in_flight.iter().position(|key| *key == self.key) {
            guard.in_flight.remove(index);
        }
    }
}

fn export_stamp(path: Option<PathBuf>) -> ExportStamp {
    let path = path?;
    let meta = std::fs::metadata(&path).ok()?;
    Some((path, meta.modified().ok()?, meta.len()))
}

/// Export names of `target`, cached. A miss runs the compile on a background
/// worker (which owns the in-flight guard and fills the cache even after this
/// call has given up); this call waits at most `wait` and returns nothing on a
/// timeout, so a later Tab finds the cache filled.
fn cached_exports(
    target: &spar::intel::ImportTarget,
    cwd: &Path,
    type_only: bool,
    wait: std::time::Duration,
) -> Vec<spar::intel::ExportItem> {
    use spar::intel::ImportTarget;
    let (package, name) = match target {
        ImportTarget::File(name) => (false, name),
        ImportTarget::Package(name) => (true, name),
        ImportTarget::Missing => return Vec::new(),
    };
    let key: ExportKey = (package, name.clone(), cwd.to_path_buf(), type_only);
    let stamp = export_stamp(spar::intel::resolve_import_path(target, cwd));
    let ttl = export_ttl();
    // `Some((items, fresh))` when an entry for this key and file state exists.
    let cached = with_export_state(|state| {
        state
            .cache
            .iter()
            .find(|entry| entry.key == key && entry.stamp == stamp)
            .map(|entry| (entry.items.clone(), entry.stored.elapsed() < ttl))
    });
    if let Some((items, true)) = &cached {
        return items.clone();
    }
    // An expired entry for an unchanged file is better than nothing while refreshing.
    let stale = cached.map(|(items, _)| items).unwrap_or_default();
    let Some(guard) = InFlight::acquire(&key) else {
        return stale;
    };
    #[cfg(test)]
    EXPORT_CALLS.with(|calls| calls.set(calls.get() + 1));
    #[cfg(test)]
    let hook = EXPORT_WORKER_HOOK.with(|hook| hook.borrow().clone());
    let (target, cwd) = (target.clone(), cwd.to_path_buf());
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("sparsh-import-exports".into())
        .spawn(move || {
            let guard = guard;
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            let result = spar::intel::exports_of_with_budget(
                &target,
                &cwd,
                type_only,
                &std::collections::HashSet::new(),
                EXPORT_WORKER_BUDGET,
            );
            if let Ok(items) = &result {
                let entry = ExportEntry {
                    key: guard.key.clone(),
                    stamp,
                    stored: std::time::Instant::now(),
                    items: items.clone(),
                };
                let mut state = guard.state.lock().unwrap_or_else(|e| e.into_inner());
                state.cache.retain(|cached| cached.key != entry.key);
                if state.cache.len() >= EXPORT_CACHE_LIMIT {
                    state.cache.remove(0);
                }
                state.cache.push(entry);
            }
            drop(guard);
            let _ = tx.send(result);
        });
    if spawned.is_err() {
        return stale;
    }
    match rx.recv_timeout(wait) {
        Ok(Ok(items)) => items,
        _ => stale,
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

fn complete_theme_arguments(
    snapshot: &CompletionSnapshot,
    line: &str,
    cursor: usize,
) -> Option<Vec<CompletionItem>> {
    let prefix = &line[..cursor];
    let words: Vec<&str> = prefix.split_whitespace().collect();
    if words.first() != Some(&"theme") {
        return None;
    }
    let trailing_space = prefix.ends_with(char::is_whitespace);
    if !trailing_space && words.len() == 1 {
        return None; // still typing the command name itself
    }
    let (position, typed) = if trailing_space {
        (words.len(), "")
    } else {
        (words.len() - 1, *words.last()?)
    };
    let start = cursor - typed.len();
    let make = |replacement: &str, description: Option<String>| CompletionItem {
        span: start..cursor,
        replacement: replacement.to_string(),
        description,
        kind: Some(ItemKind::Keyword),
    };
    let choices: Vec<CompletionItem> = match (position, words.get(1).copied()) {
        (1, _) => ["list", "set", "import", "export"]
            .iter()
            .map(|name| make(name, None))
            .collect(),
        (2, Some("set")) => {
            let mut items = vec![
                make("--accent", Some("generate from #rrggbb".into())),
                make("default", Some("built-in colors".into())),
            ];
            items.extend(snapshot.theme_names.iter().map(|(n, d)| make(n, d.clone())));
            items
        }
        (2, Some("import")) => {
            let mut items = vec![make("pywal", Some("read ~/.cache/wal/colors.json".into()))];
            items.retain(|item| item.replacement.starts_with(typed));
            items.extend(complete_paths(
                snapshot,
                typed,
                start..cursor,
                PathCompletionMode::FilesOnly,
            ));
            return Some(items);
        }
        (2, Some("export")) => {
            return Some(complete_paths(
                snapshot,
                typed,
                start..cursor,
                PathCompletionMode::FilesAndDirectories,
            ))
        }
        _ => return Some(Vec::new()),
    };
    let mut items: Vec<_> = choices
        .into_iter()
        .filter(|item| item.replacement.starts_with(typed))
        .collect();
    items.sort_by(|a, b| a.replacement.cmp(&b.replacement));
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
                kind: Some(ItemKind::Directory),
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
            kind: Some(if is_dir {
                ItemKind::Directory
            } else {
                ItemKind::File
            }),
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

    fn theme_snapshot() -> CompletionSnapshot {
        CompletionSnapshot {
            theme_names: vec![("gruvbox".into(), Some("warm".into())), ("royal".into(), None)],
            ..CompletionSnapshot::default()
        }
    }

    fn at_end(snapshot: &CompletionSnapshot, line: &str) -> Vec<String> {
        complete(snapshot, CompletionRequest { line, cursor: line.len() })
            .into_iter()
            .map(|item| item.replacement)
            .collect()
    }

    #[test]
    fn theme_offers_subcommands() {
        let names = at_end(&theme_snapshot(), "theme ");
        for want in ["list", "set", "import", "export"] {
            assert!(names.contains(&want.to_string()), "{names:?}");
        }
    }

    #[test]
    fn theme_set_offers_registered_names_default_and_accent() {
        assert_eq!(
            at_end(&theme_snapshot(), "theme set "),
            vec!["--accent", "default", "gruvbox", "royal"]
        );
        assert_eq!(at_end(&theme_snapshot(), "theme set gr"), vec!["gruvbox"]);
    }

    #[test]
    fn theme_import_offers_pywal_and_nothing_after_the_name() {
        let names = at_end(&theme_snapshot(), "theme import ");
        assert!(names.contains(&"pywal".to_string()), "{names:?}");
        assert!(at_end(&theme_snapshot(), "theme set gruvbox ").is_empty());
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

    fn wait_idle() {
        for _ in 0..500 {
            if with_export_state(|s| s.in_flight.is_empty()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("export worker never finished");
    }

    fn set_worker_hook(hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>) {
        EXPORT_WORKER_HOOK.with(|slot| *slot.borrow_mut() = hook);
    }

    const WAIT: std::time::Duration = std::time::Duration::from_millis(2000);

    #[test]
    fn a_slow_target_is_not_respawned_and_its_late_result_lands_in_the_cache() {
        let dir = temp_dir_with("lib.spar", LIB);
        let target = spar::intel::ImportTarget::File("lib.spar".into());
        set_worker_hook(Some(std::sync::Arc::new(|| {
            std::thread::sleep(std::time::Duration::from_millis(300))
        })));
        let before = calls();
        let short = std::time::Duration::from_millis(10);
        assert!(cached_exports(&target, dir.path(), false, short).is_empty());
        // Further Tabs while the worker runs return at once and spawn nothing.
        assert!(cached_exports(&target, dir.path(), false, short).is_empty());
        assert!(cached_exports(&target, dir.path(), false, short).is_empty());
        assert_eq!(calls() - before, 1);
        wait_idle();
        set_worker_hook(None);
        // The late result is cached: the next call needs no new worker.
        assert_eq!(cached_exports(&target, dir.path(), false, short).len(), 2);
        assert_eq!(calls() - before, 1);
    }

    #[test]
    fn the_guard_is_released_after_a_worker_panic() {
        let dir = temp_dir_with("lib.spar", LIB);
        let target = spar::intel::ImportTarget::File("lib.spar".into());
        set_worker_hook(Some(std::sync::Arc::new(|| panic!("worker boom"))));
        assert!(cached_exports(&target, dir.path(), false, WAIT).is_empty());
        wait_idle();
        with_export_state(|s| assert!(s.cache.is_empty()));
        set_worker_hook(None);
        assert_eq!(cached_exports(&target, dir.path(), false, WAIT).len(), 2);
    }

    #[test]
    fn the_guard_is_released_after_an_error_and_errors_are_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let missing = spar::intel::ImportTarget::File("missing.spar".into());
        assert!(cached_exports(&missing, dir.path(), false, WAIT).is_empty());
        wait_idle();
        with_export_state(|s| assert!(s.cache.is_empty()));
    }

    #[test]
    fn a_zero_wait_gives_nothing_now_and_the_cache_fills_afterwards() {
        let dir = temp_dir_with("lib.spar", LIB);
        let target = spar::intel::ImportTarget::File("lib.spar".into());
        assert!(cached_exports(&target, dir.path(), false, std::time::Duration::ZERO).is_empty());
        wait_idle();
        let before = calls();
        assert_eq!(cached_exports(&target, dir.path(), false, WAIT).len(), 2);
        assert_eq!(calls(), before);
    }

    #[test]
    fn an_extensionless_import_sees_edits_to_the_resolved_file() {
        let dir = temp_dir_with("lib.spar", LIB);
        let snap = snapshot_in(dir.path());
        let line = "import { } from \"lib\";";
        assert_eq!(import_names(&snap, line, 9), vec!["greet", "port"]);
        std::fs::write(
            dir.path().join("lib.spar"),
            format!("{LIB}export var extra_name: int = 3;\n"),
        )
        .unwrap();
        let names = import_names(&snap, line, 9);
        assert!(names.contains(&"extra_name".to_string()), "{names:?}");
    }

    #[test]
    fn package_targets_are_cached() {
        let dir = tempfile::tempdir().unwrap();
        let target = spar::intel::ImportTarget::Package("std/fs".into());
        let before = calls();
        let first = cached_exports(&target, dir.path(), false, WAIT);
        assert!(!first.is_empty());
        assert_eq!(cached_exports(&target, dir.path(), false, WAIT).len(), first.len());
        assert_eq!(calls() - before, 1);
    }

    #[test]
    fn cache_entries_expire_after_the_ttl() {
        let dir = temp_dir_with("lib.spar", LIB);
        let target = spar::intel::ImportTarget::File("lib.spar".into());
        EXPORT_TTL_OVERRIDE.with(|ttl| ttl.set(Some(std::time::Duration::from_millis(100))));
        let before = calls();
        assert_eq!(cached_exports(&target, dir.path(), false, WAIT).len(), 2);
        assert_eq!(cached_exports(&target, dir.path(), false, WAIT).len(), 2);
        assert_eq!(calls() - before, 1);
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(cached_exports(&target, dir.path(), false, WAIT).len(), 2);
        assert_eq!(calls() - before, 2);
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

    // ── Scope completion ───────────────────────────────────────────────────

    const SCOPE_SESSION: &str = "var count: int = 1;";

    fn scope_names(session: &str, line: &str) -> Vec<String> {
        member_items(session, line).into_iter().map(|i| i.replacement).collect()
    }

    #[test]
    fn a_session_variable_is_offered_after_var_x_eq() {
        let items = member_items(SCOPE_SESSION, "var x = co");
        let count = items.iter().find(|i| i.replacement == "count").expect("count");
        assert_eq!(count.kind, Some(ItemKind::Variable));
        assert_eq!(count.description.as_deref(), Some("int"));
        assert_eq!(count.span, 8..10);
    }

    #[test]
    fn scope_items_are_not_offered_in_command_position() {
        let names = scope_names(SCOPE_SESSION, "co");
        assert!(!names.contains(&"count".to_string()), "{names:?}");
        assert!(!names.contains(&"continue".to_string()), "{names:?}");
    }

    #[test]
    fn scope_items_win_over_snapshot_identifiers_without_duplicates() {
        let mut snapshot = member_snapshot(SCOPE_SESSION);
        snapshot.spar_identifiers.insert("count".into());
        let items = complete(&snapshot, CompletionRequest { line: "var x = co", cursor: 12 });
        let hits: Vec<_> = items.iter().filter(|i| i.replacement == "count").collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, Some(ItemKind::Variable));
    }

    #[test]
    fn snapshot_identifiers_still_appear_when_the_session_does_not_know_them() {
        let names = scope_names(SCOPE_SESSION, "var x = q");
        assert!(names.contains(&"q".to_string()), "{names:?}");
    }

    #[test]
    fn loop_variable_is_offered_inside_a_block_interpolation() {
        let names = scope_names("", "for i in [1, 2] { echo ${i");
        assert!(names.contains(&"i".to_string()), "{names:?}");
    }

    #[test]
    fn session_names_are_offered_inside_command_interpolation() {
        let names = scope_names(SCOPE_SESSION, "echo ${co");
        assert!(names.contains(&"count".to_string()), "{names:?}");
    }

    #[test]
    fn scope_completion_reuses_the_cached_analysis() {
        let snapshot = member_snapshot("var cached_scope_probe: int = 1;");
        let before = analyses();
        complete(&snapshot, CompletionRequest { line: "var x = ca", cursor: 11 });
        complete(&snapshot, CompletionRequest { line: "var y = ca", cursor: 11 });
        assert_eq!(analyses() - before, 1);
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
    fn an_int_literal_is_not_a_receiver() {
        assert!(member_names(SESSION, "x = 3.").is_empty());
        assert!(member_names(SESSION, "x = 3.1").is_empty());
        assert!(member_names(SESSION, "var y = 3.").is_empty());
        // A name ending in digits is still a receiver.
        let session = "struct P { name: str = \"\"; };\nvar p2: P = P();";
        assert!(member_names(session, "x = p2.").contains(&"name".to_string()));
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

    fn analyses() -> usize {
        MEMBER_ANALYSES.with(|count| count.get())
    }

    #[test]
    fn the_session_source_is_compiled_once_across_tabs() {
        let snapshot = member_snapshot(SESSION);
        let before = analyses();
        let first = complete(&snapshot, CompletionRequest { line: "p.", cursor: 2 });
        let second = complete(&snapshot, CompletionRequest { line: "p.na", cursor: 4 });
        let third = complete(&snapshot, CompletionRequest { line: "var n = p.", cursor: 11 });
        assert_eq!(analyses() - before, 1);
        assert!(first.len() >= 2 && second.len() == 1 && !third.is_empty());
    }

    #[test]
    fn a_changed_session_source_recompiles() {
        let before = analyses();
        assert!(member_names(SESSION, "p.").contains(&"port".to_string()));
        let changed = format!("{SESSION}\nstruct Extra {{ z: int = 0; }};");
        assert!(member_names(&changed, "p.").contains(&"port".to_string()));
        assert_eq!(analyses() - before, 2);
        // Same source again: still cached.
        assert!(member_names(&changed, "p.").contains(&"port".to_string()));
        assert_eq!(analyses() - before, 2);
    }

    #[test]
    fn cached_results_equal_uncached_ones() {
        let snapshot = member_snapshot(SESSION);
        for line in ["p.", "p.na", "p.PO", "var n = p.", "nothing."] {
            let cached = complete(&snapshot, CompletionRequest { line, cursor: line.len() });
            let source = format!("{SESSION}\n{line}");
            let direct: Vec<_> = spar::intel::complete_members(&spar::intel::IntelRequest {
                source: &source,
                offset: source.len(),
                base_dir: &snapshot.cwd,
            })
            .into_iter()
            .map(|item| item.label)
            .collect();
            let mut cached: Vec<_> = cached.into_iter().map(|item| item.replacement).collect();
            cached.sort();
            let mut direct = direct;
            direct.sort();
            if line.starts_with("p.") || line.starts_with("var") {
                assert_eq!(cached, direct, "{line}");
            }
        }
    }

    #[test]
    fn non_ascii_partial_is_not_a_member_position() {
        assert_eq!(member_context(&member_snapshot(SESSION), "p.n\u{e9}", "p.n\u{e9}".len()), None);
        assert_eq!(member_names(SESSION, "p.na"), vec!["name"]);
    }

    #[test]
    fn command_looking_names_and_open_blocks() {
        assert!(member_names(SESSION, "ls.").is_empty());
        let session = format!("{SESSION}\nfn f() -> int {{");
        // An unclosed block above must not break the mapping or crash.
        let _ = member_names(&session, "p.");
        let closed = format!("{SESSION}\nfn f() -> int {{\n    return 1;\n}};");
        assert!(member_names(&closed, "p.").contains(&"name".to_string()));
    }

    // ── Signature hint ─────────────────────────────────────────────────────

    const BUILD: &str = "fn build(profile: str, release: bool = false) -> int { return 0; };";

    fn call_snapshot(session: &str) -> CompletionSnapshot {
        let mut snapshot = member_snapshot(session);
        snapshot.spar_functions.insert("build".into(), vec!["profile".into(), "release".into()]);
        snapshot
    }

    #[test]
    fn signature_hint_shows_the_session_signature_with_the_active_parameter() {
        let snapshot = call_snapshot(BUILD);
        let line = "build(profile: ";
        let hint = signature_hint_info(&snapshot, line, line.len()).expect("hint");
        assert_eq!(hint.text, "build(profile: str, release: bool = false)");
        assert_eq!(hint.active, 0);
        assert_eq!(&hint.text[hint.active_range.clone()], "profile: str");
        assert_eq!(signature_hint(&snapshot, line, line.len()).as_deref(), Some(hint.text.as_str()));

        let line = "build(profile: \"x\", ";
        let hint = signature_hint_info(&snapshot, line, line.len()).expect("hint");
        assert_eq!(hint.active, 1);
        assert_eq!(&hint.text[hint.active_range], "release: bool = false");
    }

    #[test]
    fn signature_hint_is_none_outside_a_call() {
        let snapshot = call_snapshot(BUILD);
        for line in ["build", "build(profile: \"x\")", "ls -la", "echo build(", "", "var x = 1"] {
            assert_eq!(signature_hint(&snapshot, line, line.len()), None, "{line}");
        }
    }

    #[test]
    fn signature_hint_is_none_for_unknown_callee_and_cursor_in_a_string() {
        let snapshot = call_snapshot(BUILD);
        assert_eq!(signature_hint(&snapshot, "nothing(", 8), None);
        assert_eq!(signature_hint(&snapshot, "var s = \"build(", 15), None);
    }

    #[test]
    fn signature_hint_works_for_a_nested_call_and_a_mid_line_cursor() {
        let session = format!("{BUILD}\nfn two(a: int, b: int) -> int {{ return a; }};");
        let snapshot = call_snapshot(&session);
        let line = "two(a: build(profile: \"p\", ";
        let hint = signature_hint_info(&snapshot, line, line.len()).expect("hint");
        assert!(hint.text.starts_with("build("), "{}", hint.text);
        let line = "build(profile: \"x\") + 1";
        // The cursor right after the opening parenthesis is inside the call.
        assert!(signature_hint(&snapshot, line, 6).is_some());
        // A multi-byte character before the cursor does not panic.
        let _ = signature_hint(&snapshot, "build(\u{e9}", "build(\u{e9}".len() - 1);
    }

    #[test]
    fn signature_hint_reuses_the_cached_session_analysis() {
        let snapshot = call_snapshot(&format!("{BUILD}\nvar unique_hint_cache: int = 1;"));
        let analyses = || MEMBER_ANALYSES.with(|count| count.get());
        let _ = signature_hint(&snapshot, "build(", 6);
        let before = analyses();
        for line in ["build(profile: ", "build(profile: \"a\", ", "build("] {
            assert!(signature_hint(&snapshot, line, line.len()).is_some());
        }
        assert_eq!(analyses(), before);
    }

    #[test]
    fn named_parameter_items_describe_type_and_default() {
        let snapshot = call_snapshot(BUILD);
        let items = complete(&snapshot, CompletionRequest { line: "build(", cursor: 6 });
        assert_eq!(items.len(), 1, "{items:?}");
        let line = "build(profile: \"x\", re";
        let items = complete(&snapshot, CompletionRequest { line, cursor: line.len() });
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].replacement, "release: ");
        assert_eq!(items[0].description.as_deref(), Some("bool = false"));
        let line = "build(pro";
        let items = complete(&snapshot, CompletionRequest { line, cursor: line.len() });
        assert_eq!(items[0].replacement, "profile: ");
        assert_eq!(items[0].description.as_deref(), Some("str"));
    }

    #[test]
    fn parameter_items_fall_back_when_the_session_does_not_know_the_function() {
        let mut snapshot = CompletionSnapshot::fixture();
        snapshot.spar_functions.insert("create".into(), vec!["name".into()]);
        let items = complete(&snapshot, CompletionRequest { line: "create(na", cursor: 9 });
        assert_eq!(items[0].description.as_deref(), Some("parameter of create"));
    }

    fn kind_of(items: &[CompletionItem], name: &str) -> Option<ItemKind> {
        items.iter().find(|i| i.replacement == name).unwrap_or_else(|| panic!("{name} missing")).kind
    }

    #[test]
    fn every_producer_sets_a_kind() {
        let snapshot = CompletionSnapshot::fixture();
        let at = |line: &str| complete(&snapshot, CompletionRequest { line, cursor: line.len() });
        let items = at("g");
        assert_eq!(kind_of(&items, "git"), Some(ItemKind::Command));
        assert_eq!(kind_of(&items, "gs"), Some(ItemKind::Alias));
        assert_eq!(kind_of(&at("p"), "pwd"), Some(ItemKind::Builtin));
        assert_eq!(kind_of(&at("var x = pro"), "project"), Some(ItemKind::Variable));
        assert_eq!(kind_of(&at("bu"), "build("), Some(ItemKind::Function));
        assert_eq!(kind_of(&at("build(pro"), "profile: "), Some(ItemKind::Parameter));
        assert_eq!(kind_of(&at("cd ~"), "~/"), Some(ItemKind::Directory));
        for line in ["cd ", "cat ", "ls "] {
            assert!(complete_in_fixture_dir(line).iter().all(|i| i.kind.is_some()), "{line}");
        }
        let dirs = complete_in_fixture_dir("cd ");
        assert!(dirs.iter().all(|i| i.kind == Some(ItemKind::Directory)));
        let files = complete_in_fixture_dir("cat ");
        assert!(files.iter().all(|i| i.kind == Some(ItemKind::File)));
        assert_eq!(kind_of(&files, "a.txt"), Some(ItemKind::File));
    }

    #[test]
    fn z_completes_visited_directories_ranked_and_widens_to_all_typed_words() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("alpha");
        let b = root.path().join("alpine");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let mut snapshot = snapshot_in(root.path());
        snapshot.visited_dirs = vec![
            crate::DirVisit { path: a.clone(), count: 1, last: 0 },
            crate::DirVisit { path: b.clone(), count: 9, last: 0 },
            crate::DirVisit { path: root.path().join("gone"), count: 99, last: 0 },
        ];
        let all = complete_at(&snapshot, "z ", 2);
        let names: Vec<_> = all.iter().map(|item| item.replacement.clone()).collect();
        assert_eq!(names, ["alpine", "alpha"]);
        assert_eq!(all[0].description.as_deref(), Some(b.display().to_string().as_str()));
        let one = complete_at(&snapshot, "z alph", 6);
        assert_eq!(one.len(), 1);
        let two = complete_at(&snapshot, "z tmp alph", 10);
        assert_eq!(two.len(), 1);
        assert_eq!(two[0].span, 2..10);
        let prefix = format!("z {}", root.path().display());
        assert_eq!(complete_at(&snapshot, &prefix, prefix.len()).len(), 2);
        // A shared folder name is ambiguous, so the full path is inserted instead.
        let other = root.path().join("x").join("alpha");
        std::fs::create_dir_all(&other).unwrap();
        snapshot.visited_dirs.push(crate::DirVisit { path: other.clone(), count: 1, last: 0 });
        let shared: Vec<_> = complete_at(&snapshot, "z alpha", 7).iter().map(|i| i.replacement.clone()).collect();
        assert!(shared.contains(&a.display().to_string()), "{shared:?}");
        // A short name set with `z --set` is what the menu shows and inserts.
        snapshot.dir_aliases = vec![("web".into(), a.clone())];
        let named: Vec<_> = complete_at(&snapshot, "z ", 2).iter().map(|i| i.replacement.clone()).collect();
        assert!(named.contains(&"web".to_string()), "{named:?}");
    }

    #[test]
    fn await_offers_async_functions_pipes_offer_stages_and_formats() {
        let mut snapshot = CompletionSnapshot::fixture();
        snapshot.spar_functions.insert("dadJokes".into(), vec!["term".into(), "limit".into()]);
        snapshot.spar_functions.insert("select".into(), vec!["fields".into()]);
        snapshot.async_functions.insert("dadJokes".into());
        let names = |line: &str| -> Vec<String> {
            complete(&snapshot, CompletionRequest { line, cursor: line.len() })
                .into_iter()
                .map(|item| item.replacement)
                .collect()
        };
        assert_eq!(names("await "), ["dadJokes"]);
        assert_eq!(names("await dad"), ["dadJokes"]);
        assert!(names("dadJokes() |> sel").contains(&"select".to_string()));
        assert!(names("dadJokes() | from js").contains(&"json".to_string()));
        assert!(names("dadJokes() |> to y").contains(&"yaml".to_string()));
        // Not an `await` position: ordinary completion still applies.
        assert!(!names("build ").contains(&"dadJokes".to_string()));
    }
}
