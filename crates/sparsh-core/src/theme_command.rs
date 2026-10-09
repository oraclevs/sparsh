//! Theme commands and private, atomic persistence.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::builtin::{BuiltinError, BuiltinOutput};
use crate::{
    ColorSpec, PaletteKey, ShellError, SparshConfig, ThemeColor, ThemeFileState, ThemeLayer,
    ThemeRole, ThemeSource,
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn run(
    args: &[String],
    state: &mut ThemeFileState,
    config: &SparshConfig,
    home: Option<&Path>,
) -> Result<BuiltinOutput, ShellError> {
    let result = command(args, state, config, home).map_err(|message| {
        ShellError::Builtin(BuiltinError {
            message: format!("theme: {message}"),
            status: 1,
        })
    })?;
    Ok(BuiltinOutput {
        stdout: result.into_bytes(),
        stderr: Vec::new(),
        status: 0,
    })
}

const USAGE: &str = "usage: theme [list | set NAME | set default | set --accent #rrggbb | import pywal|FILE | export FILE [--force]]";

fn command(
    args: &[String],
    state: &mut ThemeFileState,
    config: &SparshConfig,
    home: Option<&Path>,
) -> Result<String, String> {
    if let Some(home) = home {
        let _ = state.refresh(home, false);
    }
    match args {
        [] => {
            let mut out = String::new();
            for (property, value) in status_rows(state, config) {
                out.push_str(&format!("{property}\t{value}\n"));
            }
            Ok(out)
        }
        [action] if action == "list" => {
            let mut out = String::new();
            for (name, description, active) in list_rows(state, config) {
                out.push_str(if active { "* " } else { "  " });
                out.push_str(&name);
                if let Some(description) = description {
                    out.push('\t');
                    out.push_str(&description);
                }
                out.push('\n');
            }
            Ok(out)
        }
        [action, name] if action == "set" && name == "default" => {
            let home = require_home(home)?;
            ensure_src_dir(home)?;
            let active = ThemeFileState::path(home);
            match fs::symlink_metadata(&active) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(format!("{} is a symlink", active.display()))
                }
                Ok(metadata) if !metadata.is_file() => {
                    return Err(format!("{} is not a regular file", active.display()))
                }
                Ok(_) => fs::remove_file(&active)
                    .map_err(|error| format!("{}: {error}", active.display()))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", active.display())),
            }
            state.refresh(home, true);
            Ok("theme: default\n".into())
        }
        [action, flag, color] if action == "set" && flag == "--accent" => {
            let home = require_home(home)?;
            let parsed =
                crate::parse_color(color).map_err(|error| format!("invalid accent: {error}"))?;
            if !matches!(parsed, ColorSpec::Rgb(_, _, _)) {
                return Err("accent must be #rrggbb".into());
            }
            let source = ThemeSource {
                kind: "accent".into(),
                name: None,
                origin: Some(color.clone()),
            };
            activate_generated(home, state, &source, &accent_layer(parsed))?;
            Ok(format!("theme: accent {color}\n"))
        }
        [action, name] if action == "set" && !name.starts_with("--") => {
            let home = require_home(home)?;
            let Some(theme) = config.themes.iter().find(|theme| &theme.name == name) else {
                let names: Vec<_> = config.themes.iter().map(|t| t.name.as_str()).collect();
                return Err(format!(
                    "unknown theme `{name}`; registered: {}",
                    if names.is_empty() { "(none)".to_string() } else { names.join(", ") }
                ));
            };
            let source = ThemeSource {
                kind: "registered".into(),
                name: Some(name.clone()),
                origin: None,
            };
            activate_generated(home, state, &source, &theme.layer)?;
            Ok(format!("theme: {name}\n"))
        }
        [action, target] if action == "import" && target == "pywal" => {
            let home = require_home(home)?;
            let wal = home.join(".cache/wal/colors.json");
            let layer = pywal_layer(&wal)?;
            let source = ThemeSource {
                kind: "pywal".into(),
                name: None,
                origin: Some(wal.display().to_string()),
            };
            activate_generated(home, state, &source, &layer)?;
            Ok(format!("theme: imported pywal from {}\n", wal.display()))
        }
        [action, file] if action == "import" => {
            let home = require_home(home)?;
            let path = Path::new(file);
            let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
            let (_, layer) = crate::theme_file::load_source(&text, path)?;
            let source = ThemeSource {
                kind: "external".into(),
                name: path.file_stem().and_then(|s| s.to_str()).map(str::to_string),
                origin: Some(path.display().to_string()),
            };
            activate_generated(home, state, &source, &layer)?;
            Ok(format!("theme: imported {}\n", path.display()))
        }
        [action, target] | [action, target, _] if action == "export" => {
            let force = parse_force(args, "export")?;
            let path = Path::new(target);
            let source = ThemeSource {
                kind: "external".into(),
                name: path.file_stem().and_then(|s| s.to_str()).map(str::to_string),
                origin: Some(path.display().to_string()),
            };
            let text = render_state(&source, &effective_layer(&config.theme, state.layer()));
            write_theme_atomic(path, text.as_bytes(), force)?;
            Ok(format!("theme: exported {}\n", path.display()))
        }
        _ => Err(USAGE.into()),
    }
}

