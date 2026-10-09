mod alias;
pub mod builtin;
mod builtin_tables;
mod command_args;
mod completion;
mod config;
mod config_home;
mod directory;
mod dispatch;
mod environment;
mod execute;
mod function_pipeline;
mod help_pages;
mod history;
mod job;
mod keybinding;
pub mod listing;
mod path;
mod prompt_config;
mod resolver;
mod services;
mod session;
mod shell_input;
mod template;
mod theme_config;
mod theme_command;
mod theme_file;
pub mod value;
mod zdir;

pub use builtin::{BuiltinError, BuiltinMetadata, BuiltinOutput, BuiltinRegistry};
pub use completion::{
    complete, signature_hint, signature_hint_info, CompletionContext, CompletionItem,
    CompletionRequest, CompletionSnapshot, ItemKind, SignatureHint,
};
pub use config::{
    CompletionConfig, ConfigLoadError, EnvironmentVariableConfig, HistoryConfig, PromptConfig,
    PromptGitConfig, PromptPathConfig, PromptTimeConfig, SparshConfig, ThemeKindConfig,
};
pub use help_pages::{help_page, render_text as render_help_text, HelpPage};
pub use history::{HistoryAccess, HistoryRecord, HistorySettings};
pub use job::{JobId, JobState, ShellJob};
pub use keybinding::{
    default_pager_keybindings, merged_pager_keybindings, KeyChord, KeybindingAction,
    KeybindingConfig, KeybindingKey, PagerAction, PagerKeybindingConfig, KEYBINDING_ACTION_NAMES,
    PAGER_ACTION_NAMES,
};
pub use prompt_config::{
    legacy_time_format_to_strftime, parse_color, ColorSpec, NeededWidgets, PromptIssue,
    RightPromptConfig, SlotConfig, SlotSpec, TextStyle, Threshold, Thresholds,
};
pub use session::{
    CommandDiagnostic, CommandKind, EditorMode, SessionMode, ShellError, ShellResult, ShellSession,
    ShellUiSnapshot, StartupMode, StatementOutcome,
};
pub use spar::InputCompleteness;
pub use template::{suggest, Piece, Template, WidgetKind, WidgetRef};
pub use theme_config::{PaletteKey, ThemeColor, ThemeLayer, ThemeRole, ThemeStyleSpec};
pub use theme_file::{ThemeFileState, ThemeRefresh, ThemeSource};
pub use value::render_value;
pub use zdir::{rank as rank_directories, DirAlias, DirVisit};

pub const PRODUCT_NAME: &str = "sparsh";

pub fn input_completeness(source: &str) -> InputCompleteness {
    if shell_input::needs_more_input(source) {
        InputCompleteness::Incomplete
    } else {
        spar::input_completeness(source)
    }
}

#[cfg(test)]
pub(crate) static PROCESS_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    #[test]
    fn identifies_the_core_product() {
        assert_eq!(super::PRODUCT_NAME, "sparsh");
    }
}
