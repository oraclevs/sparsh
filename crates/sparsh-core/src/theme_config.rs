//! Shared theme roles and sparse theme layers.

use std::collections::BTreeMap;

use crate::ColorSpec;

macro_rules! theme_roles {
    ($($variant:ident => $path:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub enum ThemeRole { $($variant),+ }
        impl ThemeRole {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn config_path(self) -> &'static str {
                match self { $(Self::$variant => $path),+ }
            }
        }
    };
}

theme_roles! {
    Cwd => "prompt.cwd",
    GitBranch => "git.gitBranch",
    GitDirty => "git.gitDirty",
    GitClean => "git.gitClean",
    GitStaged => "git.gitStaged",
    GitModified => "git.gitModified",
    GitUntracked => "git.gitUntracked",
    GitConflict => "git.gitConflict",
    GitAhead => "git.gitAhead",
    GitBehind => "git.gitBehind",
    Success => "prompt.success",
    Time => "prompt.time",
    Failure => "prompt.failure",
    Duration => "prompt.duration",
    PromptMarker => "prompt.promptMarker",
    Secondary => "prompt.secondary",
    Warning => "syntax.warning",
    Error => "syntax.error",
    Builtin => "syntax.builtin",
    Alias => "syntax.alias",
    ExternalCommand => "syntax.externalCommand",
    UnknownCommand => "syntax.unknownCommand",
    Argument => "syntax.argument",
    Path => "syntax.path",
    Function => "syntax.functionName",
    Parameter => "syntax.parameter",
    Option => "syntax.option",
    QuotedString => "syntax.quotedString",
    Operator => "syntax.operator",
    SparSyntax => "syntax.sparSyntax",
    Comment => "syntax.comment",
    TypeName => "syntax.typeName",
    VirtualEnvironment => "prompt.virtualEnvironment",
    ProjectPython => "prompt.projectPython",
    ProjectRust => "prompt.projectRust",
    ProjectFlutter => "prompt.projectFlutter",
    ProjectDart => "prompt.projectDart",
    ProjectNode => "prompt.projectNode",
    ProjectGo => "prompt.projectGo",
    DataKey => "data.dataKey",
    DataString => "data.dataString",
    DataNumber => "data.dataNumber",
    DataBool => "data.dataBool",
    DataNull => "data.dataNull",
    DataPunct => "data.dataPunct",
    TableHeader => "data.tableHeader",
    TableIndex => "data.tableIndex",
    TableBorder => "data.tableBorder",
    FileDirectory => "data.fileDirectory",
    FileExecutable => "data.fileExecutable",
    FileSymlink => "data.fileSymlink",
    FileSpecial => "data.fileSpecial",
    CompletionFunction => "completion.completionFunction",
    CompletionVariable => "completion.completionVariable",
    CompletionType => "completion.completionType",
    CompletionKeyword => "completion.completionKeyword",
    CompletionPath => "completion.completionPath",
    CompletionDirectory => "completion.completionDirectory",
    CompletionCommand => "completion.completionCommand",
    MenuBorder => "completion.menuBorder",
    MenuText => "completion.menuText",
    MenuSelected => "completion.menuSelected",
    MenuDetail => "completion.menuDetail",
    MenuFooter => "completion.menuFooter",
    MenuCount => "completion.menuCount",
    MenuMatch => "completion.menuMatch",
    HintHistory => "completion.hintHistory",
    HintSignature => "completion.hintSignature",
    HintActive => "completion.hintActive",
    HelpTitle => "help.title",
    HelpHeading => "help.heading",
    HelpBody => "help.body",
    HelpOption => "help.option",
    HelpExample => "help.example",
    HelpCrossReference => "help.crossReference",
    EditorHeader => "editor.header",
    EditorPosition => "editor.position",
    EditorStatus => "editor.status",
    EditorSelection => "editor.selection",
    PagerStatus => "pager.status",
    PagerSearch => "pager.search",
    PagerSelected => "pager.selected",
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PaletteKey {
    Accent,
    Success,
    Error,
    Warning,
    Info,
    Muted,
    Text,
    SelectionFg,
    SelectionBg,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeColor {
    Literal(ColorSpec),
    Palette(PaletteKey),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ThemeStyleSpec {
    pub foreground: Option<ThemeColor>,
    pub background: Option<ThemeColor>,
    pub bold: Option<bool>,
    pub dim: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ThemeLayer {
    pub palette: BTreeMap<PaletteKey, ColorSpec>,
    pub roles: BTreeMap<ThemeRole, ThemeStyleSpec>,
}

impl PaletteKey {
    pub const ALL: &'static [Self] = &[
        Self::Accent,
        Self::Text,
        Self::Muted,
        Self::Success,
        Self::Warning,
        Self::Error,
        Self::Info,
        Self::SelectionFg,
        Self::SelectionBg,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Accent => "accent",
            Self::Text => "text",
            Self::Muted => "muted",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Info => "info",
            Self::SelectionFg => "selectionFg",
            Self::SelectionBg => "selectionBg",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|key| key.name() == name)
    }
}

impl ThemeLayer {
    pub fn parse(value: &spar::ConfigValue, path: &str) -> Result<Self, String> {
        let fields = expect_object(value, path)?;
        let groups = [
            "prompt",
            "syntax",
            "completion",
            "data",
            "git",
            "help",
            "editor",
            "pager",
        ];
        let mut allowed = vec!["palette"];
        allowed.extend(groups);
        validate_known_fields(fields, path, &allowed)?;
        let mut layer = Self::default();

        if let Some(palette) = fields.get("palette") {
            let palette_path = format!("{path}.palette");
            let values = expect_object(palette, &palette_path)?;
            validate_known_fields(
                values,
                &palette_path,
                &PaletteKey::ALL
                    .iter()
                    .map(|key| key.name())
                    .collect::<Vec<_>>(),
            )?;
            for (name, value) in values {
                if is_none(value) {
                    continue;
                }
                let key = PaletteKey::parse(name).expect("palette key validated");
                let field_path = format!("{palette_path}.{name}");
                let text = expect_string(value, &field_path)?;
                if text.starts_with('$') {
                    return Err(format!(
                        "{field_path}: palette values must be literal colors"
                    ));
                }
                let color =
                    crate::parse_color(text).map_err(|error| format!("{field_path}: {error}"))?;
                layer.palette.insert(key, color);
            }
        }

        for group in groups {
            let Some(value) = fields.get(group) else {
                continue;
            };
            if is_none(value) {
                continue;
            }
            let group_path = format!("{path}.{group}");
            let roles = expect_object(value, &group_path)?;
            let choices: Vec<&str> = ThemeRole::ALL
                .iter()
                .filter_map(|role| role.config_path().strip_prefix(&format!("{group}.")))
                .collect();
            validate_known_fields(roles, &group_path, &choices)?;
            for (name, value) in roles {
                if is_none(value) {
                    continue;
                }
                let role_path = format!("{group}.{name}");
                let role = ThemeRole::ALL
                    .iter()
                    .find(|role| role.config_path() == role_path)
                    .copied()
                    .expect("role key validated");
                layer
                    .roles
                    .insert(role, parse_style(value, &format!("{group_path}.{name}"))?);
            }
        }
        Ok(layer)
    }
}

fn unwrap_option(value: &spar::ConfigValue) -> &spar::ConfigValue {
    match value {
        spar::ConfigValue::Option(Some(value)) => unwrap_option(value),
        _ => value,
    }
}

fn is_none(value: &spar::ConfigValue) -> bool {
    matches!(value, spar::ConfigValue::Option(None))
}

pub(crate) fn expect_object<'a>(
    value: &'a spar::ConfigValue,
    path: &str,
) -> Result<&'a indexmap::IndexMap<String, spar::ConfigValue>, String> {
    match unwrap_option(value) {
        spar::ConfigValue::Object(fields) => Ok(fields),
        other => Err(format!(
            "{path} must be a section, got {}",
            other.type_name()
        )),
    }
}