/// `(name, description, active)` for `default` and every registered theme.
pub(crate) fn list_rows(
    state: &ThemeFileState,
    config: &SparshConfig,
) -> Vec<(String, Option<String>, bool)> {
    let active = state
        .source()
        .filter(|source| source.kind == "registered")
        .and_then(|source| source.name.clone());
    let mut rows = vec![(
        "default".to_string(),
        Some("built-in colors".to_string()),
        state.source().is_none(),
    )];
    for theme in &config.themes {
        rows.push((
            theme.name.clone(),
            theme.description.clone(),
            active.as_deref() == Some(theme.name.as_str()),
        ));
    }
    rows
}

/// `(property, value)` lines describing the active theme.
pub(crate) fn status_rows(
    state: &ThemeFileState,
    config: &SparshConfig,
) -> Vec<(&'static str, String)> {
    let (name, kind, origin) = match state.source() {
        Some(source) => (
            source.name.clone().unwrap_or_else(|| source.kind.clone()),
            source.kind.clone(),
            source.origin.clone(),
        ),
        None => ("default".to_string(), "default".to_string(), None),
    };
    let palette = state.layer().palette.len() + config.theme.palette.len();
    let overrides = state.layer().roles.len() + config.theme.roles.len();
    let mut rows = vec![("theme", name), ("source", kind)];
    if let Some(origin) = origin {
        rows.push(("origin", origin));
    }
    rows.push(("palette", format!("{palette} values")));
    rows.push(("overrides", format!("{overrides} roles")));
    rows
}

/// The overrides in effect, without baking built-in defaults: the user's
/// theme first, then the generated layer on top.
fn effective_layer(config: &ThemeLayer, generated: &ThemeLayer) -> ThemeLayer {
    let mut layer = config.clone();
    layer.palette.extend(generated.palette.iter().map(|(k, v)| (*k, *v)));
    layer.roles.extend(generated.roles.iter().map(|(k, v)| (*k, v.clone())));
    layer
}

fn activate_generated(
    home: &Path,
    state: &mut ThemeFileState,
    source: &ThemeSource,
    layer: &ThemeLayer,
) -> Result<(), String> {
    ensure_src_dir(home)?;
    let active = ThemeFileState::path(home);
    let text = render_state(source, layer);
    // Never write a file the loader would reject.
    crate::theme_file::load_source(&text, &active)?;
    write_theme_atomic(&active, text.as_bytes(), true)?;
    state.refresh(home, true);
    Ok(())
}

