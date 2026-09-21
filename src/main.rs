use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use sparsh_core::{SessionMode, ShellResult, ShellSession, StartupMode};
use sparsh_ui::{
    edit_buffer_file, render_command_diagnostic, render_error, render_result, run_interactive,
    run_noninteractive_loop, ColorPolicy, Theme,
};

const HELP: &str = "\
Sparsh — the Spar shell

usage:
  sparsh
  sparsh -c <input>
  sparsh --login
  sparsh --remote-command <input>
  sparsh --help
  sparsh --version
";

const BUILD_FINGERPRINT: &str = "foundation-v2.9";

fn version_string() -> String {
    format!("sparsh {} ({BUILD_FINGERPRINT})", env!("CARGO_PKG_VERSION"))
}

enum CliMode {
    Default,
    EditBuffer(PathBuf),
    Startup(StartupMode),
    Help,
    Version,
}

fn parse_args(arguments: impl IntoIterator<Item = String>) -> Result<CliMode, String> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(CliMode::Default),
        [flag] if flag == "--help" => Ok(CliMode::Help),
        [flag] if flag == "--version" => Ok(CliMode::Version),
        [flag] if flag == "--login" => Ok(CliMode::Startup(StartupMode::Login)),
        [flag, path] if flag == "--edit-buffer" => {
            Ok(CliMode::EditBuffer(PathBuf::from(path.as_str())))
        }
        [flag, input] if flag == "-c" => Ok(CliMode::Startup(StartupMode::Command(input.clone()))),
        [flag, input] if flag == "--remote-command" => {
            Ok(CliMode::Startup(StartupMode::RemoteCommand(input.clone())))
        }
        _ => Err("invalid arguments".into()),
    }
}

fn load_config_or_report(session: &mut ShellSession, err: &mut impl Write) {
    if let Err(error) = session.reload_config() {
        let _ = render_error(&error, None, &Theme::plain(), err);
    }
}

fn run_one_command(startup: StartupMode, input: String) -> i32 {
    let mut session = match ShellSession::try_new_for(&startup, false, false) {
        Ok(session) => session,
        Err(error) => {
            let _ = render_error(&error, None, &Theme::plain(), &mut io::stderr().lock());
            return error.status();
        }
    };
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    load_config_or_report(&mut session, &mut err);

    match session.submit_script(&input) {
        Ok(result) => {
            let status = result_status(&result);
            if let Err(error) = render_result(&result, &Theme::plain(), false, &mut out) {
                let _ = writeln!(err, "sparsh: {error}");
                return 1;
            }
            if let ShellResult::CommandStatus {
                diagnostic: Some(diagnostic),
                ..
            } = &result
            {
                if render_command_diagnostic(diagnostic, Some(&input), &Theme::plain(), &mut err)
                    .is_err()
                {
                    return 1;
                }
            }
            status
        }
        Err(error) => {
            let status = error.status();
            if render_error(&error, Some(&input), &Theme::plain(), &mut err).is_err() {
                return 1;
            }
            status
        }
    }
}

fn run_stream(startup: StartupMode) -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let stdin_is_tty = stdin.is_terminal();
    let stderr_is_tty = stderr.is_terminal();
    let session_mode = startup.session_mode(stdin_is_tty, stderr_is_tty);
    let interactive = session_mode == SessionMode::InteractiveTty;
    let color = ColorPolicy::for_environment(
        interactive && stdout.is_terminal(),
        std::env::var_os("NO_COLOR").is_some(),
    );

    let mut session = match ShellSession::try_new_for(&startup, stdin_is_tty, stderr_is_tty) {
        Ok(session) => session,
        Err(error) => {
            let mut err = stderr.lock();
            let _ = render_error(&error, None, &Theme::plain(), &mut err);
            return error.status();
        }
    };
    load_config_or_report(&mut session, &mut stderr.lock());

    let result = if interactive {
        run_interactive(&mut session, color)
    } else {
        run_noninteractive_loop(
            &mut session,
            stdin.lock(),
            &mut stdout.lock(),
            &mut stderr.lock(),
        )
    };
    match result {
        Ok(status) => status,
        Err(error) => {
            let _ = writeln!(stderr.lock(), "sparsh: {error}");
            1
        }
    }
}

fn result_status(result: &ShellResult) -> i32 {
    match result {
        ShellResult::Empty
        | ShellResult::Value(_)
        | ShellResult::Structured(_)
        | ShellResult::EditorMode(_)
        | ShellResult::ReloadConfig
        | ShellResult::ExecRequest { .. }
        | ShellResult::SourceRequest(_) => 0,
        ShellResult::Builtin(output) => output.status,
        ShellResult::Process(outcome) => outcome.exit_code,
        ShellResult::BackgroundJob { .. } => 0,
        ShellResult::CommandStatus { status, .. } => *status,
        ShellResult::Exit(status) => *status,
    }
}

fn run() -> i32 {
    match parse_args(std::env::args().skip(1)) {
        Ok(CliMode::Default) => {
            let startup = if io::stdin().is_terminal() && io::stderr().is_terminal() {
                StartupMode::Interactive
            } else {
                StartupMode::Stdin
            };
            run_stream(startup)
        }
        Ok(CliMode::EditBuffer(path)) => match edit_buffer_file(&path) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("sparsh editor: {error}");
                1
            }
        },
        Ok(CliMode::Startup(StartupMode::Command(input))) => {
            run_one_command(StartupMode::Command(input.clone()), input)
        }
        Ok(CliMode::Startup(StartupMode::RemoteCommand(input))) => {
            run_one_command(StartupMode::RemoteCommand(input.clone()), input)
        }
        Ok(CliMode::Startup(mode @ StartupMode::Login)) => run_stream(mode),
        Ok(CliMode::Startup(mode @ StartupMode::Interactive)) => run_stream(mode),
        Ok(CliMode::Startup(mode @ StartupMode::Stdin)) => run_stream(mode),
        Ok(CliMode::Help) => {
            print!("{HELP}");
            0
        }
        Ok(CliMode::Version) => {
            println!("{}", version_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parser_accepts_hidden_sparsh_buffer_editor_mode() {
        let mode = parse_args([
            "--edit-buffer".to_string(),
            "/tmp/sparsh-buffer.spar".to_string(),
        ])
        .unwrap();

        assert!(matches!(
            mode,
            CliMode::EditBuffer(path) if path.as_path() == std::path::Path::new("/tmp/sparsh-buffer.spar")
        ));
    }

    #[test]
    fn cli_parser_distinguishes_login_remote_and_command_modes() {
        assert!(matches!(
            parse_args(Vec::<String>::new()).unwrap(),
            CliMode::Default
        ));
        assert!(matches!(
            parse_args(["--login".to_string()]).unwrap(),
            CliMode::Startup(StartupMode::Login)
        ));
        assert!(matches!(
            parse_args(["-c".to_string(), "true".to_string()]).unwrap(),
            CliMode::Startup(StartupMode::Command(command)) if command == "true"
        ));
        assert!(matches!(
            parse_args(["--remote-command".to_string(), "true".to_string()]).unwrap(),
            CliMode::Startup(StartupMode::RemoteCommand(command)) if command == "true"
        ));
    }

    #[test]
    fn version_identifies_foundation_patch_build() {
        assert_eq!(version_string(), "sparsh 0.1.0 (foundation-v2.9)");
    }
}