pub(crate) fn expect_string<'a>(value: &'a spar::ConfigValue, path: &str) -> Result<&'a str, String> {
    match unwrap_option(value) {
        spar::ConfigValue::Str(text) => Ok(text),
        other => Err(format!("{path} must be str, got {}", other.type_name())),
    }
}

fn expect_bool(value: &spar::ConfigValue, path: &str) -> Result<bool, String> {
    match unwrap_option(value) {
        spar::ConfigValue::Bool(value) => Ok(*value),
        other => Err(format!("{path} must be bool, got {}", other.type_name())),
    }
}

fn validate_known_fields(
    fields: &indexmap::IndexMap<String, spar::ConfigValue>,
    path: &str,
    choices: &[&str],
) -> Result<(), String> {
    for name in fields.keys() {
        if !choices.contains(&name.as_str()) {
            let suggestion = crate::template::suggest(name, choices)
                .map(|candidate| format!("; did you mean `{candidate}`?"))
                .unwrap_or_default();
            return Err(format!("unknown theme field {path}.{name}{suggestion}"));
        }
    }
    Ok(())
}

fn parse_style(value: &spar::ConfigValue, path: &str) -> Result<ThemeStyleSpec, String> {
    let fields = expect_object(value, path)?;
    validate_known_fields(
        fields,
        path,
        &[
            "foreground",
            "background",
            "bold",
            "dim",
            "italic",
            "underline",
        ],
    )?;
    let mut style = ThemeStyleSpec::default();
    for (name, value) in fields {
        if is_none(value) {
            continue;
        }
        let field_path = format!("{path}.{name}");
        match name.as_str() {
            "foreground" => style.foreground = Some(parse_theme_color(value, &field_path)?),
            "background" => style.background = Some(parse_theme_color(value, &field_path)?),
            "bold" => style.bold = Some(expect_bool(value, &field_path)?),
            "dim" => style.dim = Some(expect_bool(value, &field_path)?),
            "italic" => style.italic = Some(expect_bool(value, &field_path)?),
            "underline" => style.underline = Some(expect_bool(value, &field_path)?),
            _ => unreachable!("style key validated"),
        }
    }
    Ok(style)
}