fn parse_force(args: &[String], action: &str) -> Result<bool, String> {
    match args {
        [got, _] if got == action => Ok(false),
        [got, _, flag] if got == action && flag == "--force" => Ok(true),
        _ => Err(format!("usage: theme {action} FILE [--force]")),
    }
}

fn require_home(home: Option<&Path>) -> Result<&Path, String> {
    home.ok_or("HOME is required for theme file operations".into())
}

fn ensure_src_dir(home: &Path) -> Result<(), String> {
    let mut path = home.to_path_buf();
    for component in [".sparsh", "src"] {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(format!("{} is not a regular directory", path.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                        .map_err(|error| format!("{}: {error}", path.display()))?;
                }
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("{} is a symlink", path.display()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

pub(crate) fn write_theme_atomic(path: &Path, bytes: &[u8], force: bool) -> Result<(), String> {
    let parent = path.parent().ok_or("theme path has no parent")?;
    if parent.as_os_str().is_empty() {
        return Err("theme path has no parent".into());
    }
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    reject_symlink(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(format!("{} is not a regular file", path.display()))
        }
        Ok(_) if !force => return Err(format!("{} already exists; use --force", path.display())),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", path.display())),
    }
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".sparsh-theme-{}-{sequence}.tmp",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = (|| -> Result<(), String> {
        let mut file = options
            .open(&temp)
            .map_err(|error| format!("{}: {error}", temp.display()))?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        fs::rename(&temp, path).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
}


pub(crate) fn render_state(source: &ThemeSource, layer: &ThemeLayer) -> String {
    let mut out = String::from(
        "// Written by sparsh. Delete it (theme set default) to return to your own theme.\nvar themeState: Record = {\n",
    );
    out.push_str(&format!("    source: {{ kind: \"{}\";", source.kind));
    if let Some(name) = &source.name {
        out.push_str(&format!(" name: \"{}\";", escape(name)));
    }
    if let Some(origin) = &source.origin {
        out.push_str(&format!(" origin: \"{}\";", escape(origin)));
    }
    out.push_str(" };\n    theme: {\n");
    if !layer.palette.is_empty() {
        out.push_str("        palette: {");
        for key in PaletteKey::ALL {
            if let Some(color) = layer.palette.get(key) {
                out.push_str(&format!(" {}: \"{}\";", key.name(), color_text(*color)));
            }
        }
        out.push_str(" };\n");
    }
    for group in GROUPS {
        let roles: Vec<_> = ThemeRole::ALL
            .iter()
            .filter(|role| {
                role.config_path().starts_with(&format!("{group}.")) && layer.roles.contains_key(role)
            })
            .collect();
        if roles.is_empty() {
            continue;
        }
        out.push_str(&format!("        {group}: {{\n"));
        for role in roles {
            let field = role.config_path().split_once('.').unwrap().1;
            let spec = &layer.roles[role];
            out.push_str(&format!("            {field}: {{"));
            if let Some(color) = spec.foreground {
                out.push_str(&format!(" foreground: \"{}\";", theme_color_text(color)));
            }
            if let Some(color) = spec.background {
                out.push_str(&format!(" background: \"{}\";", theme_color_text(color)));
            }
            for (name, value) in [
                ("bold", spec.bold),
                ("dim", spec.dim),
                ("italic", spec.italic),
                ("underline", spec.underline),
            ] {
                if let Some(value) = value {
                    out.push_str(&format!(" {name}: {value};"));
                }
            }
            out.push_str(" };\n");
        }
        out.push_str("        };\n");
    }
    out.push_str("    };\n};\n");
    out
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

const GROUPS: &[&str] = &[
    "prompt",
    "syntax",
    "completion",
    "data",
    "git",
    "help",
    "editor",
    "pager",
];

fn title(group: &str) -> String {
    let mut chars = group.chars();
    format!(
        "{}{}",
        chars.next().unwrap().to_ascii_uppercase(),
        chars.as_str()
    )
}

fn color_text(color: ColorSpec) -> String {
    match color {
        ColorSpec::Indexed(value) => value.to_string(),
        ColorSpec::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn theme_color_text(color: ThemeColor) -> String {
    match color {
        ThemeColor::Literal(color) => color_text(color),
        ThemeColor::Palette(key) => format!("${}", key.name()),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ThemeStyleSpec;

    fn registry() -> SparshConfig {
        let mut config = SparshConfig::default();
        let mut gruv = ThemeLayer::default();
        gruv.palette.insert(PaletteKey::Accent, ColorSpec::Rgb(0xfa, 0xbd, 0x2f));
        config.themes.push(crate::ThemeKindConfig {
            name: "gruvbox".into(),
            description: Some("warm".into()),
            layer: gruv,
        });
        config.themes.push(crate::ThemeKindConfig {
            name: "royal".into(),
            description: None,
            layer: ThemeLayer::default(),
        });
        config
    }

    fn run_args(
        args: &[&str],
        state: &mut ThemeFileState,
        config: &SparshConfig,
        home: &Path,
    ) -> Result<String, String> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        command(&args, state, config, Some(home))
    }

    #[test]
    fn set_selects_a_registered_theme_and_records_its_source() {
        let home = tempfile::tempdir().unwrap();
        let mut state = ThemeFileState::default();
        let out = run_args(&["set", "gruvbox"], &mut state, &registry(), home.path()).unwrap();
        assert_eq!(out, "theme: gruvbox\n");
        assert_eq!(state.layer().palette[&PaletteKey::Accent], ColorSpec::Rgb(0xfa, 0xbd, 0x2f));
        let source = state.source().unwrap();
        assert_eq!((source.kind.as_str(), source.name.as_deref()), ("registered", Some("gruvbox")));
    }

    #[test]
    fn set_unknown_lists_registered_names() {
        let home = tempfile::tempdir().unwrap();
        let mut state = ThemeFileState::default();
        let error = run_args(&["set", "nope"], &mut state, &registry(), home.path()).unwrap_err();
        assert!(
            error.contains("unknown theme `nope`") && error.contains("gruvbox") && error.contains("royal"),
            "{error}"
        );
    }

    #[test]
    fn list_marks_the_active_theme_and_always_has_default() {
        let home = tempfile::tempdir().unwrap();
        let mut state = ThemeFileState::default();
        let config = registry();
        assert_eq!(
            run_args(&["list"], &mut state, &config, home.path()).unwrap(),
            "* default\tbuilt-in colors\n  gruvbox\twarm\n  royal\n"
        );
        run_args(&["set", "royal"], &mut state, &config, home.path()).unwrap();
        assert_eq!(
            run_args(&["list"], &mut state, &config, home.path()).unwrap(),
            "  default\tbuilt-in colors\n  gruvbox\twarm\n* royal\n"
        );
    }

    #[test]
    fn status_shows_source_name_and_origin() {
        let home = tempfile::tempdir().unwrap();
        let mut state = ThemeFileState::default();
        let config = registry();
        assert!(run_args(&[], &mut state, &config, home.path()).unwrap().starts_with("theme\tdefault\nsource\tdefault\n"));
        run_args(&["set", "gruvbox"], &mut state, &config, home.path()).unwrap();
        let out = run_args(&[], &mut state, &config, home.path()).unwrap();
        assert!(out.starts_with("theme\tgruvbox\nsource\tregistered\n"), "{out}");
        run_args(&["set", "--accent", "#ff6ac1"], &mut state, &config, home.path()).unwrap();
        let out = run_args(&[], &mut state, &config, home.path()).unwrap();
        assert!(out.contains("source\taccent\norigin\t#ff6ac1\n"), "{out}");
    }

    #[test]
    fn set_default_removes_only_the_generated_file() {
        let home = tempfile::tempdir().unwrap();
        ensure_src_dir(home.path()).unwrap();
        let modules = home.path().join(".sparsh/src/modules");
        fs::create_dir_all(&modules).unwrap();
        fs::write(modules.join("theme.spar"), "// mine\n").unwrap();
        let mut state = ThemeFileState::default();
        run_args(&["set", "gruvbox"], &mut state, &registry(), home.path()).unwrap();
        assert!(ThemeFileState::path(home.path()).is_file());
        run_args(&["set", "default"], &mut state, &registry(), home.path()).unwrap();
        assert!(!ThemeFileState::path(home.path()).exists());
        assert!(ThemeFileState::path(home.path()).ends_with("theme.generated.spar"));
        assert_eq!(fs::read_to_string(modules.join("theme.spar")).unwrap(), "// mine\n");
        assert!(state.source().is_none());
    }

    #[test]
    fn export_then_import_round_trips_with_external_source() {
        let home = tempfile::tempdir().unwrap();
        let mut state = ThemeFileState::default();
        let config = registry();
        run_args(&["set", "gruvbox"], &mut state, &config, home.path()).unwrap();
        let file = home.path().join("mine.spar");
        run_args(&["export", file.to_str().unwrap()], &mut state, &config, home.path()).unwrap();
        run_args(&["set", "default"], &mut state, &config, home.path()).unwrap();
        run_args(&["import", file.to_str().unwrap()], &mut state, &config, home.path()).unwrap();
        assert_eq!(state.layer().palette[&PaletteKey::Accent], ColorSpec::Rgb(0xfa, 0xbd, 0x2f));
        let source = state.source().unwrap();
        assert_eq!(source.kind, "external");
        assert_eq!(source.name.as_deref(), Some("mine"));
    }

    #[test]
    fn rendered_state_round_trips_through_the_loader() {
        let mut layer = ThemeLayer::default();
        layer.palette.insert(PaletteKey::Accent, ColorSpec::Rgb(1, 2, 3));
        layer.roles.insert(
            ThemeRole::Cwd,
            ThemeStyleSpec {
                foreground: Some(ThemeColor::Palette(PaletteKey::Accent)),
                bold: Some(true),
                ..Default::default()
            },
        );
        let source = ThemeSource {
            kind: "wallpaper".into(),
            name: None,
            origin: Some("/tmp/a \"b\".png".into()),
        };
        let text = render_state(&source, &layer);
        let (back_source, back_layer) = crate::theme_file::load_source(
            &text,
            Path::new("/tmp/x/theme.generated.spar"),
        )
        .unwrap();
        assert_eq!(back_source, source);
        assert_eq!(back_layer, layer);
    }

    #[test]
    fn accent_keeps_semantic_hues_for_any_accent() {
        for accent in [(255u8, 106u8, 193u8), (138, 180, 248), (20, 200, 60)] {
            let layer = accent_layer(ColorSpec::Rgb(accent.0, accent.1, accent.2));
            let hue_of = |key: PaletteKey| match layer.palette[&key] {
                ColorSpec::Rgb(r, g, b) => hue_saturation(r, g, b).0,
                ColorSpec::Indexed(_) => panic!("expected rgb"),
            };
            let success = hue_of(PaletteKey::Success);
            let warning = hue_of(PaletteKey::Warning);
            let error = hue_of(PaletteKey::Error);
            let info = hue_of(PaletteKey::Info);
            assert!((90.0..=160.0).contains(&success), "success {success}");
            assert!((30.0..=65.0).contains(&warning), "warning {warning}");
            assert!(error <= 25.0 || error >= 340.0, "error {error}");
            assert!((190.0..=250.0).contains(&info), "info {info}");
        }
    }

    #[test]
    fn atomic_writer_refuses_existing_and_symlink_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("theme.spar");
        write_theme_atomic(&target, b"first", false).unwrap();
        assert!(write_theme_atomic(&target, b"second", false).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"first");
        write_theme_atomic(&target, b"second", true).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, dir.path().join("link.spar")).unwrap();
            assert!(write_theme_atomic(&dir.path().join("link.spar"), b"third", true).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"second");
        }
    }

    #[test]
    fn accent_generates_nine_readable_palette_colors() {
        let layer = accent_layer(ColorSpec::Rgb(138, 180, 248));
        assert_eq!(layer.palette.len(), 9);
        for key in [
            PaletteKey::Accent,
            PaletteKey::Text,
            PaletteKey::Success,
            PaletteKey::Warning,
            PaletteKey::Error,
            PaletteKey::Info,
            PaletteKey::Muted,
        ] {
            let color = layer.palette[&key];
            let luminance = relative_luminance(color);
            assert!((0.175..=0.183).contains(&luminance), "{key:?}: {luminance}");
        }
        let selected = contrast_ratio(
            layer.palette[&PaletteKey::SelectionFg],
            layer.palette[&PaletteKey::SelectionBg],
        );
        assert!(selected >= 4.5, "{selected}");
    }

    #[test]
    fn accent_contrast_holds_across_hues_and_neutral_inputs() {
        for color in [
            ColorSpec::Rgb(255, 0, 0),
            ColorSpec::Rgb(0, 255, 0),
            ColorSpec::Rgb(0, 0, 255),
            ColorSpec::Rgb(0, 0, 0),
            ColorSpec::Rgb(255, 255, 255),
            ColorSpec::Rgb(120, 120, 120),
        ] {
            let layer = accent_layer(color);
            for key in [
                PaletteKey::Accent,
                PaletteKey::Text,
                PaletteKey::Muted,
                PaletteKey::Success,
                PaletteKey::Warning,
                PaletteKey::Error,
                PaletteKey::Info,
                PaletteKey::SelectionBg,
            ] {
                let luminance = relative_luminance(layer.palette[&key]);
                assert!(
                    (0.175..=0.183).contains(&luminance),
                    "{color:?} {key:?} {luminance}"
                );
            }
            assert!(
                contrast_ratio(
                    layer.palette[&PaletteKey::SelectionFg],
                    layer.palette[&PaletteKey::SelectionBg]
                ) >= 4.5
            );
        }
    }

    #[test]
    fn pywal_missing_or_malformed_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("colors.json");
        assert!(pywal_layer(&path).is_err());
        fs::write(&path, b"{bad").unwrap();
        assert!(pywal_layer(&path).is_err());
    }
}

fn accent_layer(accent: ColorSpec) -> ThemeLayer {
    let (r, g, b) = match accent {
        ColorSpec::Rgb(r, g, b) => (r, g, b),
        ColorSpec::Indexed(_) => (138, 180, 248),
    };
    let (hue, saturation) = hue_saturation(r, g, b);
    let saturation = saturation.clamp(0.45, 0.82);
    let mut layer = ThemeLayer::default();
    for (key, hue_for_key, sat) in [
        (PaletteKey::Accent, hue, saturation),
        (PaletteKey::Text, hue, 0.02),
        (PaletteKey::Muted, hue, 0.05),
        (PaletteKey::Success, 130.0, saturation),
        (PaletteKey::Warning, 45.0, saturation),
        (PaletteKey::Error, 5.0, saturation),
        (PaletteKey::Info, 210.0, saturation),
        (PaletteKey::SelectionBg, hue, saturation),
    ] {
        layer
            .palette
            .insert(key, color_at_luminance(hue_for_key, sat, 0.179));
    }
    layer
        .palette
        .insert(PaletteKey::SelectionFg, ColorSpec::Rgb(255, 255, 255));
    layer
}

fn pywal_layer(path: &Path) -> Result<ThemeLayer, String> {
    let bytes = fs::read(path).map_err(|error| format!("pywal {}: {error}", path.display()))?;
    let input: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("pywal {}: {error}", path.display()))?;
    let colors = input
        .get("colors")
        .and_then(serde_json::Value::as_object)
        .ok_or("pywal: missing colors object")?;
    let special = input
        .get("special")
        .and_then(serde_json::Value::as_object)
        .ok_or("pywal: missing special object")?;
    let foreground = special
        .get("foreground")
        .and_then(serde_json::Value::as_str)
        .ok_or("pywal: missing special.foreground")?;
    let background = special
        .get("background")
        .and_then(serde_json::Value::as_str)
        .ok_or("pywal: missing special.background")?;
    parse_pywal_color(background, "special.background")?;
    build_pywal_layer(colors, foreground)
}

fn build_pywal_layer(
    colors: &serde_json::Map<String, serde_json::Value>,
    foreground: &str,
) -> Result<ThemeLayer, String> {
    let mut layer = ThemeLayer::default();
    let fg = parse_pywal_color(foreground, "special.foreground")?;
    layer.palette.insert(PaletteKey::Text, clamp_color(fg));
    for (key, source) in [
        (PaletteKey::Accent, "color4"),
        (PaletteKey::Muted, "color8"),
        (PaletteKey::Success, "color2"),
        (PaletteKey::Warning, "color3"),
        (PaletteKey::Error, "color1"),
        (PaletteKey::Info, "color6"),
        (PaletteKey::SelectionBg, "color12"),
    ] {
        let value = colors
            .get(source)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("pywal: missing colors.{source}"))?;
        layer.palette.insert(
            key,
            clamp_color(parse_pywal_color(value, &format!("colors.{source}"))?),
        );
    }
    layer
        .palette
        .insert(PaletteKey::SelectionFg, ColorSpec::Rgb(255, 255, 255));
    Ok(layer)
}

fn parse_pywal_color(text: &str, path: &str) -> Result<ColorSpec, String> {
    if !text.starts_with('#') {
        return Err(format!("pywal: {path} must be #rrggbb"));
    }
    match crate::parse_color(text) {
        Ok(color @ ColorSpec::Rgb(_, _, _)) => Ok(color),
        _ => Err(format!("pywal: {path} must be #rrggbb")),
    }
}

fn clamp_color(color: ColorSpec) -> ColorSpec {
    let ColorSpec::Rgb(r, g, b) = color else {
        return color;
    };
    let (hue, saturation) = hue_saturation(r, g, b);
    color_at_luminance(hue, saturation.clamp(0.02, 0.82), 0.179)
}

fn hue_saturation(r: u8, g: u8, b: u8) -> (f64, f64) {
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let lightness = (max + min) / 2.0;
    let saturation = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * lightness - 1.0).abs())
    };
    (hue, saturation)
}

fn color_at_luminance(hue: f64, saturation: f64, target: f64) -> ColorSpec {
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..30 {
        let mid = (low + high) / 2.0;
        if relative_luminance(hsl_color(hue, saturation, mid)) < target {
            low = mid;
        } else {
            high = mid;
        }
    }
    hsl_color(hue, saturation, (low + high) / 2.0)
}

fn hsl_color(hue: f64, saturation: f64, lightness: f64) -> ColorSpec {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let x = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match sector as u8 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = lightness - chroma / 2.0;
    let byte = |value: f64| ((value + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    ColorSpec::Rgb(byte(r), byte(g), byte(b))
}

fn relative_luminance(color: ColorSpec) -> f64 {
    let ColorSpec::Rgb(r, g, b) = color else {
        return 0.0;
    };
    let linear = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

#[cfg(test)]
fn contrast_ratio(a: ColorSpec, b: ColorSpec) -> f64 {
    let first = relative_luminance(a);
    let second = relative_luminance(b);
    let high = first.max(second);
    let low = first.min(second);
    (high + 0.05) / (low + 0.05)
}
