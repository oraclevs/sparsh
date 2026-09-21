use indexmap::IndexMap;
use std::fmt;
use std::path::{Path, PathBuf};

use spar::{ConfigValue, SparError};

use crate::alias::AliasService;
use crate::environment::EnvironmentService;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptPathConfig {
    pub enabled: bool,
    pub parent_length: usize,
    pub max_last_length: usize,
    pub max_width: usize,
}

impl Default for PromptPathConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            parent_length: 2,
            max_last_length: 30,
            // Unbounded by default. The prompt uses the actual terminal width and
            // only applies maxWidth when the user explicitly configures one.
            max_width: usize::MAX,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptGitConfig {
    pub enabled: bool,
    pub show_branch: bool,
    pub show_ahead_behind: bool,
    pub show_staged: bool,
    pub show_modified: bool,
    pub show_untracked: bool,
    pub show_conflicts: bool,
}

impl Default for PromptGitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_branch: true,
            show_ahead_behind: true,
            show_staged: true,
            show_modified: true,
            show_untracked: true,
            show_conflicts: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptTimeConfig {
    pub enabled: bool,
    pub format: String,
}

impl Default for PromptTimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            format: "HH:mm:ss".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptConfig {
    pub show_status: bool,
    pub show_duration: bool,
    pub duration_threshold_ms: u64,
    pub path: PromptPathConfig,
    pub git: PromptGitConfig,
    pub time: PromptTimeConfig,
    /// The right side of the first prompt line. Built from `prompt.right`, or
    /// from the legacy `showDuration`/`time` keys when `right` is absent.
    pub right: crate::prompt_config::RightPromptConfig,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            show_status: true,
            show_duration: true,
            duration_threshold_ms: 2_000,
            path: PromptPathConfig::default(),
            git: PromptGitConfig::default(),
            time: PromptTimeConfig::default(),
            right: crate::prompt_config::RightPromptConfig::legacy_default(true, true, "%H:%M:%S"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryConfig {
    pub path: Option<PathBuf>,
    pub max_entries: usize,
    pub dedupe_consecutive: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            path: None,
            max_entries: 10_000,
            dedupe_consecutive: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionConfig {
    pub enabled: bool,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentVariableConfig {
    pub name: String,
    pub value: Option<String>,
    pub prepend: Vec<String>,
    pub append: Vec<String>,
}

impl EnvironmentVariableConfig {
    pub fn value(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: Some(value.into()),
            prepend: Vec::new(),
            append: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SparshConfig {
    pub aliases: Vec<(String, Vec<String>)>,
    pub environment: Vec<EnvironmentVariableConfig>,
    pub prompt: PromptConfig,
    pub history: HistoryConfig,
    pub completion: CompletionConfig,
    pub keybindings: Vec<crate::KeybindingConfig>,
    /// Problems found in the `prompt` section. They never fail the load: bad
    /// settings fall back to defaults and are reported to the user instead.
    pub prompt_issues: Vec<crate::prompt_config::PromptIssue>,
}

impl SparshConfig {
    pub fn validate(&self) -> Result<(), String> {
        let mut aliases = AliasService::new();
        aliases.define_many(&self.aliases)?;

        let mut environment = EnvironmentService::from_pairs_for_validation();
        environment.ensure_many(
            &self
                .environment
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>(),
        )?;
        for entry in &self.environment {
            let has_value = entry.value.is_some();
            let has_path_edits = !entry.prepend.is_empty() || !entry.append.is_empty();
            if !has_value && !has_path_edits {
                return Err(format!(
                    "environment variable `{}` requires value, prepend, or append",
                    entry.name
                ));
            }
            if has_value && has_path_edits {
                return Err(format!(
                    "environment variable `{}` cannot combine value with prepend/append",
                    entry.name
                ));
            }
            if has_path_edits && entry.name != "PATH" {
                return Err(format!(
                    "environment prepend/append is currently supported only for PATH, not `{}`",
                    entry.name
                ));
            }
        }

        if self.history.max_entries == 0 {
            return Err("history.maxEntries must be greater than zero".into());
        }
        crate::keybinding::validate_keybindings(&self.keybindings)?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum ConfigLoadError {
    Io { path: PathBuf, message: String },
    Spar(Vec<SparError>),
    Invalid(String),
}

impl fmt::Display for ConfigLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(formatter, "failed to read {}: {message}", path.display())
            }
            Self::Spar(errors) => {
                for (index, error) in errors.iter().enumerate() {
                    if index > 0 {
                        writeln!(formatter)?;
                    }
                    write!(formatter, "{error}")?;
                }
                Ok(())
            }
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ConfigLoadError {}

pub(crate) fn config_path(environment: &EnvironmentService) -> Option<PathBuf> {
    environment
        .get("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".sparsh/sparsh.spar"))
}

pub(crate) fn read_source(path: &Path) -> Result<String, ConfigLoadError> {
    match std::fs::read_to_string(path) {
        Ok(source) => Ok(source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(ConfigLoadError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

/// Evaluates the single Sparsh rc file into `session`. The file is ordinary
/// Spar source: it may declare variables/functions/imports and may optionally
/// declare a typed `struct Config { ... };` for declarative shell UI/environment
/// settings. Public config types can live in `~/.sparsh/sparsh-types.spar`; Sparsh
/// validates the resulting section independently. Absence of `Config` means defaults.
pub(crate) fn evaluate_source_in_session(
    session: &mut spar::Session,
    source: &str,
) -> Result<SparshConfig, ConfigLoadError> {
    if !source.trim().is_empty() {
        session.eval(source).map_err(ConfigLoadError::Spar)?;
    }

    let config = match session.section("Config") {
        Some(value) => config_from_value(&value).map_err(ConfigLoadError::Invalid)?,
        None => SparshConfig::default(),
    };
    config.validate().map_err(ConfigLoadError::Invalid)?;
    Ok(config)
}

#[cfg(test)]
pub fn load_candidate(path: &Path) -> Result<SparshConfig, ConfigLoadError> {
    let source = read_source(path)?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut session = spar::Engine::default().with_base_dir(base_dir).session();
    evaluate_source_in_session(&mut session, &source)
}

fn config_from_value(value: &ConfigValue) -> Result<SparshConfig, String> {
    let root = expect_section(value, "config")?;
    if root.contains_key("startup") {
        return Err(
            "config.startup has been removed; define `function startup() -> shell { ... };` instead"
                .into(),
        );
    }
    ensure_allowed_fields(
        root,
        "config",
        &[
            "aliases",
            "environment",
            "prompt",
            "history",
            "completion",
            "keybindings",
        ],
    )?;
    let mut config = SparshConfig::default();

    if let Some(value) = root.get("aliases") {
        config.aliases = expect_list(value, "config.aliases")?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let path = format!("config.aliases[{index}]");
                let alias = expect_section(value, &path)?;
                ensure_allowed_fields(alias, &path, &["name", "command"])?;
                let name = expect_string(required(alias, "name", &path)?, &format!("{path}.name"))?;
                let command = string_list(
                    required(alias, "command", &path)?,
                    &format!("{path}.command"),
                )?;
                Ok((name, command))
            })
            .collect::<Result<Vec<_>, String>>()?;
    }

    if let Some(value) = root.get("environment") {
        config.environment = expect_list(value, "config.environment")?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let path = format!("config.environment[{index}]");
                let entry = expect_section(value, &path)?;
                ensure_allowed_fields(entry, &path, &["name", "value", "prepend", "append"])?;
                let name = expect_string(required(entry, "name", &path)?, &format!("{path}.name"))?;
                let value = entry
                    .get("value")
                    .map(|value| expect_string(value, &format!("{path}.value")))
                    .transpose()?;
                let prepend = entry
                    .get("prepend")
                    .map(|value| string_list(value, &format!("{path}.prepend")))
                    .transpose()?
                    .unwrap_or_default();
                let append = entry
                    .get("append")
                    .map(|value| string_list(value, &format!("{path}.append")))
                    .transpose()?
                    .unwrap_or_default();
                Ok(EnvironmentVariableConfig {
                    name,
                    value,
                    prepend,
                    append,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
    }

    if let Some(value) = root.get("prompt") {
        // Prompt problems are soft: defaults are used for anything invalid and
        // the issues are reported, so a prompt typo never discards the rest of
        // the configuration.
        let (prompt, issues) = crate::prompt_config::parse_prompt(value);
        config.prompt = prompt;
        config.prompt_issues = issues;
    }

    if let Some(value) = root.get("history") {
        let history = expect_section(value, "config.history")?;
        ensure_allowed_fields(
            history,
            "config.history",
            &["path", "maxEntries", "dedupeConsecutive"],
        )?;
        if let Some(value) = history.get("path") {
            config.history.path = Some(PathBuf::from(expect_string(value, "config.history.path")?));
        }
        if let Some(value) = history.get("maxEntries") {
            let entries = expect_int(value, "config.history.maxEntries")?;
            config.history.max_entries = usize::try_from(entries)
                .map_err(|_| "config.history.maxEntries must be greater than zero".to_string())?;
        }
        if let Some(value) = history.get("dedupeConsecutive") {
            config.history.dedupe_consecutive =
                expect_bool(value, "config.history.dedupeConsecutive")?;
        }
    }

    if let Some(value) = root.get("completion") {
        let completion = expect_section(value, "config.completion")?;
        ensure_allowed_fields(completion, "config.completion", &["enabled"])?;
        if let Some(value) = completion.get("enabled") {
            config.completion.enabled = expect_bool(value, "config.completion.enabled")?;
        }
    }

    if let Some(value) = root.get("keybindings") {
        config.keybindings = expect_list(value, "config.keybindings")?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let path = format!("config.keybindings[{index}]");
                let binding = expect_section(value, &path)?;
                ensure_allowed_fields(binding, &path, &["key", "action"])?;
                let key = expect_string(required(binding, "key", &path)?, &format!("{path}.key"))?;
                let action = expect_string(
                    required(binding, "action", &path)?,
                    &format!("{path}.action"),
                )?;
                Ok(crate::KeybindingConfig {
                    chord: crate::KeyChord::parse(&key)?,
                    action: crate::KeybindingAction::parse(&action)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        crate::keybinding::validate_keybindings(&config.keybindings)?;
    }

    Ok(config)
}

fn ensure_allowed_fields(
    section: &IndexMap<String, ConfigValue>,
    path: &str,
    allowed: &[&str],
) -> Result<(), String> {
    for field in section.keys() {
        if !allowed.iter().any(|candidate| *candidate == field) {
            return Err(format!("unsupported Sparsh config field: {path}.{field}"));
        }
    }
    Ok(())
}

fn required<'a>(
    section: &'a IndexMap<String, ConfigValue>,
    field: &str,
    path: &str,
) -> Result<&'a ConfigValue, String> {
    section
        .get(field)
        .ok_or_else(|| format!("{path}.{field} is required"))
}

fn expect_section<'a>(
    value: &'a ConfigValue,
    path: &str,
) -> Result<&'a IndexMap<String, ConfigValue>, String> {
    match value {
        ConfigValue::Section(value) => Ok(value),
        other => Err(format!(
            "{path} must be a section, got {}",
            other.type_name()
        )),
    }
}

fn expect_list<'a>(value: &'a ConfigValue, path: &str) -> Result<&'a [ConfigValue], String> {
    match value {
        ConfigValue::List(value) => Ok(value),
        other => Err(format!("{path} must be a list, got {}", other.type_name())),
    }
}

fn expect_string(value: &ConfigValue, path: &str) -> Result<String, String> {
    match value {
        ConfigValue::Str(value) => Ok(value.clone()),
        other => Err(format!("{path} must be str, got {}", other.type_name())),
    }
}

fn expect_bool(value: &ConfigValue, path: &str) -> Result<bool, String> {
    match value {
        ConfigValue::Bool(value) => Ok(*value),
        other => Err(format!("{path} must be bool, got {}", other.type_name())),
    }
}

fn expect_int(value: &ConfigValue, path: &str) -> Result<i64, String> {
    match value {
        ConfigValue::Int(value) => Ok(*value),
        other => Err(format!("{path} must be int, got {}", other.type_name())),
    }
}

fn string_list(value: &ConfigValue, path: &str) -> Result<Vec<String>, String> {
    expect_list(value, path)?
        .iter()
        .enumerate()
        .map(|(index, value)| expect_string(value, &format!("{path}[{index}]")))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::environment::EnvironmentService;

    fn wrapped_config(body: &str) -> String {
        format!(
            r#"type SparshAlias {{ name: str; command: List<str>; }};
type SparshEnvironmentVariable {{ name: str; value?: str; prepend?: List<str>; append?: List<str>; }};
type SparshPromptPath {{ enabled?: bool; parentLength?: int; maxLastLength?: int; maxWidth?: int; }};
type SparshPromptGit {{ enabled?: bool; showBranch?: bool; showAheadBehind?: bool; showStaged?: bool; showModified?: bool; showUntracked?: bool; showConflicts?: bool; }};
type SparshPromptTime {{ enabled?: bool; format?: str; }};
type SparshPrompt {{ showStatus?: bool; showDuration?: bool; durationThresholdMs?: int; path?: SparshPromptPath; git?: SparshPromptGit; time?: SparshPromptTime; }};
type SparshHistory {{ path?: str; maxEntries?: int; dedupeConsecutive?: bool; }};
type SparshCompletion {{ enabled?: bool; }};
type SparshKeybinding {{ key: str; action: str; }};
struct Config {{
{body}
}};
"#
        )
    }

    /// A config file in the current syntax: the shipped example types plus a
    /// `struct Config: SparshConfig { ... }` body.
    fn example_typed_config(body: &str) -> String {
        format!(
            "{}\nstruct Config: SparshConfig {{\n{body}\n}};\n",
            include_str!("../../../examples/sparsh-types.spar")
        )
    }

    #[test]
    fn invalid_prompt_settings_do_not_discard_the_rest_of_the_config() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sparsh.spar");
        fs::write(
            &file,
            example_typed_config(
                r#"    aliases = [{ name: "gs"; command: ["git", "status"]; }];
    prompt = {
        path: { parentLength: 0; };
        right: { slot2: { text: "{cpuu}"; }; };
    };"#,
            ),
        )
        .unwrap();
        let config = load_candidate(&file).expect("prompt errors are soft");
        assert_eq!(
            config.aliases.len(),
            1,
            "aliases must survive a broken prompt"
        );
        assert_eq!(
            config.prompt.path.parent_length,
            PromptPathConfig::default().parent_length
        );
        assert!(matches!(
            config.prompt.right.slots[1],
            Some(crate::SlotConfig::Broken { .. })
        ));
        assert!(config.prompt_issues.iter().any(|i| i.slot == Some(2)));
        assert!(config
            .prompt_issues
            .iter()
            .any(|i| i.path == "config.prompt.path.parentLength"));
    }

    #[test]
    fn non_prompt_config_errors_are_still_hard_errors() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sparsh.spar");
        fs::write(
            &file,
            example_typed_config(r#"    history = { maxEntries: 0; };"#),
        )
        .unwrap();
        assert!(load_candidate(&file).is_err());
    }

    #[test]
    fn config_path_is_exactly_home_dot_sparsh_sparsh_spar() {
        let environment = EnvironmentService::from_pairs([
            ("XDG_CONFIG_HOME", "/tmp/xdg"),
            ("HOME", "/home/test"),
        ]);
        assert_eq!(
            config_path(&environment),
            Some(PathBuf::from("/home/test/.sparsh/sparsh.spar"))
        );

        let empty = EnvironmentService::from_pairs(std::iter::empty::<(&str, &str)>());
        assert_eq!(config_path(&empty), None);
    }

    #[test]
    fn missing_config_uses_defaults() {
        let dir = tempdir().unwrap();
        let config = load_candidate(&dir.path().join("missing.spar")).unwrap();
        assert_eq!(config, SparshConfig::default());
    }

    #[test]
    fn local_typed_struct_config_loads_all_supported_sections() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparsh.spar");
        fs::write(
            &path,
            wrapped_config(
                r#"    aliases: List<SparshAlias> = [
        { name: "ll"; command: ["ls", "-la"]; }
    ];
    environment: List<SparshEnvironmentVariable> = [
        { name: "EDITOR"; value: "nvim"; },
        { name: "PATH"; prepend: ["$HOME/.local/bin", "$HOME/.cargo/bin"]; append: ["$HOME/.pub-cache/bin"]; }
    ];
    prompt: SparshPrompt = { showDuration: false; };
    history: SparshHistory = { path: "/tmp/sparsh-history"; maxEntries: 5000; dedupeConsecutive: false; };
    completion: SparshCompletion = { enabled: false; };
    keybindings: List<SparshKeybinding> = [
        { key: "ctrl+r"; action: "historySearch"; },
        { key: "alt+e"; action: "openEditor"; }
    ];"#,
            ),
        )
        .unwrap();

        let config = load_candidate(&path).unwrap();

        assert_eq!(
            config.aliases,
            vec![("ll".into(), vec!["ls".into(), "-la".into()])]
        );
        assert_eq!(
            config.environment,
            vec![
                EnvironmentVariableConfig::value("EDITOR", "nvim"),
                EnvironmentVariableConfig {
                    name: "PATH".into(),
                    value: None,
                    prepend: vec!["$HOME/.local/bin".into(), "$HOME/.cargo/bin".into()],
                    append: vec!["$HOME/.pub-cache/bin".into()],
                },
            ]
        );
        assert!(!config.prompt.show_duration);
        assert_eq!(
            config.history.path,
            Some(PathBuf::from("/tmp/sparsh-history"))
        );
        assert_eq!(config.history.max_entries, 5000);
        assert!(!config.history.dedupe_consecutive);
        assert!(!config.completion.enabled);
        assert_eq!(config.keybindings.len(), 2);
        assert_eq!(
            config.keybindings[0].action,
            crate::KeybindingAction::HistorySearch
        );
        assert_eq!(
            config.keybindings[1].action,
            crate::KeybindingAction::OpenEditor
        );
    }

    #[test]
    fn config_file_may_contain_only_spar_declarations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparsh.spar");
        fs::write(
            &path,
            r#"function greet(name: str) -> str { return "Hello ${name}"; };"#,
        )
        .unwrap();

        let config = load_candidate(&path).unwrap();
        assert_eq!(config, SparshConfig::default());
    }

    #[test]
    fn prompt_status_duration_and_path_git_time_fields_load_from_local_types() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparsh.spar");
        fs::write(
            &path,
            wrapped_config(
                r#"    prompt: SparshPrompt = {
        showStatus: false;
        showDuration: true;
        durationThresholdMs: 125;
        path: { enabled: true; parentLength: 1; maxLastLength: 18; maxWidth: 40; };
        git: { enabled: true; showBranch: false; showAheadBehind: false; showStaged: false; showModified: false; showUntracked: false; showConflicts: false; };
        time: { enabled: true; format: "HH:mm"; };
    };"#,
            ),
        )
        .unwrap();

        let config = load_candidate(&path).unwrap();

        assert!(!config.prompt.show_status);
        assert!(config.prompt.show_duration);
        assert_eq!(config.prompt.duration_threshold_ms, 125);
        assert_eq!(config.prompt.path.parent_length, 1);
        assert_eq!(config.prompt.path.max_last_length, 18);
        assert_eq!(config.prompt.path.max_width, 40);
        assert!(!config.prompt.git.show_branch);
        assert!(!config.prompt.git.show_ahead_behind);
        assert!(!config.prompt.git.show_staged);
        assert!(!config.prompt.git.show_modified);
        assert!(!config.prompt.git.show_untracked);
        assert!(!config.prompt.git.show_conflicts);
        assert_eq!(config.prompt.time.format, "HH:mm");
    }

    #[test]
    fn partial_config_fills_rust_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparsh.spar");
        fs::write(
            &path,
            wrapped_config(
                r#"    aliases: List<SparshAlias> = [{ name: "g"; command: ["git"]; }];"#,
            ),
        )
        .unwrap();

        let config = load_candidate(&path).unwrap();

        assert_eq!(config.aliases, vec![("g".into(), vec!["git".into()])]);
        assert_eq!(config.prompt, PromptConfig::default());
        assert_eq!(config.history, HistoryConfig::default());
        assert_eq!(config.completion, CompletionConfig::default());
        assert!(config.keybindings.is_empty());
    }

    #[test]
    fn command_is_a_valid_config_field_name() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparsh.spar");
        fs::write(
            &path,
            wrapped_config(r#"    aliases: List<SparshAlias> = [{ name: "ll"; command: ["eza", "--icons"]; }];"#),
        )
        .unwrap();
        let config = load_candidate(&path).unwrap();
        assert_eq!(config.aliases[0].1, ["eza", "--icons"]);
    }

    #[test]
    fn config_rejects_empty_alias_command_and_zero_history_limit() {
        let empty_alias = SparshConfig {
            aliases: vec![("bad".into(), Vec::new())],
            ..SparshConfig::default()
        };
        assert!(empty_alias
            .validate()
            .unwrap_err()
            .contains("requires a command"));

        let zero_history = SparshConfig {
            history: HistoryConfig {
                max_entries: 0,
                ..HistoryConfig::default()
            },
            ..SparshConfig::default()
        };
        assert!(zero_history
            .validate()
            .unwrap_err()
            .contains("greater than zero"));
    }

    #[test]
    fn syntax_diagnostic_line_is_relative_to_user_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparsh.spar");
        fs::write(
            &path,
            "struct Config {\n    prompt: section = { showDuration: bool = ; };\n};",
        )
        .unwrap();

        let error = load_candidate(&path).unwrap_err();
        let ConfigLoadError::Spar(errors) = error else {
            panic!("expected Spar diagnostic");
        };
        let text = errors.first().unwrap().to_string();
        assert!(text.contains("2:"), "unexpected diagnostic: {text}");
    }