fn parse_theme_color(value: &spar::ConfigValue, path: &str) -> Result<ThemeColor, String> {
    let text = expect_string(value, path)?;
    if let Some(name) = text.strip_prefix('$') {
        let Some(key) = PaletteKey::parse(name) else {
            let choices = PaletteKey::ALL
                .iter()
                .map(|key| key.name())
                .collect::<Vec<_>>();
            let suggestion = crate::template::suggest(name, &choices)
                .map(|candidate| format!("; did you mean `${candidate}`?"))
                .unwrap_or_default();
            return Err(format!(
                "{path}: unknown palette reference `{text}`{suggestion}"
            ));
        };
        Ok(ThemeColor::Palette(key))
    } else {
        crate::parse_color(text)
            .map(ThemeColor::Literal)
            .map_err(|error| format!("{path}: {error}"))
    }
}

impl ThemeRole {
    pub const fn palette_key(self) -> PaletteKey {
        use PaletteKey as P;
        use ThemeRole as R;
        match self {
            R::Cwd
            | R::PromptMarker
            | R::MenuCount
            | R::MenuMatch
            | R::HintActive
            | R::HelpTitle
            | R::EditorHeader
            | R::PagerSearch
            | R::Builtin
            | R::Alias
            | R::SparSyntax
            | R::VirtualEnvironment
            | R::GitBranch
            | R::GitBehind
            | R::DataBool
            | R::CompletionKeyword => P::Accent,
            R::Success
            | R::GitClean
            | R::GitStaged
            | R::Function
            | R::ExternalCommand
            | R::QuotedString
            | R::DataString
            | R::TableHeader
            | R::TableIndex
            | R::ProjectNode
            | R::CompletionFunction
            | R::CompletionCommand
            | R::HelpHeading
            | R::HelpExample => P::Success,
            R::Failure | R::Error | R::UnknownCommand | R::GitConflict | R::FileExecutable => {
                P::Error
            }
            R::Warning
            | R::Duration
            | R::Option
            | R::GitDirty
            | R::GitModified
            | R::FileSpecial
            | R::ProjectRust
            | R::TypeName
            | R::HelpOption => P::Warning,
            R::Time
            | R::Path
            | R::Parameter
            | R::Operator
            | R::GitAhead
            | R::GitUntracked
            | R::DataKey
            | R::DataNumber
            | R::FileDirectory
            | R::FileSymlink
            | R::ProjectPython
            | R::ProjectFlutter
            | R::ProjectDart
            | R::ProjectGo
            | R::CompletionVariable
            | R::CompletionPath
            | R::CompletionDirectory
            | R::MenuFooter
            | R::HelpCrossReference => P::Info,
            R::Secondary
            | R::Comment
            | R::DataNull
            | R::DataPunct
            | R::TableBorder
            | R::HintHistory
            | R::HintSignature
            | R::MenuBorder
            | R::MenuDetail => P::Muted,
            R::Argument | R::MenuText | R::HelpBody | R::EditorPosition | R::CompletionType => {
                P::Text
            }
            R::MenuSelected
            | R::EditorSelection
            | R::EditorStatus
            | R::PagerSelected
            | R::PagerStatus => P::SelectionFg,
        }
    }
}

impl ThemeLayer {
    /// `generated` (tool-written layer) wins over `config` (the user's theme).
    pub fn resolved(config: &Self, generated: &Self) -> Self {
        let mut palette = config.palette.clone();
        palette.extend(generated.palette.iter().map(|(key, value)| (*key, *value)));
        let mut roles = BTreeMap::new();
        for role in ThemeRole::ALL {
            let mut style = default_role_style(*role);
            apply_resolved_layer(&mut style, *role, config, &palette);
            apply_resolved_layer(&mut style, *role, generated, &palette);
            roles.insert(*role, style);
        }
        Self { palette, roles }
    }
}

