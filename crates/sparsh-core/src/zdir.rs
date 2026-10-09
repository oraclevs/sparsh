//! `z`: jump to a directory the user has been in before, found by name.
//! Only directories recorded in history are candidates; nothing is guessed.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use spar::{TableValue, Value};

/// One remembered directory: how often it was entered and when last (unix secs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirVisit {
    pub path: PathBuf,
    pub count: u64,
    pub last: u64,
}

fn score(visit: &DirVisit, now: u64) -> f64 {
    let age = now.saturating_sub(visit.last);
    let weight = match age {
        0..=3_599 => 4.0,
        3_600..=86_399 => 2.0,
        86_400..=604_799 => 0.5,
        _ => 0.25,
    };
    visit.count as f64 * weight
}

/// Words must appear in order in the path, and the last word must sit inside
/// the final path component (`z doc` finds `/home/me/docs`, not `/docs/a/b`).
fn matches(path: &str, words: &[String]) -> bool {
    let lower = path.to_lowercase();
    let last_component = lower.trim_end_matches('/').rfind('/').map_or(0, |index| index + 1);
    let mut from = 0;
    for (index, word) in words.iter().enumerate() {
        let word = word.to_lowercase();
        let start = if index + 1 == words.len() { from.max(last_component) } else { from };
        match lower.get(start..).and_then(|rest| rest.find(&word)) {
            Some(offset) => from = start + offset + word.len(),
            None => return false,
        }
    }
    true
}

/// A user-chosen short name for a directory (`z --set name`).
pub type DirAlias = (String, PathBuf);

