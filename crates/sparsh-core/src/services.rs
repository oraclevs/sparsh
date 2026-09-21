use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use crate::alias::AliasService;
use crate::directory::DirectoryService;
use crate::environment::EnvironmentService;
use crate::job::JobTable;
use crate::path::PathService;
use crate::resolver::CommandResolver;
use crate::{EnvironmentVariableConfig, SparshConfig};

pub(crate) struct ShellServices {
    pub aliases: AliasService,
    pub directories: DirectoryService,
    pub environment: EnvironmentService,
    pub path: PathService,
    pub resolver: CommandResolver,
    pub jobs: JobTable,
    pub history: Option<Arc<dyn crate::history::HistoryAccess>>,
    pub login_shell: bool,
    config_alias_originals: BTreeMap<String, Option<Vec<String>>>,
    config_environment_originals: BTreeMap<String, Option<OsString>>,
    python_activation_stack: Vec<Vec<(OsString, Option<OsString>)>>,
}

impl ShellServices {
    pub(crate) fn from_process() -> Result<Self, String> {
        let mut environment = EnvironmentService::from_current();
        let path = PathService::from_environment(environment.get("PATH"));
        let directories = DirectoryService::new()?;
        environment.set_os("PWD", directories.current().as_os_str());
        Ok(Self {
            aliases: AliasService::new(),
            directories,
            environment,
            path,
            resolver: CommandResolver::new(),
            jobs: JobTable::new(),
            history: None,
            login_shell: false,
            config_alias_originals: BTreeMap::new(),
            config_environment_originals: BTreeMap::new(),
            python_activation_stack: Vec::new(),
        })
    }

    pub(crate) fn snapshot_for_isolated_stage(&self) -> Self {
        Self {
            aliases: self.aliases.clone(),
            directories: self.directories.clone(),
            environment: self.environment.clone(),
            path: self.path.clone(),
            resolver: self.resolver.clone(),
            jobs: JobTable::new(),
            history: self.history.clone(),
            login_shell: self.login_shell,
            config_alias_originals: self.config_alias_originals.clone(),
            config_environment_originals: self.config_environment_originals.clone(),
            python_activation_stack: self.python_activation_stack.clone(),
        }
    }

    pub(crate) fn apply_config(&mut self, config: &SparshConfig) -> Result<(), String> {
        config.validate()?;

        let mut aliases = self.aliases.clone();
        for (name, original) in &self.config_alias_originals {
            restore_alias(&mut aliases, name, original.as_deref())?;
        }

        let mut environment = self.environment.clone();
        for (name, original) in &self.config_environment_originals {
            restore_environment(&mut environment, name, original.as_deref())?;
        }

        let mut alias_originals = BTreeMap::new();
        for (name, _) in &config.aliases {
            alias_originals.insert(name.clone(), aliases.get(name).map(|words| words.to_vec()));
        }
        aliases.define_many(&config.aliases)?;

        let mut environment_originals = BTreeMap::new();
        for entry in &config.environment {
            environment_originals
                .entry(entry.name.clone())
                .or_insert_with(|| environment.get(&entry.name).map(OsString::from));
        }
        apply_environment_config(
            &mut environment,
            &config.environment,
            self.directories.current(),
        )?;
        // PWD is owned by the live shell directory service, not config.spar.
        environment.set_os("PWD", self.directories.current().as_os_str());

        let path = PathService::from_environment(environment.get("PATH"));

        self.aliases = aliases;
        self.environment = environment;
        self.path = path;
        self.resolver.clear();
        self.config_alias_originals = alias_originals;
        self.config_environment_originals = environment_originals;
        Ok(())
    }

    pub(crate) fn sync_path_environment(&mut self) -> Result<(), String> {
        let value = self.path.to_environment()?;
        self.environment.set_os("PATH", value);
        Ok(())
    }

    pub(crate) fn reload_path_from_environment(&mut self) {
        self.path = PathService::from_environment(self.environment.get("PATH"));
    }

    pub(crate) fn record_python_activation(
        &mut self,
        before: &[(OsString, OsString)],
        after: &[(OsString, OsString)],
    ) {
        let before = before.iter().cloned().collect::<BTreeMap<_, _>>();
        let after = after.iter().cloned().collect::<BTreeMap<_, _>>();
        let before_virtual_env = before.get(std::ffi::OsStr::new("VIRTUAL_ENV"));
        let after_virtual_env = after.get(std::ffi::OsStr::new("VIRTUAL_ENV"));

        if after_virtual_env.is_none() || after_virtual_env == before_virtual_env {
            return;
        }

        let mut restore = BTreeMap::<OsString, Option<OsString>>::new();
        for (name, value) in &before {
            if name.as_os_str() == std::ffi::OsStr::new("PWD") {
                continue;
            }
            if after.get(name) != Some(value) {
                restore.insert(name.clone(), Some(value.clone()));
            }
        }
        for name in after.keys() {
            if name.as_os_str() == std::ffi::OsStr::new("PWD") {
                continue;
            }
            if !before.contains_key(name) {
                restore.insert(name.clone(), None);
            }
        }

        self.python_activation_stack
            .push(restore.into_iter().collect());
    }