fn default_role_style(role: ThemeRole) -> ThemeStyleSpec {
    use ThemeRole as R;
    let (foreground, background, bold, dim, italic) = match role {
        R::Cwd | R::DataKey | R::FileDirectory | R::ProjectPython | R::CompletionDirectory => {
            (Some(12), None, true, false, false)
        }
        R::GitBranch | R::GitBehind => (Some(13), None, false, false, false),
        R::GitDirty | R::GitModified | R::Warning | R::FileSpecial | R::TypeName => {
            (Some(3), None, false, false, false)
        }
        R::GitClean
        | R::Success
        | R::Function
        | R::PromptMarker
        | R::TableHeader
        | R::ProjectNode
        | R::CompletionFunction
        | R::HelpHeading
        | R::HelpExample => (Some(10), None, true, false, false),
        R::GitStaged
        | R::ExternalCommand
        | R::QuotedString
        | R::DataString
        | R::CompletionCommand => (Some(10), None, false, false, false),
        R::GitUntracked | R::Path | R::CompletionPath | R::MenuFooter => {
            (Some(12), None, false, false, false)
        }
        R::GitConflict | R::Failure | R::Error | R::UnknownCommand | R::FileExecutable => {
            (Some(9), None, true, false, false)
        }
        R::GitAhead | R::Parameter | R::DataNumber | R::FileSymlink | R::CompletionVariable => {
            (Some(14), None, false, false, false)
        }
        R::Time
        | R::Builtin
        | R::MenuCount
        | R::HintActive
        | R::HelpTitle
        | R::HelpCrossReference => (Some(14), None, true, false, false),
        R::Duration | R::Option | R::HelpOption => (Some(11), None, false, false, false),
        R::ProjectRust | R::MenuMatch => (Some(11), None, true, false, false),
        R::Secondary | R::DataPunct | R::TableBorder | R::MenuBorder | R::HintSignature => {
            (Some(8), None, false, false, false)
        }
        R::Argument | R::MenuText | R::EditorPosition => (Some(7), None, false, false, false),
        R::Alias | R::SparSyntax | R::VirtualEnvironment | R::DataBool | R::CompletionKeyword => {
            (Some(13), None, true, false, false)
        }
        R::Operator => (Some(12), None, true, false, false),
        R::Comment | R::DataNull => (Some(8), None, false, false, true),
        R::ProjectFlutter | R::ProjectDart | R::ProjectGo => (Some(14), None, true, false, false),
        R::TableIndex => (Some(2), None, true, false, false),
        R::CompletionType => (Some(3), None, false, false, false),
        R::MenuSelected => (Some(0), Some(14), true, false, false),
        R::MenuDetail => (None, None, false, true, false),
        R::HintHistory => (Some(243), None, false, false, false),
        R::HelpBody | R::EditorSelection | R::PagerStatus | R::PagerSearch => {
            (None, None, false, false, false)
        }
        R::EditorHeader => (Some(6), None, true, false, false),
        R::EditorStatus => (Some(0), Some(6), false, false, false),
        R::PagerSelected => (Some(0), Some(14), false, false, false),
    };
    ThemeStyleSpec {
        foreground: foreground.map(|value| ThemeColor::Literal(ColorSpec::Indexed(value))),
        background: background.map(|value| ThemeColor::Literal(ColorSpec::Indexed(value))),
        bold: bold.then_some(true),
        dim: dim.then_some(true),
        italic: italic.then_some(true),
        underline: None,
    }
}

fn apply_resolved_layer(
    style: &mut ThemeStyleSpec,
    role: ThemeRole,
    layer: &ThemeLayer,
    palette: &BTreeMap<PaletteKey, ColorSpec>,
) {
    if let Some(color) = layer.palette.get(&role.palette_key()) {
        style.foreground = Some(ThemeColor::Literal(*color));
    }
    if role.palette_key() == PaletteKey::SelectionFg {
        if let Some(color) = layer.palette.get(&PaletteKey::SelectionBg) {
            style.background = Some(ThemeColor::Literal(*color));
        }
    }
    if let Some(upper) = layer.roles.get(&role) {
        let resolve = |value: ThemeColor| match value {
            ThemeColor::Literal(color) => Some(color),
            ThemeColor::Palette(key) => palette.get(&key).copied(),
        };
        if let Some(color) = upper.foreground.and_then(resolve) {
            style.foreground = Some(ThemeColor::Literal(color));
        }
        if let Some(color) = upper.background.and_then(resolve) {
            style.background = Some(ThemeColor::Literal(color));
        }
        if upper.bold.is_some() {
            style.bold = upper.bold;
        }
        if upper.dim.is_some() {
            style.dim = upper.dim;
        }
        if upper.italic.is_some() {
            style.italic = upper.italic;
        }
        if upper.underline.is_some() {
            style.underline = upper.underline;
        }
    }
}