fn base_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Remembered directories matching `words`, best first. `skip` (the current
/// directory) is left out, since jumping there is pointless. A lone word that
/// is an alias wins outright; then directories named exactly that word; then
/// the rest by frecency.
pub fn rank(
    visits: &[DirVisit],
    words: &[String],
    now: u64,
    skip: Option<&Path>,
    aliases: &[DirAlias],
) -> Vec<PathBuf> {
    let mut found: Vec<(&DirVisit, f64)> = visits
        .iter()
        .filter(|visit| visit.path.to_str().is_some_and(|path| matches(path, words)))
        .map(|visit| (visit, score(visit, now)))
        .collect();
    found.sort_by(|left, right| {
        right.1.total_cmp(&left.1).then_with(|| left.0.path.cmp(&right.0.path))
    });
    let mut ordered: Vec<PathBuf> = found.into_iter().map(|(visit, _)| visit.path.clone()).collect();
    if let [word] = words {
        let word = word.to_lowercase();
        // `sort_by_key` is stable: frecency order survives inside each group.
        ordered.sort_by_key(|path| base_name(path) != word);
        let named = aliases
            .iter()
            .filter(|(name, _)| name.to_lowercase() == word)
            .map(|(_, path)| path.clone());
        let mut front: Vec<PathBuf> = named.collect();
        front.extend(ordered);
        ordered = front;
    }
    let mut seen = std::collections::HashSet::new();
    ordered.retain(|path| skip != Some(path.as_path()) && seen.insert(path.clone()));
    ordered
}

/// What to show and insert for `path` in a menu: its alias, else its folder
/// name when no other known directory or alias shares it. `None` means the
/// name is ambiguous and the full path should be used.
pub fn short_name(path: &Path, aliases: &[DirAlias], known: &[DirVisit]) -> Option<String> {
    if let Some((name, _)) = aliases.iter().find(|(_, target)| target == path) {
        return Some(name.clone());
    }
    let name = base_name(path);
    if name.is_empty() {
        return None;
    }
    let shared = known.iter().any(|visit| visit.path != path && base_name(&visit.path) == name)
        || aliases.iter().any(|(alias, target)| target != path && alias.to_lowercase() == name);
    (!shared).then(|| path.file_name().unwrap_or_default().to_string_lossy().into_owned())
}

/// Every known directory, best first, with the short name the menu would use.
/// Aliased directories that were never visited are listed with 0 visits.
pub fn listing(visits: &[DirVisit], aliases: &[DirAlias], now: u64) -> Vec<(Option<String>, DirVisit)> {
    let mut all: Vec<DirVisit> = visits.to_vec();
    for (_, path) in aliases {
        if !all.iter().any(|visit| &visit.path == path) {
            all.push(DirVisit { path: path.clone(), count: 0, last: 0 });
        }
    }
    let order = rank(&all, &[], now, None, &[]);
    order
        .into_iter()
        .filter_map(|path| all.iter().find(|visit| visit.path == path).cloned())
        .map(|visit| (short_name(&visit.path, aliases, &all), visit))
        .collect()
}

/// `z --list` as a table value: name, path, visits, last.
pub fn listing_table(visits: &[DirVisit], aliases: &[DirAlias], now: u64) -> Result<Value, String> {
    let rows = listing(visits, aliases, now)
        .into_iter()
        .map(|(name, visit)| {
            let mut fields = IndexMap::new();
            fields.insert("name".to_string(), name.map_or(Value::Void, Value::String));
            fields.insert("path".to_string(), Value::String(visit.path.to_string_lossy().into_owned()));
            fields.insert("visits".to_string(), Value::Int(i64::try_from(visit.count).unwrap_or(i64::MAX)));
            fields.insert(
                "last".to_string(),
                if visit.last == 0 {
                    Value::Void
                } else {
                    Value::String(crate::listing::format_iso8601(visit.last as i64))
                },
            );
            Value::Object(fields.into())
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(Value::Table(TableValue::with_schema(Vec::new(), spar::Schema::default()).into()));
    }
    let mut schema = spar::Schema::infer_records(&rows)
        .map_err(|error| format!("z: cannot build table: {error:?}"))?;
    let rank = |name: &str| ["name", "path", "visits", "last"].iter().position(|c| *c == name).unwrap_or(4);
    schema.fields.sort_by_key(|field| rank(&field.name));
    Ok(Value::Table(TableValue::with_schema(rows, schema).into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visit(path: &str, count: u64, last: u64) -> DirVisit {
        DirVisit { path: PathBuf::from(path), count, last }
    }

    #[test]
    fn last_word_must_match_the_final_component() {
        let visits = [visit("/home/me/docs/a", 9, 100), visit("/home/me/docs", 1, 100)];
        let words = ["docs".to_string()];
        assert_eq!(rank(&visits, &words, 100, None, &[]), vec![PathBuf::from("/home/me/docs")]);
    }

    #[test]
    fn words_match_in_order() {
        let visits = [visit("/work/spar/src", 1, 0), visit("/src/work/spar", 1, 0)];
        let words = ["work".to_string(), "src".to_string()];
        assert_eq!(rank(&visits, &words, 0, None, &[]), vec![PathBuf::from("/work/spar/src")]);
    }

    #[test]
    fn frequent_recent_directories_rank_first_and_cwd_is_skipped() {
        let now = 1_000_000;
        let visits = [
            visit("/p/old", 50, 0),
            visit("/p/new", 5, now - 10),
            visit("/p/here", 99, now),
        ];
        let ranked = rank(&visits, &["p".to_string()][..0], now, Some(Path::new("/p/here")), &[]);
        assert_eq!(ranked, vec![PathBuf::from("/p/new"), PathBuf::from("/p/old")]);
    }

    #[test]
    fn matching_is_case_insensitive_and_empty_query_lists_all() {
        let visits = [visit("/Home/Me/Projects", 1, 0)];
        assert_eq!(rank(&visits, &["proj".to_string()], 0, None, &[]).len(), 1);
        assert_eq!(rank(&visits, &[], 0, None, &[]).len(), 1);
    }

    #[test]
    fn exact_name_and_alias_beat_busier_partial_matches() {
        let visits = [visit("/a/yovo-api", 50, 0), visit("/a/yovo", 1, 0)];
        let words = ["yovo".to_string()];
        assert_eq!(rank(&visits, &words, 0, None, &[])[0], PathBuf::from("/a/yovo"));
        let aliases = [("yovo".to_string(), PathBuf::from("/elsewhere"))];
        assert_eq!(rank(&visits, &words, 0, None, &aliases)[0], PathBuf::from("/elsewhere"));
    }

    #[test]
    fn short_names_fall_back_to_full_paths_when_ambiguous() {
        let visits = [visit("/a/src", 1, 0), visit("/b/src", 1, 0), visit("/a/lib", 1, 0)];
        assert_eq!(short_name(Path::new("/a/lib"), &[], &visits), Some("lib".into()));
        assert_eq!(short_name(Path::new("/a/src"), &[], &visits), None);
        let aliases = [("web".to_string(), PathBuf::from("/a/src"))];
        assert_eq!(short_name(Path::new("/a/src"), &aliases, &visits), Some("web".into()));
    }

    #[test]
    fn listing_includes_unvisited_aliases_and_every_visited_directory() {
        let visits = [visit("/a/one", 3, 100), visit("/a/two", 1, 100)];
        let aliases = [("home".to_string(), PathBuf::from("/h"))];
        let rows = listing(&visits, &aliases, 100);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|(name, v)| name.as_deref() == Some("home") && v.count == 0));
        assert!(listing_table(&visits, &aliases, 100).is_ok());
    }
}
