use std::io::{self, BufRead, Write};

use sparsh_core::{render_value, CommandDiagnostic, ShellResult, ShellSession};

mod command_editor;
mod completion;
mod data_view;
mod diagnostic;
mod editor;
mod encoded;
mod git;
mod highlight;
mod history;
mod http_view;
mod paste;
mod project;
mod prompt;
mod sampler;
mod structured;
mod styled;
mod theme;
mod validator;
mod widgets;
mod width;

pub use command_editor::edit_buffer_file;
pub use completion::SparshCompleter;
pub use diagnostic::{render_error, render_error_text, render_prompt_issues};
pub use editor::run_interactive;
pub use git::GitProbe;
pub use highlight::SparshHighlighter;
pub use paste::{
    is_multiline_paste_candidate, multiline_submissions, review_multiline_paste, PasteDecision,
};
pub use project::{active_python_environment, detect_projects, ProjectKind};
pub use prompt::{GitState, PromptData, PromptState, SparshPrompt};
pub use sampler::{
    BatteryReading, BatteryState, DiskUsage, MemUsage, SystemSampler, SystemSnapshot,
};
pub use theme::{SemanticRole, Theme};
pub use validator::SparshValidator;
pub use widgets::{render_slot, Level, LocalTime, RenderedPiece, RenderedSlot, WidgetInputs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorPolicy {
    Auto,
    Never,
}

impl ColorPolicy {
    pub fn for_environment(interactive: bool, no_color_is_set: bool) -> Self {
        if interactive && !no_color_is_set {
            Self::Auto
        } else {
            Self::Never
        }
    }

    pub fn theme(self) -> Theme {
        match self {
            Self::Auto => Theme::colored(),
            Self::Never => Theme::plain(),
        }
    }
}

pub fn render_result<W: Write>(
    result: &ShellResult,
    theme: &Theme,
    interactive: bool,
    out: &mut W,
) -> io::Result<()> {
    match result {
        ShellResult::Value(value) => writeln!(out, "{}", render_value(value)),
        ShellResult::Structured(value) if !interactive => {
            writeln!(out, "{}", structured::render_structured_plain(value))
        }
        ShellResult::Structured(value) => {
            let options = data_view::RenderOptions::for_terminal();
            writeln!(
                out,
                "{}",
                structured::render_structured_value(value, theme, &options)
            )?;
            if value.stream_preview && value.truncated {
                writeln!(
                    out,
                    "{}",
                    theme.paint(SemanticRole::Secondary, "… preview truncated")
                )?;
            }
            Ok(())
        }
        ShellResult::Builtin(output) => {
            let _ = (theme, interactive);
            // Builtin command output is command output, not Sparsh UI. Preserve
            // bytes exactly just like external commands (important for printf,
            // redirected data, and ANSI-aware user tooling).
            out.write_all(&output.stdout)?;
            Ok(())
        }
        ShellResult::BackgroundJob { id, pgid } => writeln!(out, "[{id}] {pgid}"),
        ShellResult::Empty
        | ShellResult::EditorMode(_)
        | ShellResult::ReloadConfig
        | ShellResult::ReloadConfigWith(_)
        | ShellResult::ExecRequest { .. }
        | ShellResult::SourceRequest(_)
        | ShellResult::Process(_)
        | ShellResult::CommandStatus { .. }
        | ShellResult::Exit(_) => Ok(()),
    }
}

pub fn render_command_diagnostic<W: Write>(
    diagnostic: &CommandDiagnostic,
    source: Option<&str>,
    theme: &Theme,
    err: &mut W,
) -> io::Result<()> {
    match diagnostic {
        CommandDiagnostic::NotFound {
            program,
            suggestions,
        } => {
            let error = sparsh_core::ShellError::CommandNotFound {
                program: program.clone(),
                suggestions: suggestions.clone(),
            };
            render_error(&error, source, theme, err)
        }
    }
}

pub fn run_noninteractive_loop<R, W, E>(
    session: &mut ShellSession,
    mut input: R,
    out: &mut W,
    err: &mut E,
) -> io::Result<i32>
where
    R: BufRead,
    W: Write,
    E: Write,
{
    let mut line = String::new();
    let theme = Theme::plain();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(session.last_status());
        }
        let submitted = line.trim_end_matches(['\r', '\n']);
        match session.submit_script(submitted) {
            Ok(result) => {
                let exit_status = match &result {
                    ShellResult::Exit(status) => Some(*status),
                    _ => None,
                };
                render_result(&result, &theme, false, out)?;
                if let ShellResult::CommandStatus {
                    diagnostic: Some(diagnostic),
                    ..
                } = &result
                {
                    render_command_diagnostic(diagnostic, Some(submitted), &theme, err)?;
                }
                if let Some(status) = exit_status {
                    return Ok(status);
                }
            }
            Err(error) => render_error(&error, Some(submitted), &theme, err)?,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use sparsh_core::{BuiltinOutput, ShellResult, ShellSession};

    use super::{render_result, run_noninteractive_loop, ColorPolicy, Theme};

    #[test]
    fn noninteractive_loop_keeps_one_session_and_renders_a_value() {
        let mut session = ShellSession::new();
        let input = Cursor::new(b"var project: str = \"spar\";\nproject\n");
        let mut output = Vec::new();
        let mut errors = Vec::new();

        let status =
            run_noninteractive_loop(&mut session, input, &mut output, &mut errors).unwrap();

        assert_eq!(status, 0);
        assert_eq!(String::from_utf8(output).unwrap(), "\"spar\"\n");
        assert!(errors.is_empty());
    }

    #[test]
    fn builtin_output_is_plain_and_process_outcomes_add_no_decoration() {
        let mut output = Vec::new();
        render_result(
            &ShellResult::Builtin(BuiltinOutput {
                stdout: b"/tmp\n".to_vec(),
                stderr: Vec::new(),
                status: 0,
            }),
            &Theme::plain(),
            false,
            &mut output,
        )
        .unwrap();
        let mut session = ShellSession::new();
        let process = session.submit("true").unwrap();
        assert!(matches!(&process, ShellResult::Process(_)));
        render_result(&process, &Theme::plain(), false, &mut output).unwrap();

        assert_eq!(String::from_utf8(output).unwrap(), "/tmp\n");
    }

    #[test]
    fn interactive_builtin_command_output_is_not_recolored() {
        let mut output = Vec::new();

        render_result(
            &ShellResult::Builtin(BuiltinOutput {
                stdout: b"/tmp\n".to_vec(),
                stderr: Vec::new(),
                status: 0,
            }),
            &Theme::colored(),
            true,
            &mut output,
        )
        .unwrap();

        assert_eq!(output.as_slice(), b"/tmp\n");
    }

    #[test]
    fn structured_tables_render_with_headers_rows_and_stream_preview_marker() {
        use indexmap::IndexMap;

        let table = spar::TableValue::from_records(vec![
            spar::Value::Object(IndexMap::from([
                ("name".into(), spar::Value::String("Obi".into())),
                ("age".into(), spar::Value::Int(24)),
            ])),
            spar::Value::Object(IndexMap::from([
                ("name".into(), spar::Value::String("Ada".into())),
                ("age".into(), spar::Value::Int(31)),
            ])),
        ])
        .unwrap();
        let result = ShellResult::Structured(spar::InteractiveRuntimeValue {
            value: spar::Value::Table(table),
            stream_preview: true,
            truncated: true,
            presentation: spar::InteractivePresentation::Value,
        });
        let mut output = Vec::new();

        render_result(&result, &Theme::plain(), true, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("age"));
        assert!(output.contains("name"));
        assert!(output.contains("24"));
        assert!(output.contains("Obi"));
        assert!(output.contains("31"));
        assert!(output.contains("Ada"));
        assert!(output.contains("preview truncated"));
    }

    #[test]
    fn schema_and_inspect_use_dedicated_interactive_presentations() {
        use indexmap::IndexMap;

        let table = spar::TableValue::from_records(vec![spar::Value::Object(IndexMap::from([
            ("name".into(), spar::Value::String("Obi".into())),
            ("age".into(), spar::Value::Int(24)),
        ]))])
        .unwrap();

        let schema_result = ShellResult::Structured(spar::InteractiveRuntimeValue {
            value: spar::Value::Schema(table.schema().clone()),
            stream_preview: false,
            truncated: false,
            presentation: spar::InteractivePresentation::Schema,
        });
        let mut schema_output = Vec::new();
        render_result(&schema_result, &Theme::plain(), true, &mut schema_output).unwrap();
        let schema_output = String::from_utf8(schema_output).unwrap();
        assert!(schema_output.contains("Schema"));
        assert!(schema_output.contains("field"));
        assert!(schema_output.contains("type"));
        assert!(schema_output.contains("optional"));
        assert!(schema_output.contains("name"));
        assert!(schema_output.contains("str"));

        let inspect_result = ShellResult::Structured(spar::InteractiveRuntimeValue {
            value: spar::Value::Table(table),
            stream_preview: false,
            truncated: false,
            presentation: spar::InteractivePresentation::Inspect,
        });
        let mut inspect_output = Vec::new();
        render_result(&inspect_result, &Theme::plain(), true, &mut inspect_output).unwrap();
        let inspect_output = String::from_utf8(inspect_output).unwrap();
        assert!(inspect_output.contains("Inspect"));
        assert!(inspect_output.contains("type: Table"));
        assert!(inspect_output.contains("rows: 1"));
        assert!(inspect_output.contains("schema:"));
        assert!(inspect_output.contains("value:"));
        assert!(inspect_output.contains("Obi"));
    }

    #[test]
    fn no_color_and_noninteractive_modes_disable_style() {
        assert_eq!(ColorPolicy::for_environment(true, false), ColorPolicy::Auto);
        assert_eq!(ColorPolicy::for_environment(true, true), ColorPolicy::Never);
        assert_eq!(
            ColorPolicy::for_environment(false, false),
            ColorPolicy::Never
        );
    }

    #[test]
    fn no_color_policy_keeps_structured_layout_without_ansi() {
        use indexmap::IndexMap;

        let table = spar::TableValue::from_records(vec![spar::Value::Object(IndexMap::from([
            ("name".into(), spar::Value::String("Obi".into())),
            ("age".into(), spar::Value::Int(24)),
        ]))])
        .unwrap();
        let result = ShellResult::Structured(spar::InteractiveRuntimeValue {
            value: spar::Value::Table(table),
            stream_preview: false,
            truncated: false,
            presentation: spar::InteractivePresentation::Pipeline,
        });
        let theme = ColorPolicy::for_environment(true, true).theme();
        let mut output = Vec::new();

        render_result(&result, &theme, true, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains('╭') && output.contains('│') && output.contains('╰'));
        assert!(output.contains("Obi"));
        assert!(!output.contains("\x1b["), "{output:?}");
    }
}