    pub(crate) fn deactivate_python_environment(&mut self) -> Result<(), String> {
        if self.environment.get("VIRTUAL_ENV").is_none() {
            return Err("deactivate: no active Python virtual environment".into());
        }
        let restore = self.python_activation_stack.pop().ok_or_else(|| {
            "deactivate: active Python environment was not activated through `source` in this Sparsh session".to_string()
        })?;

        let mut current = self
            .environment
            .snapshot()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        for (name, old_value) in restore {
            match old_value {
                Some(value) => {
                    current.insert(name, value);
                }
                None => {
                    current.remove(&name);
                }
            }
        }

        self.environment
            .replace_snapshot(current.into_iter().collect());
        self.reload_path_from_environment();
        self.path.invalidate_executable_cache();
        self.resolver.clear();
        Ok(())
    }

    pub(crate) fn set_history_access(&mut self, history: Arc<dyn crate::history::HistoryAccess>) {
        self.history = Some(history);
    }
}

fn apply_environment_config(
    environment: &mut EnvironmentService,
    entries: &[EnvironmentVariableConfig],
    cwd: &Path,
) -> Result<(), String> {
    for entry in entries {
        if let Some(value) = &entry.value {
            let expanded = expand_environment_references(value, environment)?;
            environment.set_os(&entry.name, expanded);
            continue;
        }

        // Validation guarantees prepend/append entries are PATH entries.
        let mut path = PathService::from_environment(environment.get("PATH"));
        let home = environment.get("HOME").map(Path::new);

        // PathService::prepend inserts at index 0. Expand and de-duplicate
        // first, then apply in reverse so the first configured entry retains
        // the highest priority.
        let mut prepend = Vec::new();
        for value in &entry.prepend {
            let expanded = expand_environment_references(value, environment)?;
            if !prepend.contains(&expanded) {
                prepend.push(expanded);
            }
        }
        for value in prepend.iter().rev() {
            path.prepend(Path::new(value), cwd, home)?;
        }
        for value in &entry.append {
            let expanded = expand_environment_references(value, environment)?;
            path.append(Path::new(&expanded), cwd, home)?;
        }
        environment.set_os("PATH", path.to_environment()?);
    }
    Ok(())
}

fn expand_environment_references(
    input: &str,
    environment: &EnvironmentService,
) -> Result<String, String> {
    let mut output = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'$' {
            let ch = input[index..]
                .chars()
                .next()
                .expect("index always points at a character boundary");
            output.push(ch);
            index += ch.len_utf8();
            continue;
        }

        if bytes.get(index + 1) == Some(&b'(') {
            return Err(format!(
                "command substitution is not supported in environment config: `{input}`; use startup() for dynamic values"
            ));
        }

        let (name, consumed) = if bytes.get(index + 1) == Some(&b'{') {
            let remainder = &input[index + 2..];
            let end = remainder
                .find('}')
                .ok_or_else(|| format!("unterminated environment reference in `{input}`"))?;
            (&remainder[..end], end + 3)
        } else {
            let mut end = index + 1;
            while end < bytes.len() && (bytes[end] == b'_' || bytes[end].is_ascii_alphanumeric()) {
                end += 1;
            }
            if end == index + 1 {
                output.push('$');
                index += 1;
                continue;
            }
            (&input[index + 1..end], end - index)
        };

        if !crate::environment::valid_name(name) {
            return Err(format!("invalid environment reference `${name}`"));
        }
        let value = environment
            .get(name)
            .ok_or_else(|| format!("environment variable `{name}` is not set"))?;
        let value = value
            .to_str()
            .ok_or_else(|| format!("environment variable `{name}` is not valid UTF-8"))?;
        output.push_str(value);
        index += consumed;
    }

    Ok(output)
}

fn restore_alias(
    aliases: &mut AliasService,
    name: &str,
    original: Option<&[String]>,
) -> Result<(), String> {
    match original {
        Some(words) => aliases.define(name, words.to_vec()),
        None if aliases.get(name).is_some() => aliases.remove_many(&[name.to_string()]),
        None => Ok(()),
    }
}