    #[test]
    fn canonical_local_types_config_loads_without_bundled_package_types() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("sparsh-types.spar"),
            r#"export type SparshAlias {
    name: str;
    command: List<str>;
};
export type SparshPromptPath {
    enabled?: bool;
};
export type SparshPrompt {
    showDuration?: bool;
    path?: SparshPromptPath;
};
export type SparshConfig {
    aliases?: List<SparshAlias>;
    prompt?: SparshPrompt;
};
"#,
        )
        .unwrap();
        let path = dir.path().join("sparsh.spar");
        fs::write(
            &path,
            r#"import type { SparshAlias, SparshPromptPath, SparshPrompt, SparshConfig } from "./sparsh-types";
struct Config: SparshConfig {
    aliases = [
        { name: "ll"; command: ["eza", "--icons"]; }
    ];
    prompt = {
        showDuration: false;
        path: { enabled: true; };
    };
};
"#,
        )
        .unwrap();

        let config = load_candidate(&path).unwrap();
        assert_eq!(
            config.aliases,
            vec![("ll".into(), vec!["eza".into(), "--icons".into()])]
        );
        assert!(!config.prompt.show_duration);
        assert!(config.prompt.path.enabled);
    }

    #[test]
    fn startup_config_field_is_rejected_in_favor_of_startup_function() {
        let value = ConfigValue::Section(IndexMap::from([(
            "startup".into(),
            ConfigValue::Section(IndexMap::from([(
                "commands".into(),
                ConfigValue::List(vec![ConfigValue::Str("nitch".into())]),
            )])),
        )]));

        let error = config_from_value(&value).unwrap_err();
        assert!(error.contains("config.startup has been removed"), "{error}");
        assert!(error.contains("function startup()"), "{error}");
    }

    #[test]
    fn environment_path_edits_require_path_and_cannot_mix_with_value() {
        let invalid_name = SparshConfig {
            environment: vec![EnvironmentVariableConfig {
                name: "EDITOR".into(),
                value: None,
                prepend: vec!["/tools".into()],
                append: Vec::new(),
            }],
            ..SparshConfig::default()
        };
        assert!(invalid_name
            .validate()
            .unwrap_err()
            .contains("supported only for PATH"));

        let mixed = SparshConfig {
            environment: vec![EnvironmentVariableConfig {
                name: "PATH".into(),
                value: Some("/bin".into()),
                prepend: vec!["/tools".into()],
                append: Vec::new(),
            }],
            ..SparshConfig::default()
        };
        assert!(mixed
            .validate()
            .unwrap_err()
            .contains("cannot combine value with prepend/append"));
    }

    #[test]
    fn config_rejects_fields_sparsh_does_not_implement() {
        let value = ConfigValue::Section(IndexMap::from([
            ("aliases".into(), ConfigValue::List(Vec::new())),
            (
                "futureField".into(),
                ConfigValue::Str("not-supported".into()),
            ),
        ]));

        let error = config_from_value(&value).unwrap_err();
        assert!(error.contains("futureField"), "{error}");
        assert!(error.contains("unsupported"), "{error}");
    }

    #[test]
    fn environment_names_are_validated_before_candidate_is_returned() {
        let config = SparshConfig {
            environment: vec![EnvironmentVariableConfig::value("BAD-NAME", "x")],
            ..SparshConfig::default()
        };
        assert!(config.validate().unwrap_err().contains("BAD-NAME"));

        let environment = EnvironmentService::from_pairs([("HOME", "/home/test")]);
        assert_eq!(environment.get("HOME"), Some(OsStr::new("/home/test")));
    }
    #[test]
    fn keybinding_config_rejects_unknown_actions_invalid_keys_and_duplicates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparsh.spar");

        fs::write(
            &path,
            wrapped_config(
                r#"    keybindings: List<SparshKeybinding> = [{ key: "ctrl+r"; action: "runClosure"; }];"#,
            ),
        )
        .unwrap();
        let error = load_candidate(&path).unwrap_err().to_string();
        assert!(error.contains("unknown keybinding action"), "{error}");

        fs::write(
            &path,
            wrapped_config(
                r#"    keybindings: List<SparshKeybinding> = [{ key: "meta+wat"; action: "historySearch"; }];"#,
            ),
        )
        .unwrap();
        let error = load_candidate(&path).unwrap_err().to_string();
        assert!(
            error.contains("modifier") || error.contains("unsupported"),
            "{error}"
        );

        fs::write(
            &path,
            wrapped_config(
                r#"    keybindings: List<SparshKeybinding> = [
        { key: "ctrl+r"; action: "historySearch"; },
        { key: "control+r"; action: "clearScreen"; }
    ];"#,
            ),
        )
        .unwrap();
        let error = load_candidate(&path).unwrap_err().to_string();
        assert!(error.contains("duplicate keybinding chord"), "{error}");
    }
}
