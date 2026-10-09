use std::collections::HashSet;

use sparsh_core::ThemeRole;

#[test]
fn role_paths_are_unique_and_cover_new_ui_parts() {
    let paths: HashSet<_> = ThemeRole::ALL
        .iter()
        .map(|role| role.config_path())
        .collect();
    assert_eq!(paths.len(), ThemeRole::ALL.len());
    assert_eq!(ThemeRole::Cwd.config_path(), "prompt.cwd");
    assert_eq!(
        ThemeRole::MenuSelected.config_path(),
        "completion.menuSelected"
    );
    assert_eq!(
        ThemeRole::HintHistory.config_path(),
        "completion.hintHistory"
    );
    assert_eq!(ThemeRole::EditorHeader.config_path(), "editor.header");
    assert_eq!(ThemeRole::PagerStatus.config_path(), "pager.status");
}
