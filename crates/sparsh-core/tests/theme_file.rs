use sparsh_core::{ColorSpec, PaletteKey, ThemeFileState};
use std::fs::{self, File, FileTimes};
use std::path::Path;

fn setup(home: &Path) {
    fs::create_dir_all(home.join(".sparsh/src")).unwrap();
}

fn theme_path(home: &Path) -> std::path::PathBuf {
    home.join(".sparsh/src/theme.generated.spar")
}

fn source(color: &str) -> String {
    format!(
        r##"var themeState: Record = {{
    source: {{ kind: "external"; origin: "test"; }};
    theme: {{ palette: {{ accent: "{color}"; }}; }};
}};
"##
    )
}

const STATE: &str = r##"var themeState: Record = {
    source: { kind: "registered"; name: "gruvbox"; origin: "sparsh-themes"; };
    theme: { palette: { accent: "#fabd2f"; }; prompt: { cwd: { foreground: "#83a598"; bold: true; }; }; };
};
"##;

#[test]
fn state_file_loads_layer_and_source() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path());
    fs::write(theme_path(home.path()), STATE).unwrap();
    let mut state = ThemeFileState::default();
    let refresh = state.refresh(home.path(), false);
    assert!(refresh.changed, "{refresh:?}");
    assert_eq!(
        state.layer().palette[&PaletteKey::Accent],
        ColorSpec::Rgb(0xfa, 0xbd, 0x2f)
    );
    let source = state.source().unwrap();
    assert_eq!(source.kind, "registered");
    assert_eq!(source.name.as_deref(), Some("gruvbox"));
    assert_eq!(source.origin.as_deref(), Some("sparsh-themes"));
}

#[test]
fn state_file_rejects_code_and_keeps_last_valid() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path());
    let path = theme_path(home.path());
    fs::write(&path, STATE).unwrap();
    let mut state = ThemeFileState::default();
    state.refresh(home.path(), false);
    fs::write(
        &path,
        "var themeState: Record = { source: { kind: \"external\"; }; theme: { palette: { accent: $(echo hi); }; }; };\n",
    )
    .unwrap();
    let refresh = state.refresh(home.path(), false);
    assert!(refresh.notice.is_some());
    assert_eq!(
        state.layer().palette[&PaletteKey::Accent],
        ColorSpec::Rgb(0xfa, 0xbd, 0x2f)
    );
}

#[test]
fn state_file_rejects_unknown_source_kind() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path());
    fs::write(theme_path(home.path()), STATE.replace("registered", "mystery")).unwrap();
    let mut state = ThemeFileState::default();
    let refresh = state.refresh(home.path(), false);
    assert!(refresh.notice.unwrap().contains("source.kind"));
}

#[test]
fn keeps_last_valid_theme_across_partial_rewrite_and_recovers() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path());
    let path = theme_path(home.path());
    fs::write(&path, source("#112233")).unwrap();
    let mut state = ThemeFileState::default();
    let first = state.refresh(home.path(), false);
    assert!(first.changed, "{first:?}");
    assert_eq!(
        state.layer().palette[&PaletteKey::Accent],
        ColorSpec::Rgb(0x11, 0x22, 0x33)
    );

    fs::write(&path, "var themeState: Record = {").unwrap();
    let bad = state.refresh(home.path(), false);
    assert!(!bad.changed);
    assert!(bad.notice.is_some());
    assert!(state.refresh(home.path(), false).notice.is_none());
    assert_eq!(
        state.layer().palette[&PaletteKey::Accent],
        ColorSpec::Rgb(0x11, 0x22, 0x33)
    );

    fs::write(&path, source("#445566")).unwrap();
    assert!(state.refresh(home.path(), false).changed);
    assert_eq!(
        state.layer().palette[&PaletteKey::Accent],
        ColorSpec::Rgb(0x44, 0x55, 0x66)
    );
    fs::remove_file(&path).unwrap();
    assert!(state.refresh(home.path(), false).changed);
    assert!(state.layer().palette.is_empty());
}

#[test]
fn notices_invalid_startup_file_once_and_rejects_executable_source() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path());
    let path = theme_path(home.path());
    fs::write(&path, "function attack() { return 1; };").unwrap();
    let mut state = ThemeFileState::default();
    let first = state.refresh(home.path(), false);
    assert!(!first.changed);
    assert!(first.notice.is_some());
    assert!(state.refresh(home.path(), false).notice.is_none());
    assert!(state.refresh(home.path(), true).notice.is_some());
}

#[test]
fn reloads_same_size_replacement_even_with_same_mtime() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path());
    let path = theme_path(home.path());
    fs::write(&path, source("#112233")).unwrap();
    let mut state = ThemeFileState::default();
    assert!(state.refresh(home.path(), false).changed);
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, source("#445566")).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(mtime))
        .unwrap();
    assert!(state.refresh(home.path(), false).changed);
    assert_eq!(
        state.layer().palette[&PaletteKey::Accent],
        ColorSpec::Rgb(0x44, 0x55, 0x66)
    );
}
