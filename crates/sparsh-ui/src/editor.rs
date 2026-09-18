use std::io::{self, Write};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use reedline::{Reedline, Signal};
use sparsh_core::{ShellResult, ShellSession};

use crate::{
    render_error, render_result, ColorPolicy, GitProbe, PromptData, PromptState, SparshHighlighter,
};

pub fn run_interactive(session: &mut ShellSession, color: ColorPolicy) -> io::Result<i32> {
    let theme = color.theme();
    let snapshot = Arc::new(RwLock::new(session.ui_snapshot()));
    let highlighter = SparshHighlighter::new(Arc::clone(&snapshot), theme.clone());
    let mut editor = Reedline::create()
        .with_ansi_colors(color == ColorPolicy::Auto)
        .with_highlighter(Box::new(highlighter));
    let prompt_state = PromptState::new(Duration::from_secs(2));
    let mut git = GitProbe::new();
    let mut previous_duration = None;

    loop {
        let current = session.ui_snapshot();
        if let Ok(mut shared) = snapshot.write() {
            *shared = current.clone();
        }
        let prompt = prompt_state.prompt(
            &PromptData {
                cwd: current.cwd().to_path_buf(),
                home: current.home().map(ToOwned::to_owned),
                git: git.state(current.cwd()),
                previous_status: session.last_status(),
                previous_duration,
            },
            &theme,
        );

        let signal = editor
            .read_line(&prompt)
            .map_err(|error| io::Error::other(error.to_string()))?;
        match signal {
            Signal::Success(source) => {
                let started = Instant::now();
                let result = session.submit(&source);
                previous_duration = Some(started.elapsed());
                match result {
                    Ok(result) => {
                        let exit_status = match result {
                            ShellResult::Exit(status) => Some(status),
                            _ => None,
                        };
                        let stdout = io::stdout();
                        render_result(&result, &theme, true, &mut stdout.lock())?;
                        if let Some(status) = exit_status {
                            return Ok(status);
                        }
                    }
                    Err(error) => {
                        let stderr = io::stderr();
                        render_error(&error, Some(&source), &theme, &mut stderr.lock())?;
                    }
                }
            }
            Signal::CtrlC | Signal::ExternalBreak(_) | Signal::HostCommand(_) => {
                io::stderr().flush()?;
            }
            Signal::CtrlD => return Ok(session.last_status()),
            _ => {}
        }
    }
}
