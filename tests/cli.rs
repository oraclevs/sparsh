use std::io::Write;
use std::process::{Command, Stdio};

fn sparsh() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sparsh"))
}

#[test]
fn dash_c_runs_external_commands_without_a_foreign_shell() {
    let output = sparsh().args(["-c", "printf hello"]).output().unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout, b"hello");
    assert!(output.stderr.is_empty());
}

#[test]
fn dash_c_supports_pipeline_and_redirect_syntax() {
    let directory = tempfile::tempdir().unwrap();
    let output = sparsh()
        .current_dir(directory.path())
        .args(["-c", "printf \"alpha\\nbeta\\n\" | grep beta > result.txt"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("result.txt")).unwrap(),
        "beta\n"
    );
}

#[test]
fn dash_c_returns_process_and_explicit_exit_statuses() {
    assert_eq!(
        sparsh().args(["-c", "false"]).status().unwrap().code(),
        Some(1)
    );
    assert_eq!(
        sparsh().args(["-c", "exit 7"]).status().unwrap().code(),
        Some(7)
    );
}

#[test]
fn dash_c_pwd_uses_the_child_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    let output = sparsh()
        .current_dir(directory.path())
        .args(["-c", "pwd"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n", directory.path().display())
    );
}

#[test]
fn piped_stdin_is_prompt_free_and_preserves_spar_state() {
    let mut child = sparsh()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"var project: str = \"spar\";\nproject\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout, b"\"spar\"\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn help_and_version_do_not_require_session_startup() {
    let help = sparsh().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout)
        .unwrap()
        .contains("sparsh -c <input>"));

    let version = sparsh().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(version.stdout, b"sparsh 0.1.0\n");
}

#[test]
fn invalid_arguments_exit_with_usage_status() {
    let output = sparsh().arg("--unknown").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr).unwrap().contains("usage:"));
}
