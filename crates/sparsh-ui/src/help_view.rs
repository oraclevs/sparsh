//! Colored `help NAME` page: title, explanation, usage, then options and
//! examples as bordered tables.

use sparsh_core::HelpPage;
use unicode_width::UnicodeWidthStr;

use crate::{SemanticRole, Theme};

/// Greedy word wrap; a word longer than the line is split.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while word.width() > width {
            let cut: String = word.chars().take(width).collect();
            word = word.chars().skip(width).collect();
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            lines.push(cut);
        }
        if !current.is_empty() && current.width() + 1 + word.width() > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&word);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

/// A two-column bordered table. `first_role` colors the left column.
fn table(
    headers: [&str; 2],
    rows: &[(String, String)],
    first_role: SemanticRole,
    theme: &Theme,
    total_width: usize,
) -> String {
    let border = |text: &str| theme.paint(SemanticRole::TableBorder, text);
    let natural = rows.iter().map(|(a, _)| a.width()).chain([headers[0].width()]).max().unwrap_or(0);
    // borders and padding take 7 columns; the left column gets at most 40%.
    let available = total_width.saturating_sub(7).max(24);
    let left = natural.min(available * 2 / 5).max(headers[0].width());
    let right = available.saturating_sub(left).max(12);
    let rule = |l: &str, m: &str, r: &str| {
        border(&format!("{l}{}{m}{}{r}", "─".repeat(left + 2), "─".repeat(right + 2)))
    };
    let mut out = String::new();
    out.push_str(&rule("╭", "┬", "╮"));
    out.push('\n');
    out.push_str(&format!(
        "{} {} {} {} {}\n",
        border("│"),
        theme.paint(SemanticRole::TableHeader, &pad(headers[0], left)),
        border("│"),
        theme.paint(SemanticRole::TableHeader, &pad(headers[1], right)),
        border("│"),
    ));
    out.push_str(&rule("├", "┼", "┤"));
    out.push('\n');
    for (first, second) in rows {
        let a = wrap(first, left);
        let b = wrap(second, right);
        for index in 0..a.len().max(b.len()) {
            let cell_a = a.get(index).map_or("", String::as_str);
            let cell_b = b.get(index).map_or("", String::as_str);
            out.push_str(&format!(
                "{} {} {} {} {}\n",
                border("│"),
                theme.paint(first_role, &pad(cell_a, left)),
                border("│"),
                pad(cell_b, right),
                border("│"),
            ));
        }
    }
    out.push_str(&rule("╰", "┴", "╯"));
    out.push('\n');
    out
}

pub(crate) fn render_help(page: &HelpPage, theme: &Theme, width: usize) -> String {
    let width = width.clamp(40, 110);
    let heading = |text: &str| format!("{}\n", theme.paint(SemanticRole::TableHeader, text));
    let mut out = format!(
        "\n{}  {}\n",
        theme.paint(SemanticRole::Builtin, &page.name),
        page.summary
    );
    if !page.details.is_empty() {
        out.push('\n');
        for line in wrap(&page.details, width.saturating_sub(2)) {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out.push('\n');
    out.push_str(&heading("USAGE"));
    for line in &page.usage {
        out.push_str(&format!("  {}\n", theme.paint(SemanticRole::Argument, line)));
    }
    if !page.options.is_empty() {
        out.push('\n');
        out.push_str(&heading("OPTIONS"));
        out.push_str(&table(["option", "what it does"], &page.options, SemanticRole::Option, theme, width));
    }
    if !page.examples.is_empty() {
        out.push('\n');
        out.push_str(&heading("EXAMPLES"));
        out.push_str(&table(["type this", "what happens"], &page.examples, SemanticRole::Success, theme, width));
    }
    if !page.see_also.is_empty() {
        let names: Vec<String> = page
            .see_also
            .iter()
            .map(|name| theme.paint(SemanticRole::Builtin, name))
            .collect();
        out.push_str(&format!("\n{} {}\n", theme.paint(SemanticRole::TableHeader, "SEE ALSO"), names.join(", ")));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_shows_every_section_within_the_width() {
        let registry = sparsh_core::builtin::BuiltinRegistry::new();
        let page = sparsh_core::help_page("z", &registry).unwrap();
        let text = render_help(&page, &Theme::plain(), 80);
        for part in ["USAGE", "OPTIONS", "EXAMPLES", "SEE ALSO", "z --set api", "╭", "╯"] {
            assert!(text.contains(part), "{part}\n{text}");
        }
        assert!(text.lines().all(|line| line.width() <= 80), "{text}");
    }

    #[test]
    fn wrap_splits_long_words_and_keeps_short_lines() {
        assert_eq!(wrap("aaaa bbbb", 8), ["aaaa", "bbbb"]);
        assert!(wrap(&"x".repeat(30), 10).iter().all(|line| line.width() <= 10));
    }
}
