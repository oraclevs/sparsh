//! Loads the standalone theme file while retaining the last valid layer.

use std::fs;
use std::path::{Path, PathBuf};

use spar::ast::{Expr, FieldValue, ObjectItem, StringPart, TopLevelItem};

use crate::theme_config::{expect_object, expect_string};
use crate::ThemeLayer;

/// Where the active generated theme came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeSource {
    pub kind: String,
    pub name: Option<String>,
    pub origin: Option<String>,
}

const SOURCE_KINDS: &[&str] = &["registered", "external", "wallpaper", "accent", "pywal"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeRefresh {
    pub changed: bool,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ThemeFileState {
    last_bytes: Option<Vec<u8>>,
    layer: ThemeLayer,
    source: Option<ThemeSource>,
    generation: u64,
    last_read_error: Option<String>,
}

impl ThemeFileState {
    pub fn layer(&self) -> &ThemeLayer {
        &self.layer
    }

    pub fn source(&self) -> Option<&ThemeSource> {
        self.source.as_ref()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn refresh(&mut self, home: &Path, force: bool) -> ThemeRefresh {
        let path = Self::path(home);
        let bytes = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                let message = format!("{}: {error}", path.display());
                let notice = (force || self.last_read_error.as_deref() != Some(&message))
                    .then(|| message.clone());
                self.last_read_error = Some(message);
                return ThemeRefresh {
                    changed: false,
                    notice,
                };
            }
        };
        self.last_read_error = None;
        if !force && bytes == self.last_bytes {
            return ThemeRefresh::default();
        }
        self.last_bytes = bytes.clone();
        let Some(bytes) = bytes else {
            return self.apply(None, ThemeLayer::default());
        };
        let parsed = std::str::from_utf8(&bytes)
            .map_err(|error| format!("invalid UTF-8: {error}"))
            .and_then(|source| load_source(source, &path));
        match parsed {
            Ok((source, layer)) => self.apply(Some(source), layer),
            Err(message) => ThemeRefresh {
                changed: false,
                notice: Some(format!("{}: {message}", path.display())),
            },
        }
    }

    pub fn path(home: &Path) -> PathBuf {
        home.join(".sparsh/src/theme.generated.spar")
    }

    fn apply(&mut self, source: Option<ThemeSource>, layer: ThemeLayer) -> ThemeRefresh {
        if self.layer == layer && self.source == source {
            return ThemeRefresh::default();
        }
        self.layer = layer;
        self.source = source;
        self.generation = self.generation.wrapping_add(1);
        ThemeRefresh {
            changed: true,
            notice: None,
        }
    }
}

pub(crate) fn load_source(source: &str, path: &Path) -> Result<(ThemeSource, ThemeLayer), String> {
    let tokens = spar::Lexer::new(source)
        .tokenize()
        .map_err(|error| error.to_string())?;
    let program = spar::Parser::new(tokens)
        .parse()
        .map_err(|error| error.to_string())?;
    // Only `var themeState = <literal record>;` is accepted: a tool-written
    // file must never run code.
    let [TopLevelItem::Var(decl)] = program.items.as_slice() else {
        return Err("theme state file must contain only `var themeState = { ... };`".into());
    };
    if decl.name != "themeState" {
        return Err("theme state file must declare `themeState`".into());
    }
    match &decl.value {
        Some(value) if declarative(value) => {}
        Some(_) => return Err("theme state contains an executable expression".into()),
        None => return Err("themeState needs a value".into()),
    }
    let base_dir = path.parent().ok_or("theme file has no parent directory")?;
    let mut session = spar::Engine::default().with_base_dir(base_dir).session();
    session.eval(source).map_err(format_errors)?;
    let value = session
        .value("themeState")
        .ok_or("theme file did not define themeState")?;
    let root = expect_object(value, "themeState")?;
    let meta = root
        .get("source")
        .ok_or("themeState.source is required")?;
    let meta = expect_object(meta, "themeState.source")?;
    let kind = expect_string(
        meta.get("kind").ok_or("themeState.source.kind is required")?,
        "themeState.source.kind",
    )?;
    if !SOURCE_KINDS.contains(&kind) {
        return Err(format!(
            "themeState.source.kind `{kind}` must be one of {}",
            SOURCE_KINDS.join(", ")
        ));
    }
    let optional = |key: &str| -> Result<Option<String>, String> {
        match meta.get(key) {
            Some(value) => Ok(Some(
                expect_string(value, &format!("themeState.source.{key}"))?.to_string(),
            )),
            None => Ok(None),
        }
    };
    let source_meta = ThemeSource {
        kind: kind.to_string(),
        name: optional("name")?,
        origin: optional("origin")?,
    };
    let theme = root.get("theme").ok_or("themeState.theme is required")?;
    let layer = ThemeLayer::parse(theme, "themeState.theme")?;
    Ok((source_meta, layer))
}

fn format_errors(errors: Vec<spar::SparError>) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn declarative(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(_) => true,
        Expr::String(value) => value
            .parts
            .iter()
            .all(|part| matches!(part, StringPart::Literal(_))),
        Expr::List(items, _) => items.iter().all(declarative),
        Expr::Grouped(inner, _) => declarative(inner),
        Expr::Object(items, _) => items.iter().all(|item| match item {
            ObjectItem::Field(field) => {
                matches!(&field.value, Some(FieldValue::Expr(value)) if declarative(value))
            }
            ObjectItem::Spread(_) => false,
        }),
        _ => false,
    }
}
