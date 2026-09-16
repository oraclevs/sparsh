use std::io::{self, BufRead, Write};

use sparsh_core::{render_value, ShellError, ShellResult, ShellSession};

mod prompt;
mod theme;

pub use prompt::{GitState, PromptData, PromptState, SparshPrompt};
pub use theme::{SemanticRole, Theme};

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
}

pub fn render_result<W: Write>(result: &ShellResult, out: &mut W) -> io::Result<()> {
    match result {
        ShellResult::Value(value) => writeln!(out, "{}", render_value(value)),
        ShellResult::Builtin(output) => {
            if let Some(text) = &output.stdout {
                out.write_all(text.as_bytes())?;
            }
            Ok(())
        }
        ShellResult::Empty | ShellResult::Process(_) | ShellResult::Exit(_) => Ok(()),
    }
}

pub fn render_error<W: Write>(error: &ShellError, err: &mut W) -> io::Result<()> {
    writeln!(err, "{error}")
}

pub fn run_loop<R, W, E>(
    session: &mut ShellSession,
    mut input: R,
    out: &mut W,
    err: &mut E,
    interactive: bool,
    _color: ColorPolicy,
) -> io::Result<i32>
where
    R: BufRead,
    W: Write,
    E: Write,
{
    let mut line = String::new();
    loop {
        if interactive {
            err.write_all("❯ ".as_bytes())?;
            err.flush()?;
        }
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(session.last_status());
        }
        let submitted = line.trim_end_matches(['\r', '\n']);
        match session.submit(submitted) {
            Ok(result) => {
                let exit_status = match result {
                    ShellResult::Exit(status) => Some(status),
                    _ => None,
                };
                render_result(&result, out)?;
                if let Some(status) = exit_status {
                    return Ok(status);
                }
            }
            Err(error) => render_error(&error, err)?,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use sparsh_core::{BuiltinOutput, ShellResult, ShellSession};

    use super::{render_result, run_loop, ColorPolicy};

    #[test]
    fn noninteractive_loop_keeps_one_session_and_renders_a_value() {
        let mut session = ShellSession::new();
        let input = Cursor::new(b"var project: str = \"spar\";\nproject\n");
        let mut output = Vec::new();
        let mut errors = Vec::new();

        let status = run_loop(
            &mut session,
            input,
            &mut output,
            &mut errors,
            false,
            ColorPolicy::Never,
        )
        .unwrap();

        assert_eq!(status, 0);
        assert_eq!(String::from_utf8(output).unwrap(), "\"spar\"\n");
        assert!(errors.is_empty());
    }

    #[test]
    fn interactive_loop_prints_a_prompt_to_stderr_only() {
        let mut session = ShellSession::new();
        let mut output = Vec::new();
        let mut errors = Vec::new();

        run_loop(
            &mut session,
            Cursor::new(Vec::<u8>::new()),
            &mut output,
            &mut errors,
            true,
            ColorPolicy::Auto,
        )
        .unwrap();

        assert!(output.is_empty());
        assert_eq!(String::from_utf8(errors).unwrap(), "❯ ");
    }

    #[test]
    fn builtin_output_is_plain_and_process_outcomes_add_no_decoration() {
        let mut output = Vec::new();
        render_result(
            &ShellResult::Builtin(BuiltinOutput {
                stdout: Some("/tmp\n".into()),
                status: 0,
            }),
            &mut output,
        )
        .unwrap();
        let mut session = ShellSession::new();
        let process = session.submit("true").unwrap();
        assert!(matches!(process, ShellResult::Process(_)));
        render_result(&process, &mut output).unwrap();

        assert_eq!(String::from_utf8(output).unwrap(), "/tmp\n");
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
}