fn restore_environment(
    environment: &mut EnvironmentService,
    name: &str,
    original: Option<&std::ffi::OsStr>,
) -> Result<(), String> {
    match original {
        Some(value) => {
            environment.set_os(name, value);
            Ok(())
        }
        None => environment.unset_many(&[name.to_string()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_config_replaces_only_previous_config_overlay() {
        let mut services = ShellServices::from_process().unwrap();
        services
            .aliases
            .define("keep", vec!["original".into()])
            .unwrap();
        services
            .environment
            .set_many(&[("KEEP_ENV".into(), "original".into())])
            .unwrap();

        let first = SparshConfig {
            aliases: vec![
                ("keep".into(), vec!["configured".into()]),
                ("new".into(), vec!["true".into()]),
            ],
            environment: vec![
                EnvironmentVariableConfig::value("KEEP_ENV", "configured"),
                EnvironmentVariableConfig::value("NEW_ENV", "one"),
            ],
            ..SparshConfig::default()
        };
        services.apply_config(&first).unwrap();
        assert_eq!(services.aliases.get("keep").unwrap(), ["configured"]);
        assert_eq!(services.environment.get("KEEP_ENV").unwrap(), "configured");

        let second = SparshConfig::default();
        services.apply_config(&second).unwrap();

        assert_eq!(services.aliases.get("keep").unwrap(), ["original"]);
        assert!(services.aliases.get("new").is_none());
        assert_eq!(services.environment.get("KEEP_ENV").unwrap(), "original");
        assert!(services.environment.get("NEW_ENV").is_none());
    }

    #[test]
    fn environment_config_expands_static_values_and_edits_path_in_declared_order() {
        let mut services = ShellServices::from_process().unwrap();
        services.environment.set_os("HOME", "/home/test");
        services.environment.set_os("PATH", "/usr/bin:/bin");
        services.reload_path_from_environment();

        let config = SparshConfig {
            environment: vec![
                EnvironmentVariableConfig::value("EDITOR", "$HOME/bin/nvim"),
                EnvironmentVariableConfig {
                    name: "PATH".into(),
                    value: None,
                    prepend: vec![
                        "$HOME/.local/bin".into(),
                        "$HOME/.cargo/bin".into(),
                        "$HOME/.local/bin".into(),
                    ],
                    append: vec!["$HOME/.pub-cache/bin".into()],
                },
            ],
            ..SparshConfig::default()
        };

        services.apply_config(&config).unwrap();

        assert_eq!(
            services.environment.get("EDITOR").unwrap(),
            "/home/test/bin/nvim"
        );
        assert_eq!(
            services.path.directories(),
            [
                Path::new("/home/test/.local/bin"),
                Path::new("/home/test/.cargo/bin"),
                Path::new("/usr/bin"),
                Path::new("/bin"),
                Path::new("/home/test/.pub-cache/bin"),
            ]
        );
    }

    #[test]
    fn removing_config_restores_the_original_path_overlay() {
        let mut services = ShellServices::from_process().unwrap();
        services.environment.set_os("HOME", "/home/test");
        services.environment.set_os("PATH", "/usr/bin:/bin");
        services.reload_path_from_environment();

        let config = SparshConfig {
            environment: vec![EnvironmentVariableConfig {
                name: "PATH".into(),
                value: None,
                prepend: vec!["$HOME/.cargo/bin".into()],
                append: Vec::new(),
            }],
            ..SparshConfig::default()
        };
        services.apply_config(&config).unwrap();
        assert_eq!(
            services.environment.get("PATH").unwrap(),
            "/home/test/.cargo/bin:/usr/bin:/bin"
        );

        services.apply_config(&SparshConfig::default()).unwrap();
        assert_eq!(services.environment.get("PATH").unwrap(), "/usr/bin:/bin");
    }

    #[test]
    fn missing_environment_reference_rejects_config_without_mutating_live_state() {
        let mut services = ShellServices::from_process().unwrap();
        services.environment.set_os("KEEP", "old");
        let before = services.environment.snapshot();

        let config = SparshConfig {
            environment: vec![EnvironmentVariableConfig::value(
                "KEEP",
                "${SPARSH_TEST_MISSING_VARIABLE}/bin",
            )],
            ..SparshConfig::default()
        };

        let error = services.apply_config(&config).unwrap_err();
        assert!(error.contains("SPARSH_TEST_MISSING_VARIABLE"));
        assert_eq!(services.environment.snapshot(), before);
    }

    #[test]
    fn invalid_candidate_does_not_mutate_active_state() {
        let mut services = ShellServices::from_process().unwrap();
        services
            .aliases
            .define("keep", vec!["true".into()])
            .unwrap();
        let before_path = services.path.directories().to_vec();

        let invalid = SparshConfig {
            aliases: vec![("bad/name".into(), vec!["false".into()])],
            ..SparshConfig::default()
        };

        assert!(services.apply_config(&invalid).is_err());
        assert_eq!(services.aliases.get("keep").unwrap(), ["true"]);
        assert_eq!(services.path.directories(), before_path);
    }
}
