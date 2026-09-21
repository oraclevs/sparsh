use std::collections::HashSet;

pub const KEYBINDING_ACTION_NAMES: &[&str] = &[
    "completion",
    "historyMenu",
    "historySearch",
    "openEditor",
    "clearScreen",
    "insertNewline",
    "submit",
    "cancel",
    "eof",
    "previousHistory",
    "nextHistory",
    "up",
    "down",
    "left",
    "right",
    "toStart",
    "toEnd",
    "pager",
];

pub const PAGER_ACTION_NAMES: &[&str] = &[
    "lineDown",
    "lineUp",
    "pageDown",
    "pageUp",
    "halfPageDown",
    "halfPageUp",
    "top",
    "bottom",
    "left",
    "right",
    "search",
    "searchNext",
    "searchPrevious",
    "quit",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeybindingKey {
    Char(char),
    Tab,
    BackTab,
    Enter,
    Esc,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Function(u8),
}

impl std::fmt::Display for KeyChord {
    /// The config spelling, e.g. `ctrl+alt+l`, so messages match what users wrote.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.control {
            formatter.write_str("ctrl+")?;
        }
        if self.alt {
            formatter.write_str("alt+")?;
        }
        if self.shift {
            formatter.write_str("shift+")?;
        }
        match self.key {
            KeybindingKey::Char(character) => write!(formatter, "{character}"),
            KeybindingKey::Function(number) => write!(formatter, "f{number}"),
            KeybindingKey::Tab => formatter.write_str("tab"),
            KeybindingKey::BackTab => formatter.write_str("shift+tab"),
            KeybindingKey::Enter => formatter.write_str("enter"),
            KeybindingKey::Esc => formatter.write_str("esc"),
            KeybindingKey::Backspace => formatter.write_str("backspace"),
            KeybindingKey::Delete => formatter.write_str("delete"),
            KeybindingKey::Insert => formatter.write_str("insert"),
            KeybindingKey::Left => formatter.write_str("left"),
            KeybindingKey::Right => formatter.write_str("right"),
            KeybindingKey::Up => formatter.write_str("up"),
            KeybindingKey::Down => formatter.write_str("down"),
            KeybindingKey::Home => formatter.write_str("home"),
            KeybindingKey::End => formatter.write_str("end"),
            KeybindingKey::PageUp => formatter.write_str("pageup"),
            KeybindingKey::PageDown => formatter.write_str("pagedown"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: KeybindingKey,
}

impl KeyChord {
    pub fn parse(source: &str) -> Result<Self, String> {
        let source = source.trim();
        if source.is_empty() {
            return Err("keybinding key must not be empty".into());
        }

        let parts = source.split('+').map(str::trim).collect::<Vec<_>>();
        if parts.iter().any(|part| part.is_empty()) {
            return Err(format!("invalid keybinding key `{source}`"));
        }

        let (key_name, modifier_names) = parts
            .split_last()
            .ok_or_else(|| "keybinding key must not be empty".to_string())?;
        let mut control = false;
        let mut alt = false;
        let mut shift = false;

        for modifier in modifier_names {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => {
                    if control {
                        return Err(format!(
                            "duplicate control modifier in keybinding `{source}`"
                        ));
                    }
                    control = true;
                }
                "alt" | "option" => {
                    if alt {
                        return Err(format!("duplicate alt modifier in keybinding `{source}`"));
                    }
                    alt = true;
                }
                "shift" => {
                    if shift {
                        return Err(format!("duplicate shift modifier in keybinding `{source}`"));
                    }
                    shift = true;
                }
                other => {
                    return Err(format!(
                        "unsupported keybinding modifier `{other}` in `{source}`; use ctrl, alt, or shift"
                    ));
                }
            }
        }

        let mut key = parse_key_name(key_name, source)?;
        if shift && matches!(key, KeybindingKey::Tab) {
            key = KeybindingKey::BackTab;
        }

        Ok(Self {
            control,
            alt,
            shift,
            key,
        })
    }
}

fn parse_key_name(key_name: &str, source: &str) -> Result<KeybindingKey, String> {
    let lower = key_name.to_ascii_lowercase();
    let named = match lower.as_str() {
        "tab" => Some(KeybindingKey::Tab),
        "backtab" => Some(KeybindingKey::BackTab),
        "enter" | "return" => Some(KeybindingKey::Enter),
        "esc" | "escape" => Some(KeybindingKey::Esc),
        "backspace" => Some(KeybindingKey::Backspace),
        "delete" | "del" => Some(KeybindingKey::Delete),
        "insert" | "ins" => Some(KeybindingKey::Insert),
        "left" => Some(KeybindingKey::Left),
        "right" => Some(KeybindingKey::Right),
        "up" => Some(KeybindingKey::Up),
        "down" => Some(KeybindingKey::Down),
        "home" => Some(KeybindingKey::Home),
        "end" => Some(KeybindingKey::End),
        "pageup" => Some(KeybindingKey::PageUp),
        "pagedown" => Some(KeybindingKey::PageDown),
        "space" => Some(KeybindingKey::Char(' ')),
        _ => None,
    };
    if let Some(key) = named {
        return Ok(key);
    }

    if let Some(number) = lower.strip_prefix('f') {
        if let Ok(number) = number.parse::<u8>() {
            if (1..=24).contains(&number) {
                return Ok(KeybindingKey::Function(number));
            }
        }
    }

    let mut chars = key_name.chars();
    if let (Some(character), None) = (chars.next(), chars.next()) {
        return Ok(KeybindingKey::Char(character));
    }

    Err(format!(
        "unsupported keybinding key `{key_name}` in `{source}`"
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeybindingAction {
    Completion,
    HistoryMenu,
    HistorySearch,
    OpenEditor,
    ClearScreen,
    InsertNewline,
    Submit,
    Cancel,
    Eof,
    PreviousHistory,
    NextHistory,
    Up,
    Down,
    Left,
    Right,
    ToStart,
    ToEnd,
    /// Opens the pager over the last structured value (runs `view`).
    Pager,
}

impl KeybindingAction {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "completion" => Ok(Self::Completion),
            "historyMenu" => Ok(Self::HistoryMenu),
            "historySearch" => Ok(Self::HistorySearch),
            "openEditor" => Ok(Self::OpenEditor),
            "clearScreen" => Ok(Self::ClearScreen),
            "insertNewline" => Ok(Self::InsertNewline),
            "submit" => Ok(Self::Submit),
            "cancel" => Ok(Self::Cancel),
            "eof" => Ok(Self::Eof),
            "previousHistory" => Ok(Self::PreviousHistory),
            "nextHistory" => Ok(Self::NextHistory),
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "toStart" => Ok(Self::ToStart),
            "toEnd" => Ok(Self::ToEnd),
            "pager" => Ok(Self::Pager),
            _ => Err(format!(
                "unknown keybinding action `{name}`; supported actions: {}",
                KEYBINDING_ACTION_NAMES.join(", ")
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeybindingConfig {
    pub chord: KeyChord,
    pub action: KeybindingAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagerAction {
    LineDown,
    LineUp,
    PageDown,
    PageUp,
    HalfPageDown,
    HalfPageUp,
    Top,
    Bottom,
    Left,
    Right,
    Search,
    SearchNext,
    SearchPrevious,
    Quit,
}

impl PagerAction {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "lineDown" => Ok(Self::LineDown),
            "lineUp" => Ok(Self::LineUp),
            "pageDown" => Ok(Self::PageDown),
            "pageUp" => Ok(Self::PageUp),
            "halfPageDown" => Ok(Self::HalfPageDown),
            "halfPageUp" => Ok(Self::HalfPageUp),
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "search" => Ok(Self::Search),
            "searchNext" => Ok(Self::SearchNext),
            "searchPrevious" => Ok(Self::SearchPrevious),
            "quit" => Ok(Self::Quit),
            _ => Err(format!(
                "unknown pager action `{name}`; supported actions: {}",
                PAGER_ACTION_NAMES.join(", ")
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagerKeybindingConfig {
    pub chord: KeyChord,
    pub action: PagerAction,
}

/// The pager's built-in keys (less/vim style). `pagerKeybindings` in the
/// config adds to these and replaces any default on the same chord.
pub fn default_pager_keybindings() -> Vec<PagerKeybindingConfig> {
    const DEFAULTS: &[(&str, PagerAction)] = &[
        ("j", PagerAction::LineDown),
        ("down", PagerAction::LineDown),
        ("enter", PagerAction::LineDown),
        ("k", PagerAction::LineUp),
        ("up", PagerAction::LineUp),
        ("space", PagerAction::PageDown),
        ("pagedown", PagerAction::PageDown),
        ("ctrl+f", PagerAction::PageDown),
        ("b", PagerAction::PageUp),
        ("pageup", PagerAction::PageUp),
        ("ctrl+b", PagerAction::PageUp),
        ("d", PagerAction::HalfPageDown),
        ("ctrl+d", PagerAction::HalfPageDown),
        ("u", PagerAction::HalfPageUp),
        ("ctrl+u", PagerAction::HalfPageUp),
        ("g", PagerAction::Top),
        ("home", PagerAction::Top),
        ("G", PagerAction::Bottom),
        ("end", PagerAction::Bottom),
        ("h", PagerAction::Left),
        ("left", PagerAction::Left),
        ("l", PagerAction::Right),
        ("right", PagerAction::Right),
        ("/", PagerAction::Search),
        ("n", PagerAction::SearchNext),
        ("N", PagerAction::SearchPrevious),
        ("q", PagerAction::Quit),
        ("esc", PagerAction::Quit),
        ("ctrl+c", PagerAction::Quit),
    ];
    DEFAULTS
        .iter()
        .map(|(key, action)| PagerKeybindingConfig {
            chord: KeyChord::parse(key).expect("built-in pager chord"),
            action: *action,
        })
        .collect()
}

/// Defaults with user bindings applied on top.
pub fn merged_pager_keybindings(user: &[PagerKeybindingConfig]) -> Vec<PagerKeybindingConfig> {
    let mut merged = default_pager_keybindings();
    merged.retain(|binding| user.iter().all(|custom| custom.chord != binding.chord));
    merged.extend(user.iter().cloned());
    merged
}

pub(crate) fn validate_pager_keybindings(bindings: &[PagerKeybindingConfig]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for binding in bindings {
        if !seen.insert(binding.chord) {
            return Err(format!(
                "duplicate pager keybinding chord in config: `{}`",
                binding.chord
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_keybindings(bindings: &[KeybindingConfig]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for binding in bindings {
        if !seen.insert(binding.chord) {
            return Err(format!(
                "duplicate keybinding chord in config: `{}`",
                binding.chord
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_chords_and_normalizes_shift_tab() {
        assert_eq!(
            KeyChord::parse("ctrl+alt+r").unwrap(),
            KeyChord {
                control: true,
                alt: true,
                shift: false,
                key: KeybindingKey::Char('r'),
            }
        );
        assert_eq!(
            KeyChord::parse("shift+tab").unwrap(),
            KeyChord {
                control: false,
                alt: false,
                shift: true,
                key: KeybindingKey::BackTab,
            }
        );
        assert_eq!(
            KeyChord::parse("F12").unwrap().key,
            KeybindingKey::Function(12)
        );
    }

    #[test]
    fn parses_insert_newline_action_and_alt_shift_enter() {
        assert_eq!(
            KeybindingAction::parse("insertNewline").unwrap(),
            KeybindingAction::InsertNewline
        );
        assert_eq!(
            KeyChord::parse("alt+shift+enter").unwrap(),
            KeyChord {
                control: false,
                alt: true,
                shift: true,
                key: KeybindingKey::Enter,
            }
        );
    }

    #[test]
    fn duplicate_chords_are_reported_in_config_spelling() {
        let binding = |action| KeybindingConfig {
            chord: KeyChord::parse("ctrl+l").unwrap(),
            action,
        };
        let error = validate_keybindings(&[
            binding(KeybindingAction::ClearScreen),
            binding(KeybindingAction::Cancel),
        ])
        .unwrap_err();
        assert!(error.contains("`ctrl+l`"), "{error}");
    }

    #[test]
    fn user_pager_bindings_replace_defaults_on_the_same_chord() {
        let custom = PagerKeybindingConfig {
            chord: KeyChord::parse("j").unwrap(),
            action: PagerAction::PageDown,
        };
        let merged = merged_pager_keybindings(std::slice::from_ref(&custom));
        let for_j = merged
            .iter()
            .filter(|binding| binding.chord == custom.chord)
            .collect::<Vec<_>>();
        assert_eq!(for_j, [&custom]);
        assert!(merged
            .iter()
            .any(|binding| binding.action == PagerAction::Quit));
        assert!(PagerAction::parse("scrollSideways")
            .unwrap_err()
            .contains("lineDown"));
    }

    #[test]
    fn rejects_unknown_modifiers_keys_and_actions() {
        assert!(KeyChord::parse("meta+x").unwrap_err().contains("modifier"));
        assert!(KeyChord::parse("ctrl+not-a-key")
            .unwrap_err()
            .contains("unsupported"));
        let error = KeybindingAction::parse("runClosure").unwrap_err();
        assert!(error.contains("unknown keybinding action"));
        assert!(error.contains("openEditor"));
    }
}
