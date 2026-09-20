use std::path::PathBuf;

use crate::environment::EnvironmentService;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySettings {
    pub path: PathBuf,
    pub max_entries: usize,
    pub ignore_consecutive_duplicates: bool,
}

impl HistorySettings {
    pub(crate) fn from_config(
        environment: &EnvironmentService,
        config: &crate::HistoryConfig,
    ) -> Self {
        Self {
            path: config
                .path
                .clone()
                .unwrap_or_else(|| default_history_path(environment)),
            max_entries: config.max_entries,
            ignore_consecutive_duplicates: config.dedupe_consecutive,
        }
    }
}

pub(crate) fn default_history_path(environment: &EnvironmentService) -> PathBuf {
    if let Some(state) = environment.get("XDG_STATE_HOME") {
        return PathBuf::from(state).join("sparsh/history");
    }
    if let Some(home) = environment.get("HOME") {
        return PathBuf::from(home).join(".local/state/sparsh/history");
    }
    PathBuf::from(".sparsh_history")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_state_home_wins_for_history_path() {
        let mut environment = EnvironmentService::from_pairs_for_validation();
        environment.set_os("HOME", "/home/occ");
        environment.set_os("XDG_STATE_HOME", "/state");
        assert_eq!(default_history_path(&environment), PathBuf::from("/state/sparsh/history"));
    }

    #[test]
    fn history_falls_back_to_home_local_state() {
        let mut environment = EnvironmentService::from_pairs_for_validation();
        environment.set_os("HOME", "/home/occ");
        assert_eq!(
            default_history_path(&environment),
            PathBuf::from("/home/occ/.local/state/sparsh/history")
        );
    }
}

pub trait HistoryAccess: Send + Sync {
    fn list(&self, limit: Option<usize>) -> Result<Vec<String>, String>;
    fn clear(&self) -> Result<(), String>;
}
