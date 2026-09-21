//! The `prompt` section of the config, including the right-prompt slots.
//!
//! Every problem in this section is *soft*: parsing never fails. A bad field
//! falls back to its default, a bad slot becomes [`SlotConfig::Broken`], and
//! each problem is recorded as a [`PromptIssue`] so the UI can show it while
//! the shell (and the rest of the config) keeps working.

use indexmap::IndexMap;
use std::collections::BTreeSet;

use spar::ConfigValue;

use crate::template::{suggest, Piece, Template, WidgetKind};
use crate::PromptConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpec {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStyle {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotSpec {
    pub template: Template,
    pub color: Option<ColorSpec>,
    pub style: TextStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotConfig {
    Ok(SlotSpec),
    /// The slot's configuration is invalid; the UI shows a red marker.
    Broken {
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Threshold {
    pub warn: Option<u32>,
    pub critical: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Thresholds {
    pub cpu: Threshold,
    pub ram: Threshold,
    pub disk: Threshold,
    pub battery: Threshold,
    pub load: Threshold,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RightPromptConfig {
    pub slots: [Option<SlotConfig>; 3],
    pub separator: String,
    pub glyph_width: usize,
    pub thresholds: Thresholds,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptIssue {
    /// Config path, e.g. `config.prompt.right.slot2.text`.
    pub path: String,
    pub message: String,
    /// The right-prompt slot (1-3) this issue broke, if any.
    pub slot: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NeededWidgets {
    pub kinds: BTreeSet<WidgetKind>,
    pub disk_paths: Vec<String>,
}

const DEFAULT_SEPARATOR: &str = "  ";

impl RightPromptConfig {
    /// The layout used when no `right` section is configured: identical to the
    /// prompt before slots existed (duration, then clock).
    pub fn legacy_default(show_duration: bool, time_enabled: bool, time_format: &str) -> Self {
        let slot = |source: String| {
            Template::parse(&source).ok().map(|template| {
                SlotConfig::Ok(SlotSpec {
                    template,
                    color: None,
                    style: TextStyle::default(),
                })
            })
        };
        RightPromptConfig {
            slots: [
                show_duration.then(|| slot("{duration}".into())).flatten(),
                time_enabled
                    .then(|| slot(format!("{{time:{time_format}}}")))
                    .flatten(),
                None,
            ],
            separator: DEFAULT_SEPARATOR.into(),
            glyph_width: 1,
            thresholds: Thresholds::default(),
        }
    }

    /// The widgets (and explicit disk paths) the sampler has to read. Broken
    /// slots contribute nothing.
    pub fn needed_widgets(&self) -> NeededWidgets {
        let mut needed = NeededWidgets::default();
        for slot in self.slots.iter().flatten() {
            let SlotConfig::Ok(spec) = slot else { continue };
            for piece in &spec.template.pieces {
                let Piece::Widget(widget) = piece else {
                    continue;
                };
                needed.kinds.insert(widget.kind);
                if widget.kind == WidgetKind::Disk {
                    for arg in widget.args.iter().filter(|arg| arg.starts_with('/')) {
                        if !needed.disk_paths.contains(arg) {
                            needed.disk_paths.push(arg.clone());
                        }
                    }
                }
            }
        }
        needed
    }
}

pub fn parse_color(text: &str) -> Result<ColorSpec, String> {
    let named = [
        ("black", 0),
        ("red", 1),
        ("green", 2),
        ("yellow", 3),
        ("blue", 4),
        ("magenta", 5),
        ("cyan", 6),
        ("white", 7),
        ("gray", 8),
        ("lightred", 9),
        ("lightgreen", 10),
        ("lightyellow", 11),
        ("lightblue", 12),
        ("lightmagenta", 13),
        ("lightcyan", 14),
        ("lightwhite", 15),
    ];
    if let Some((_, index)) = named.iter().find(|(name, _)| *name == text) {
        return Ok(ColorSpec::Indexed(*index));
    }
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).unwrap();
            return Ok(ColorSpec::Rgb(byte(0..2), byte(2..4), byte(4..6)));
        }
    } else if let Ok(index) = text.parse::<u8>() {
        return Ok(ColorSpec::Indexed(index));
    }
    Err(format!(
        "unknown color '{text}' (expected a name like cyan or lightred, #rrggbb, or 0-255)"
    ))
}

/// The three time formats the old `prompt.time.format` accepted, as strftime.
pub fn legacy_time_format_to_strftime(format: &str) -> Option<&'static str> {
    match format {
        "HH:mm" => Some("%H:%M"),
        "HH:mm:ss" => Some("%H:%M:%S"),
        "hh:mm:ss a" => Some("%I:%M:%S %p"),
        _ => None,
    }
}

type Section = IndexMap<String, ConfigValue>;

#[derive(Default)]
struct Issues(Vec<PromptIssue>);

impl Issues {
    fn push(&mut self, path: impl Into<String>, message: impl Into<String>, slot: Option<u8>) {
        self.0.push(PromptIssue {
            path: path.into(),
            message: message.into(),
            slot,
        });
    }
}

fn as_section<'a>(value: &'a ConfigValue, path: &str, issues: &mut Issues) -> Option<&'a Section> {
    match value {
        ConfigValue::Section(section) => Some(section),
        other => {
            issues.push(
                path,
                format!("must be a section, got {} (ignored)", other.type_name()),
                None,
            );
            None
        }
    }
}

fn check_fields(section: &Section, path: &str, allowed: &[&str], issues: &mut Issues) {
    let mut unknown: Vec<&String> = section
        .keys()
        .filter(|field| !allowed.contains(&field.as_str()))
        .collect();
    unknown.sort();
    for field in unknown {
        let message = match suggest(field, allowed) {
            Some(close) => format!("unknown field '{field}'; did you mean '{close}'? (ignored)"),
            None => format!("unknown field '{field}' (ignored)"),
        };
        issues.push(format!("{path}.{field}"), message, None);
    }
}

fn get_bool(
    section: &Section,
    key: &str,
    parent: &str,
    default: bool,
    issues: &mut Issues,
) -> bool {
    match section.get(key) {
        None => default,
        Some(ConfigValue::Bool(value)) => *value,
        Some(other) => {
            issues.push(
                format!("{parent}.{key}"),
                format!("must be bool, got {} (using default)", other.type_name()),
                None,
            );
            default
        }
    }
}

/// `Some(value)` only when present, an int, and within `min..=max`.
fn get_uint(
    section: &Section,
    key: &str,
    parent: &str,
    min: i64,
    max: i64,
    issues: &mut Issues,
) -> Option<u64> {
    match section.get(key)? {
        ConfigValue::Int(value) if (min..=max).contains(value) => Some(*value as u64),
        ConfigValue::Int(value) => {
            issues.push(
                format!("{parent}.{key}"),
                format!("must be between {min} and {max}, got {value} (using default)"),
                None,
            );
            None
        }
        other => {
            issues.push(
                format!("{parent}.{key}"),
                format!("must be int, got {} (using default)", other.type_name()),
                None,
            );
            None
        }
    }
}

pub(crate) fn parse_prompt(value: &ConfigValue) -> (PromptConfig, Vec<PromptIssue>) {
    let mut issues = Issues::default();
    let mut prompt = PromptConfig::default();
    let path = "config.prompt";
    let Some(section) = as_section(value, path, &mut issues) else {
        return (prompt, issues.0);
    };
    check_fields(
        section,
        path,
        &[
            "showStatus",
            "showDuration",
            "durationThresholdMs",
            "path",
            "git",
            "time",
            "right",
        ],
        &mut issues,
    );

    prompt.show_status = get_bool(section, "showStatus", path, prompt.show_status, &mut issues);
    if let Some(ms) = get_uint(
        section,
        "durationThresholdMs",
        path,
        0,
        86_400_000,
        &mut issues,
    ) {
        prompt.duration_threshold_ms = ms;
    }

    if let Some(value) = section.get("path") {
        let path = "config.prompt.path";
        if let Some(path_section) = as_section(value, path, &mut issues) {
            check_fields(
                path_section,
                path,
                &["enabled", "parentLength", "maxLastLength", "maxWidth"],
                &mut issues,
            );
            prompt.path.enabled = get_bool(
                path_section,
                "enabled",
                path,
                prompt.path.enabled,
                &mut issues,
            );
            if let Some(v) = get_uint(path_section, "parentLength", path, 1, i64::MAX, &mut issues)
            {
                prompt.path.parent_length = v as usize;
            }
            if let Some(v) = get_uint(
                path_section,
                "maxLastLength",
                path,
                1,
                i64::MAX,
                &mut issues,
            ) {
                prompt.path.max_last_length = v as usize;
            }
            if let Some(v) = get_uint(path_section, "maxWidth", path, 8, i64::MAX, &mut issues) {
                prompt.path.max_width = v as usize;
            }
        }
    }

    if let Some(value) = section.get("git") {
        let path = "config.prompt.git";
        if let Some(git) = as_section(value, path, &mut issues) {
            check_fields(
                git,
                path,
                &[
                    "enabled",
                    "showBranch",
                    "showAheadBehind",
                    "showStaged",
                    "showModified",
                    "showUntracked",
                    "showConflicts",
                ],
                &mut issues,
            );
            let g = &mut prompt.git;
            g.enabled = get_bool(git, "enabled", path, g.enabled, &mut issues);
            g.show_branch = get_bool(git, "showBranch", path, g.show_branch, &mut issues);
            g.show_ahead_behind = get_bool(
                git,
                "showAheadBehind",
                path,
                g.show_ahead_behind,
                &mut issues,
            );
            g.show_staged = get_bool(git, "showStaged", path, g.show_staged, &mut issues);
            g.show_modified = get_bool(git, "showModified", path, g.show_modified, &mut issues);
            g.show_untracked = get_bool(git, "showUntracked", path, g.show_untracked, &mut issues);
            g.show_conflicts = get_bool(git, "showConflicts", path, g.show_conflicts, &mut issues);
        }
    }

    // `right` wins over the legacy keys; a `right` that isn't a section is
    // ignored and the legacy layout applies.
    let right_section = section
        .get("right")
        .and_then(|value| as_section(value, "config.prompt.right", &mut issues));

    if let Some(right) = right_section {
        for key in ["showDuration", "time"] {
            if section.contains_key(key) {
                issues.push(
                    format!("config.prompt.{key}"),
                    format!("{key} is ignored because prompt.right is set"),
                    None,
                );
            }
        }
        prompt.right = parse_right(right, &mut issues);
    } else {
        prompt.show_duration = get_bool(
            section,
            "showDuration",
            path,
            prompt.show_duration,
            &mut issues,
        );
        let mut strftime = "%H:%M:%S";
        if let Some(value) = section.get("time") {
            let path = "config.prompt.time";
            if let Some(time) = as_section(value, path, &mut issues) {
                check_fields(time, path, &["enabled", "format"], &mut issues);
                prompt.time.enabled =
                    get_bool(time, "enabled", path, prompt.time.enabled, &mut issues);
                match time.get("format") {
                    None => {}
                    Some(ConfigValue::Str(format)) => match legacy_time_format_to_strftime(format) {
                        Some(converted) => {
                            prompt.time.format = format.clone();
                            strftime = converted;
                        }
                        None => issues.push(
                            "config.prompt.time.format",
                            format!("must be HH:mm, HH:mm:ss, or hh:mm:ss a, got '{format}' (using HH:mm:ss)"),
                            None,
                        ),
                    },
                    Some(other) => issues.push(
                        "config.prompt.time.format",
                        format!("must be str, got {} (using default)", other.type_name()),
                        None,
                    ),
                }
            }
        }
        prompt.right =
            RightPromptConfig::legacy_default(prompt.show_duration, prompt.time.enabled, strftime);
    }

    (prompt, issues.0)
}

fn parse_right(section: &Section, issues: &mut Issues) -> RightPromptConfig {
    let path = "config.prompt.right";
    check_fields(
        section,
        path,
        &[
            "slot1",
            "slot2",
            "slot3",
            "separator",
            "glyphWidth",
            "thresholds",
        ],
        issues,
    );
    let mut right = RightPromptConfig {
        slots: [None, None, None],
        separator: DEFAULT_SEPARATOR.into(),
        glyph_width: 1,
        thresholds: Thresholds::default(),
    };

    for (index, key) in ["slot1", "slot2", "slot3"].into_iter().enumerate() {
        if let Some(value) = section.get(key) {
            right.slots[index] = Some(parse_slot(value, index as u8 + 1, issues));
        }
    }

    match section.get("separator") {
        None => {}
        Some(ConfigValue::Str(separator)) if !separator.chars().any(char::is_control) => {
            right.separator = separator.clone();
        }
        Some(ConfigValue::Str(_)) => issues.push(
            format!("{path}.separator"),
            "must not contain control characters (using default)",
            None,
        ),
        Some(other) => issues.push(
            format!("{path}.separator"),
            format!("must be str, got {} (using default)", other.type_name()),
            None,
        ),
    }

    if let Some(width) = get_uint(section, "glyphWidth", path, 1, 2, issues) {
        right.glyph_width = width as usize;
    }

    if let Some(value) = section.get("thresholds") {
        let path = "config.prompt.right.thresholds";
        if let Some(thresholds) = as_section(value, path, issues) {
            check_fields(
                thresholds,
                path,
                &["cpu", "ram", "disk", "battery", "load"],
                issues,
            );
            right.thresholds.cpu = parse_threshold(thresholds, "cpu", path, 100, false, issues);
            right.thresholds.ram = parse_threshold(thresholds, "ram", path, 100, false, issues);
            right.thresholds.disk = parse_threshold(thresholds, "disk", path, 100, false, issues);
            right.thresholds.battery =
                parse_threshold(thresholds, "battery", path, 100, true, issues);
            right.thresholds.load = parse_threshold(thresholds, "load", path, 1000, false, issues);
        }
    }
    right
}

fn parse_threshold(
    thresholds: &Section,
    name: &str,
    parent: &str,
    max: i64,
    lower_is_worse: bool,
    issues: &mut Issues,
) -> Threshold {
    let Some(value) = thresholds.get(name) else {
        return Threshold::default();
    };
    let path = format!("{parent}.{name}");
    let Some(section) = as_section(value, &path, issues) else {
        return Threshold::default();
    };
    check_fields(section, &path, &["warn", "critical"], issues);
    let threshold = Threshold {
        warn: get_uint(section, "warn", &path, 0, max, issues).map(|v| v as u32),
        critical: get_uint(section, "critical", &path, 0, max, issues).map(|v| v as u32),
    };
    if let (Some(warn), Some(critical)) = (threshold.warn, threshold.critical) {
        let ordered = if lower_is_worse {
            warn > critical
        } else {
            warn < critical
        };
        if !ordered {
            let expectation = if lower_is_worse {
                "warn must be greater than critical (lower is worse)"
            } else {
                "warn must be less than critical"
            };
            issues.push(path, format!("{expectation} (using defaults)"), None);
            return Threshold::default();
        }
    }
    threshold
}

fn parse_slot(value: &ConfigValue, number: u8, issues: &mut Issues) -> SlotConfig {
    let path = format!("config.prompt.right.slot{number}");
    let broken = |issues: &mut Issues, field_path: String, message: String| {
        issues.push(field_path, message.clone(), Some(number));
        SlotConfig::Broken { message }
    };

    let ConfigValue::Section(section) = value else {
        return broken(
            issues,
            path,
            format!("must be a section, got {}", value.type_name()),
        );
    };
    check_fields(section, &path, &["text", "color", "style"], issues);

    let text = match section.get("text") {
        Some(ConfigValue::Str(text)) => text,
        Some(other) => {
            return broken(
                issues,
                format!("{path}.text"),
                format!("must be str, got {}", other.type_name()),
            )
        }
        None => return broken(issues, format!("{path}.text"), "is required".into()),
    };
    let template = match Template::parse(text) {
        Ok(template) => template,
        Err(message) => return broken(issues, format!("{path}.text"), message),
    };

    let color = match section.get("color") {
        None => None,
        Some(ConfigValue::Str(color)) => match parse_color(color) {
            Ok(color) => Some(color),
            Err(message) => return broken(issues, format!("{path}.color"), message),
        },
        Some(other) => {
            return broken(
                issues,
                format!("{path}.color"),
                format!("must be str, got {}", other.type_name()),
            )
        }
    };

    let mut style = TextStyle::default();
    match section.get("style") {
        None => {}
        Some(ConfigValue::List(items)) => {
            for item in items {
                match item {
                    ConfigValue::Str(name) if name == "bold" => style.bold = true,
                    ConfigValue::Str(name) if name == "dim" => style.dim = true,
                    ConfigValue::Str(name) if name == "italic" => style.italic = true,
                    ConfigValue::Str(name) if name == "underline" => style.underline = true,
                    other => {
                        let shown = match other {
                            ConfigValue::Str(name) => format!("'{name}'"),
                            other => other.type_name().to_string(),
                        };
                        return broken(
                            issues,
                            format!("{path}.style"),
                            format!(
                                "unknown style {shown} (expected bold, dim, italic or underline)"
                            ),
                        );
                    }
                }
            }
        }
        Some(other) => {
            return broken(
                issues,
                format!("{path}.style"),
                format!("must be a list of strings, got {}", other.type_name()),
            )
        }
    }

    SlotConfig::Ok(SlotSpec {
        template,
        color,
        style,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(entries: &[(&str, ConfigValue)]) -> ConfigValue {
        ConfigValue::Section(
            entries
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
        )
    }
    fn s(text: &str) -> ConfigValue {
        ConfigValue::Str(text.into())
    }
    fn i(value: i64) -> ConfigValue {
        ConfigValue::Int(value)
    }
    fn b(value: bool) -> ConfigValue {
        ConfigValue::Bool(value)
    }
    fn list(items: &[ConfigValue]) -> ConfigValue {
        ConfigValue::List(items.to_vec())
    }

    #[test]
    fn colors_parse_names_hex_and_numbers() {
        assert_eq!(parse_color("cyan"), Ok(ColorSpec::Indexed(6)));
        assert_eq!(parse_color("lightred"), Ok(ColorSpec::Indexed(9)));
        assert_eq!(parse_color("gray"), Ok(ColorSpec::Indexed(8)));
        assert_eq!(parse_color("#ff8800"), Ok(ColorSpec::Rgb(255, 136, 0)));
        assert_eq!(parse_color("208"), Ok(ColorSpec::Indexed(208)));
        let e = parse_color("teal").unwrap_err();
        assert!(
            e.contains("unknown color 'teal'") && e.contains("#rrggbb"),
            "{e}"
        );
        assert!(parse_color("300").is_err());
        assert!(parse_color("#12345").is_err());
    }

    #[test]
    fn absent_right_section_yields_todays_layout_and_no_issues() {
        let (p, issues) = parse_prompt(&sec(&[]));
        assert!(issues.is_empty());
        assert_eq!(p, crate::PromptConfig::default());
        let s1 = match &p.right.slots[0] {
            Some(SlotConfig::Ok(s)) => s,
            other => panic!("{other:?}"),
        };
        assert_eq!(s1.template, Template::parse("{duration}").unwrap());
        let s2 = match &p.right.slots[1] {
            Some(SlotConfig::Ok(s)) => s,
            other => panic!("{other:?}"),
        };
        assert_eq!(s2.template, Template::parse("{time:%H:%M:%S}").unwrap());
        assert!(p.right.slots[2].is_none());
    }

    #[test]
    fn legacy_keys_convert_when_right_is_absent() {
        let (p, issues) = parse_prompt(&sec(&[
            ("showDuration", b(false)),
            ("time", sec(&[("format", s("hh:mm:ss a"))])),
        ]));
        assert!(issues.is_empty(), "{issues:?}");
        assert!(p.right.slots[0].is_none());
        match &p.right.slots[1] {
            Some(SlotConfig::Ok(spec)) => {
                assert_eq!(
                    spec.template,
                    Template::parse("{time:%I:%M:%S %p}").unwrap()
                )
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn full_right_section_parses() {
        let (p, issues) = parse_prompt(&sec(&[(
            "right",
            sec(&[
                (
                    "slot1",
                    sec(&[
                        ("text", s("  {cpu}%")),
                        ("color", s("cyan")),
                        ("style", list(&[s("bold"), s("italic")])),
                    ]),
                ),
                ("slot3", sec(&[("text", s("{date:%a}"))])),
                ("separator", s(" | ")),
                ("glyphWidth", i(2)),
                (
                    "thresholds",
                    sec(&[("cpu", sec(&[("warn", i(60)), ("critical", i(80))]))]),
                ),
            ]),
        )]));
        assert!(issues.is_empty(), "{issues:?}");
        let spec = match &p.right.slots[0] {
            Some(SlotConfig::Ok(s)) => s,
            o => panic!("{o:?}"),
        };
        assert_eq!(spec.color, Some(ColorSpec::Indexed(6)));
        assert!(spec.style.bold && spec.style.italic && !spec.style.dim);
        assert!(p.right.slots[1].is_none());
        assert_eq!(p.right.separator, " | ");
        assert_eq!(p.right.glyph_width, 2);
        assert_eq!(
            p.right.thresholds.cpu,
            Threshold {
                warn: Some(60),
                critical: Some(80)
            }
        );
    }

    #[test]
    fn a_bad_slot_is_broken_alone_and_others_survive() {
        let (p, issues) = parse_prompt(&sec(&[(
            "right",
            sec(&[
                ("slot1", sec(&[("text", s("{cpu}"))])),
                ("slot2", sec(&[("text", s("{cpuu}"))])),
                ("slot3", sec(&[("text", s("{time}")), ("color", s("nope"))])),
            ]),
        )]));
        assert!(matches!(p.right.slots[0], Some(SlotConfig::Ok(_))));
        assert!(
            matches!(&p.right.slots[1], Some(SlotConfig::Broken { message }) if message.contains("unknown widget 'cpuu'"))
        );
        assert!(
            matches!(&p.right.slots[2], Some(SlotConfig::Broken { message }) if message.contains("unknown color 'nope'"))
        );
        let paths: Vec<_> = issues.iter().map(|i| (i.path.as_str(), i.slot)).collect();
        assert!(
            paths.contains(&("config.prompt.right.slot2.text", Some(2))),
            "{paths:?}"
        );
        assert!(
            paths.contains(&("config.prompt.right.slot3.color", Some(3))),
            "{paths:?}"
        );
    }

    #[test]
    fn slot_shape_errors_break_the_slot() {
        for bad in [
            i(5),
            sec(&[]),
            sec(&[("text", i(1))]),
            sec(&[("text", s("x")), ("style", list(&[s("wavy")]))]),
        ] {
            let (p, issues) = parse_prompt(&sec(&[("right", sec(&[("slot2", bad)]))]));
            assert!(matches!(p.right.slots[1], Some(SlotConfig::Broken { .. })));
            assert!(issues.iter().any(|i| i.slot == Some(2)));
        }
    }

    #[test]
    fn field_errors_fall_back_to_defaults_with_issues() {
        let (p, issues) = parse_prompt(&sec(&[
            ("path", sec(&[("parentLength", i(0))])),
            ("durationThresholdMs", i(999_999_999)),
            (
                "right",
                sec(&[
                    ("glyphWidth", i(5)),
                    ("separator", i(1)),
                    (
                        "thresholds",
                        sec(&[
                            ("cpu", sec(&[("warn", i(150))])),
                            ("battery", sec(&[("warn", i(10)), ("critical", i(20))])),
                        ]),
                    ),
                ]),
            ),
        ]));
        let d = crate::PromptConfig::default();
        assert_eq!(p.path.parent_length, d.path.parent_length);
        assert_eq!(p.duration_threshold_ms, d.duration_threshold_ms);
        assert_eq!(p.right.glyph_width, 1);
        assert_eq!(p.right.separator, "  ");
        assert_eq!(p.right.thresholds.cpu, Threshold::default());
        assert_eq!(p.right.thresholds.battery, Threshold::default());
        for path in [
            "config.prompt.path.parentLength",
            "config.prompt.durationThresholdMs",
            "config.prompt.right.glyphWidth",
            "config.prompt.right.separator",
            "config.prompt.right.thresholds.cpu.warn",
            "config.prompt.right.thresholds.battery",
        ] {
            assert!(
                issues.iter().any(|i| i.path == path),
                "missing issue for {path}: {issues:?}"
            );
        }
    }

    #[test]
    fn unknown_fields_are_ignored_with_a_hint() {
        let (_, issues) = parse_prompt(&sec(&[("right", sec(&[("slott1", sec(&[]))]))]));
        let i = issues
            .iter()
            .find(|i| i.path == "config.prompt.right.slott1")
            .expect("issue");
        assert!(
            i.message.contains("unknown field") && i.message.contains("did you mean 'slot1'?"),
            "{}",
            i.message
        );
        let (_, issues) = parse_prompt(&sec(&[("shwoStatus", b(true))]));
        assert!(issues
            .iter()
            .any(|i| i.message.contains("did you mean 'showStatus'?")));
    }

    #[test]
    fn non_section_values_are_ignored() {
        let (p, issues) = parse_prompt(&i(3));
        assert_eq!(p, crate::PromptConfig::default());
        assert_eq!(issues.len(), 1);
        let (p, issues) = parse_prompt(&sec(&[("right", i(3))]));
        assert_eq!(p.right, crate::PromptConfig::default().right);
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn right_wins_over_legacy_keys_with_one_issue() {
        let (p, issues) = parse_prompt(&sec(&[
            ("time", sec(&[("format", s("HH:mm"))])),
            ("right", sec(&[("slot1", sec(&[("text", s("{host}"))]))])),
        ]));
        assert!(matches!(p.right.slots[0], Some(SlotConfig::Ok(_))));
        assert!(p.right.slots[1].is_none());
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("ignored because prompt.right is set")),
            "{issues:?}"
        );
    }

    #[test]
    fn invalid_legacy_time_format_falls_back_softly() {
        let (p, issues) = parse_prompt(&sec(&[("time", sec(&[("format", s("mm:HH"))]))]));
        assert!(issues.iter().any(|i| i.path == "config.prompt.time.format"));
        match &p.right.slots[1] {
            Some(SlotConfig::Ok(spec)) => {
                assert_eq!(spec.template, Template::parse("{time:%H:%M:%S}").unwrap())
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn needed_widgets_only_counts_ok_slots() {
        let (p, _) = parse_prompt(&sec(&[(
            "right",
            sec(&[
                ("slot1", sec(&[("text", s("{cpu} {disk:/home}"))])),
                ("slot2", sec(&[("text", s("{ram}{zzz}"))])),
            ]),
        )]));
        let n = p.right.needed_widgets();
        assert!(n.kinds.contains(&WidgetKind::Cpu) && n.kinds.contains(&WidgetKind::Disk));
        assert!(!n.kinds.contains(&WidgetKind::Ram));
        assert_eq!(n.disk_paths, vec!["/home".to_string()]);
    }
}
