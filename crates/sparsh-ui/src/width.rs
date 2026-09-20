use std::path::{Component, Path};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

/// Like [`display_width`], but counts private-use-area characters (Nerd Font
/// glyphs) as `glyph_width` cells, for fonts/terminals that draw them wide.
pub(crate) fn display_width_with_glyphs(value: &str, glyph_width: usize) -> usize {
    value
        .chars()
        .map(|ch| {
            let code = ch as u32;
            let private_use = (0xE000..=0xF8FF).contains(&code)
                || (0xF0000..=0xFFFFD).contains(&code)
                || (0x100000..=0x10FFFD).contains(&code);
            if private_use {
                glyph_width
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(0)
            }
        })
        .sum()
}

pub(crate) fn truncate_display(value: &str, max_width: usize) -> String {
    if display_width(value) <= max_width {
        return value.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".into();
    }
    let target = max_width - 1;
    let mut width = 0usize;
    let mut out = String::new();
    for ch in value.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > target {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push('…');
    out
}

pub(crate) fn fold_path(
    cwd: &Path,
    home: Option<&Path>,
    budget: usize,
    parent_length: usize,
    max_last_length: usize,
) -> String {
    if budget == 0 {
        return String::new();
    }
    let (prefix, path) = match home.and_then(|home| cwd.strip_prefix(home).ok()) {
        Some(relative) => ("~".to_string(), relative),
        None => {
            let prefix = if cwd.is_absolute() { "/" } else { "" };
            (prefix.to_string(), cwd)
        }
    };

    let mut components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.is_empty() {
        return truncate_display(&prefix, budget);
    }

    let last = components.pop().expect("checked non-empty");
    let parents = components;

    // Preserve the complete path whenever the first-line budget can hold it.
    // Parent shortening is a responsive fallback, not the default rendering.
    let full = render_path(
        &prefix,
        &parents,
        &last,
        usize::MAX,
        usize::MAX,
        false,
    );
    if display_width(&full) <= budget {
        return full;
    }

    let mut parent_width = parent_length.max(1);
    let mut last_width = max_last_length.max(1);

    loop {
        let rendered = render_path(&prefix, &parents, &last, parent_width, last_width, false);
        if display_width(&rendered) <= budget {
            return rendered;
        }
        if parent_width > 1 {
            parent_width -= 1;
            continue;
        }
        if parents.len() > 2 {
            let rendered = render_path(&prefix, &parents, &last, 1, last_width, true);
            if display_width(&rendered) <= budget {
                return rendered;
            }
            let overhead = display_width(&render_path(&prefix, &parents, "", 1, 0, true));
            last_width = budget.saturating_sub(overhead).max(1);
            return truncate_display(&render_path(&prefix, &parents, &last, 1, last_width, true), budget);
        }
        let overhead = display_width(&render_path(&prefix, &parents, "", 1, 0, false));
        last_width = budget.saturating_sub(overhead).max(1);
        return truncate_display(&render_path(&prefix, &parents, &last, 1, last_width, false), budget);
    }
}

fn render_path(
    prefix: &str,
    parents: &[String],
    last: &str,
    parent_width: usize,
    last_width: usize,
    collapse_middle: bool,
) -> String {
    let mut parts = Vec::new();
    if collapse_middle && parents.len() > 2 {
        parts.push(truncate_display(&parents[0], parent_width));
        parts.push("…".into());
        parts.push(truncate_display(parents.last().unwrap(), parent_width));
    } else {
        parts.extend(parents.iter().map(|part| truncate_display(part, parent_width)));
    }
    if !last.is_empty() {
        parts.push(truncate_display(last, last_width));
    }
    let body = parts.join("/");
    match prefix {
        "/" if body.is_empty() => "/".into(),
        "/" => format!("/{body}"),
        "~" if body.is_empty() => "~".into(),
        "~" => format!("~/{body}"),
        "" => body,
        other if body.is_empty() => other.to_string(),
        other => format!("{other}/{body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_width_counts_private_use_characters() {
        use super::display_width_with_glyphs;
        assert_eq!(display_width_with_glyphs("a\u{f303}b", 1), 3);
        assert_eq!(display_width_with_glyphs("a\u{f303}b", 2), 4);
        assert_eq!(display_width_with_glyphs("a\u{f0079}b", 2), 4);
        assert_eq!(display_width_with_glyphs("abc", 2), 3);
    }

    #[test]
    fn keeps_path_shape_while_shortening_parents() {
        let path = fold_path(
            Path::new("/home/occ/Projects/Spar/phase0-showcase/src"),
            Some(Path::new("/home/occ")),
            30,
            2,
            24,
        );
        assert!(path.starts_with("~/Pr/Sp/"));
        assert!(path.ends_with("/src"));
        assert!(display_width(&path) <= 30);
    }

    #[test]
    fn keeps_full_path_when_it_fits_the_available_budget() {
        let path = fold_path(
            Path::new("/home/occ/Projects/Spar/phase0-showcase/src"),
            Some(Path::new("/home/occ")),
            80,
            2,
            24,
        );
        assert_eq!(path, "~/Projects/Spar/phase0-showcase/src");
    }

    #[test]
    fn unicode_truncation_stays_valid_and_in_budget() {
        let path = fold_path(Path::new("/tmp/日本語/very-long-directory-name"), None, 12, 2, 20);
        assert!(display_width(&path) <= 12);
        assert!(std::str::from_utf8(path.as_bytes()).is_ok());
    }
}
