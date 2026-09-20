mod alias;
pub mod builtin;
mod completion;
mod config;
mod directory;
mod dispatch;
mod environment;
mod execute;
mod function_pipeline;
mod history;
mod job;
mod path;
mod prompt_config;
mod resolver;
mod services;
mod session;
mod template;
pub mod value;

pub use builtin::{BuiltinError, BuiltinMetadata, BuiltinOutput, BuiltinRegistry};
pub use completion::{complete, CompletionContext, CompletionItem, CompletionRequest, CompletionSnapshot};
pub use config::{
    CompletionConfig, ConfigLoadError, EnvironmentVariableConfig, HistoryConfig, PromptConfig,
    PromptGitConfig, PromptPathConfig, PromptTimeConfig, SparshConfig,
};
pub use history::{HistoryAccess, HistorySettings};
pub use spar::InputCompleteness;
pub use job::{JobId, JobState, ShellJob};
pub use session::{
    CommandDiagnostic, CommandKind, EditorMode, SessionMode, ShellError, ShellResult, ShellSession,
    ShellUiSnapshot, StartupMode,
};
pub use prompt_config::{
    legacy_time_format_to_strftime, parse_color, ColorSpec, NeededWidgets, PromptIssue,
    RightPromptConfig, SlotConfig, SlotSpec, TextStyle, Threshold, Thresholds,
};
pub use template::{suggest, Piece, Template, WidgetKind, WidgetRef};
pub use value::render_value;

pub const PRODUCT_NAME: &str = "sparsh";

pub fn input_completeness(source: &str) -> InputCompleteness {
    spar::input_completeness(source)
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
