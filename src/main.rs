use std::io::{self, IsTerminal, Write};

use sparsh_core::{ShellResult, ShellSession};
use sparsh_ui::{render_error, render_result, run_loop, ColorPolicy, Theme};

const HELP: &str = "\
Sparsh — the Spar shell

usage:
  sparsh
  sparsh -c <input>
  sparsh --help
  sparsh --version
";

enum Mode {
    Interactive,
    Command(String),
    Help,
    Version,
}

fn parse_args(arguments: impl IntoIterator<Item = String>) -> Result<Mode, String> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(Mode::Interactive),
        [flag] if flag == "--help" => Ok(Mode::Help),
        [flag] if flag == "--version" => Ok(Mode::Version),
        [flag, input] if flag == "-c" => Ok(Mode::Command(input.clone())),
        _ => Err("invalid arguments".into()),
    }
}

fn run_command(input: &str) -> i32 {
    let mut session = ShellSession::new();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    match session.submit(input) {
        Ok(result) => {
            let status = match &result {
                ShellResult::Empty | ShellResult::Value(_) => 0,
                ShellResult::Builtin(output) => output.status,
                ShellResult::Process(outcome) => outcome.exit_code,
                ShellResult::Exit(status) => *status,
            };
            if let Err(error) = render_result(&result, &Theme::plain(), false, &mut out) {
                let _ = writeln!(err, "sparsh: {error}");
                return 1;
            }
            status
        }
        Err(error) => {
            let status = error.status();
            if render_error(&error, Some(input), &Theme::plain(), &mut err).is_err() {
                return 1;
            }
            status
        }
    }
}

fn run_interactive() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let interactive = stdin.is_terminal() && stderr.is_terminal();
    let color = ColorPolicy::for_environment(interactive, std::env::var_os("NO_COLOR").is_some());
    let mut session = ShellSession::new();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    match run_loop(
        &mut session,
        stdin.lock(),
        &mut out,
        &mut err,
        interactive,
        color,
    ) {
        Ok(status) => status,
        Err(error) => {
            let _ = writeln!(err, "sparsh: {error}");
            1
        }
    }
}

fn run() -> i32 {
    match parse_args(std::env::args().skip(1)) {
        Ok(Mode::Interactive) => run_interactive(),
        Ok(Mode::Command(input)) => run_command(&input),
        Ok(Mode::Help) => {
            print!("{HELP}");
            0
        }
        Ok(Mode::Version) => {
            println!("sparsh {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Err(message) => {
            eprintln!("sparsh: {message}\n\n{HELP}");
            2
        }
    }
}

fn main() {
    std::process::exit(run());
}
