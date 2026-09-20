use std::path::PathBuf;
use std::sync::OnceLock;

const CONFIG_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/config-package/src/config.spar"
));

static PACKAGE_ROOT: OnceLock<Result<PathBuf, String>> = OnceLock::new();

pub(crate) fn package_root() -> Result<PathBuf, String> {
    PACKAGE_ROOT
        .get_or_init(materialize)
        .clone()
}

pub(crate) fn engine() -> Result<spar::Engine, String> {
    spar::Engine::default().with_bundled_package_root("sparsh", package_root()?)
}

pub(crate) fn engine_for_base_dir(base_dir: impl Into<PathBuf>) -> Result<spar::Engine, String> {
    Ok(engine()?.with_base_dir(base_dir))
}

fn materialize() -> Result<PathBuf, String> {
    let root = std::env::temp_dir()
        .join("sparsh-bundled-packages")
        .join(env!("CARGO_PKG_VERSION"))
        .join("sparsh");
    std::fs::create_dir_all(&root).map_err(|error| {
        format!(
            "failed to create bundled Sparsh config package at {}: {error}",
            root.display()
        )
    })?;
    write_if_changed(&root.join("config.spar"), CONFIG_SOURCE)?;
    write_if_changed(
        &root.join("lib.spar"),
        "// Legacy Sparsh package root. New rc files use local sparsh-types.spar.\n",
    )?;
    Ok(root)
}

fn write_if_changed(path: &std::path::Path, source: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(source) {
        return Ok(());
    }
    std::fs::write(path, source)
        .map_err(|error| format!("failed to materialize {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_config_package_contains_public_schema() {
        let root = super::package_root().unwrap();
        let source = std::fs::read_to_string(root.join("config.spar")).unwrap();
        assert!(source.contains("export type SparshConfig"));
        assert!(source.contains("command: List<str>"));
    }
}
