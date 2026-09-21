use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn sparsh() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sparsh"));
    // CLI tests must never accidentally load the developer's real rc file.
    command.env_remove("HOME").env_remove("XDG_CONFIG_HOME");
    command
}

fn write_rc(home: &Path, source: &str) {
    let directory = home.join(".sparsh");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("sparsh.spar"), source).unwrap();
}

fn write_config_rc(home: &Path, fields: &str) {
    write_rc(
        home,
        &format!(
            r#"type TestAlias {{ name: str; command: List<str>; }};
type TestEnvironment {{ name: str; value: str; }};
type TestCompletion {{ enabled?: bool; }};
struct Config {{
{fields}
}};
"#
        ),
    );
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
fn exec_builtin_replaces_sparsh_with_external_process() {
    let output = sparsh()
        .args(["-c", "exec printf exec-ok"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"exec-ok");
}

#[test]
fn exec_builtin_preserves_redirections_from_the_original_command() {
    let directory = tempfile::tempdir().unwrap();
    let output = sparsh()
        .current_dir(directory.path())
        .args(["-c", "exec printf redirected > exec.txt"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("exec.txt")).unwrap(),
        "redirected"
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
fn piped_stdin_can_call_a_persistent_spar_function() {
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
        .write_all(b"function answer() -> int { return 42; };\nanswer()\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"42\n");
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
    assert_eq!(version.stdout, b"sparsh 0.1.0 (foundation-v2.9)\n");
}

#[test]
fn invalid_arguments_exit_with_usage_status() {
    let output = sparsh().arg("--unknown").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr).unwrap().contains("usage:"));
}

#[test]
fn invalid_startup_config_reports_diagnostic_but_command_still_runs_with_defaults() {
    let home = tempfile::tempdir().unwrap();
    write_config_rc(
        home.path(),
        "    completion: TestCompletion = { enabled: ; };",
    );

    let output = sparsh()
        .env("HOME", home.path())
        .args(["-c", "printf ok"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout, b"ok");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("error["), "{stderr}");
}

#[test]
fn canonical_home_rc_alias_and_environment_are_visible_to_dash_c() {
    let home = tempfile::tempdir().unwrap();
    write_config_rc(
        home.path(),
        r#"    aliases: List<TestAlias> = [{ name: "configured"; command: ["printf", "alias-ok"]; }];
    environment: List<TestEnvironment> = [{ name: "SPARSH_CONFIG_VALUE"; value: "env-ok"; }];"#,
    );

    let alias = sparsh()
        .env("HOME", home.path())
        .args(["-c", "configured"])
        .output()
        .unwrap();
    assert!(
        alias.status.success(),
        "{}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(alias.stdout, b"alias-ok");

    let environment = sparsh()
        .env("HOME", home.path())
        .args(["-c", "printenv SPARSH_CONFIG_VALUE"])
        .output()
        .unwrap();
    assert!(
        environment.status.success(),
        "{}",
        String::from_utf8_lossy(&environment.stderr)
    );
    assert_eq!(environment.stdout, b"env-ok\n");
}

#[test]
fn dash_c_can_call_named_argument_function_declared_in_sparsh_rc() {
    let home = tempfile::tempdir().unwrap();
    write_rc(
        home.path(),
        r#"function greet(prefix: str, name: str) -> str {
    return "${prefix}, ${name}";
};"#,
    );

    let output = sparsh()
        .env("HOME", home.path())
        .args(["-c", "greet(prefix: \"Hello\", name: \"OCC\")"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Hello, OCC\n");
}

#[test]
fn remote_command_uses_noninteractive_execution_and_canonical_rc() {
    let home = tempfile::tempdir().unwrap();
    write_config_rc(
        home.path(),
        r#"    environment: List<TestEnvironment> = [{ name: "SPARSH_REMOTE_VALUE"; value: "remote-ok"; }];"#,
    );

    let output = sparsh()
        .env("HOME", home.path())
        .args(["--remote-command", "printenv SPARSH_REMOTE_VALUE"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"remote-ok\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn rc_can_alias_ls_to_external_eza_without_touching_ansi_or_icons() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let eza = bin.path().join("eza");
    std::fs::write(&eza, "#!/bin/sh\nprintf '\\033[32m📁 demo\\033[0m\\n'\n").unwrap();
    std::fs::set_permissions(&eza, std::fs::Permissions::from_mode(0o755)).unwrap();
    write_config_rc(
        home.path(),
        r#"    aliases: List<TestAlias> = [{ name: "ls"; command: ["eza", "--icons"]; }];"#,
    );
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited_path}", bin.path().display());

    let output = sparsh()
        .env("HOME", home.path())
        .env("PATH", path)
        .args(["-c", "ls"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, "\u{1b}[32m📁 demo\u{1b}[0m\n".as_bytes());
}

#[test]
fn rc_shell_function_with_named_arguments_can_feed_external_pipeline() {
    let home = tempfile::tempdir().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("server.log");
    std::fs::write(&file, "info ready\nerror exploded\n").unwrap();
    write_rc(
        home.path(),
        r#"function readLog(file: str) -> shell {
    return shell { cat "${file}"; };
};"#,
    );
    let command = format!("readLog(file: \"{}\") | grep error", file.display());

    let output = sparsh()
        .env("HOME", home.path())
        .args(["-c", command.as_str()])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"error exploded\n");
}

#[test]
fn executable_spar_without_shebang_runs_through_sparsh_not_bin_sh() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("main.spar");
    std::fs::write(
        &script,
        r#"function main() -> int { println(message: "from-spar"); return 0; };"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = sparsh()
        .current_dir(directory.path())
        .args(["-c", "./main.spar"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"from-spar\n");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("command not found"));
}

#[test]
fn dash_c_expands_home_before_external_pipeline_execution() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("needle-file"), "x").unwrap();

    let output = sparsh()
        .env("HOME", home.path())
        .args(["-c", "ls ~ | grep needle-file"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"needle-file\n");
}

#[test]
fn dash_c_environment_shorthand_uses_sparsh_session_environment() {
    let output = sparsh()
        .env("SPARSH_DIRECT_ENV", "session-env")
        .args(["-c", "printf %s $SPARSH_DIRECT_ENV"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"session-env");
}

#[test]
fn dash_c_command_substitution_captures_stdout() {
    let output = sparsh()
        .args(["-c", "printf '<%s>' $(printf hello)"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"<hello>");
}

#[test]
fn piped_stdin_accepts_semicolonless_spar_declarations_and_named_calls() {
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
        .write_all(
            b"var name: str = \"OCC\"\nfunction greet(name: str) -> str { return name; }\ngreet(name: name)\n",
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"OCC\n");
}

#[test]
fn practical_native_builtins_are_available_without_bash() {
    let echo = sparsh().args(["-c", "echo hello world"]).output().unwrap();
    assert!(
        echo.status.success(),
        "{}",
        String::from_utf8_lossy(&echo.stderr)
    );
    assert_eq!(echo.stdout, b"hello world\n");

    let printf = sparsh()
        .args(["-c", "printf '%s:%d' value 7"])
        .output()
        .unwrap();
    assert!(
        printf.status.success(),
        "{}",
        String::from_utf8_lossy(&printf.stderr)
    );
    assert_eq!(printf.stdout, b"value:7");

    let help = sparsh().args(["-c", "help echo"]).output().unwrap();
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    let help_text = String::from_utf8(help.stdout).unwrap();
    assert!(help_text.contains("echo"), "{help_text}");
    assert!(help_text.contains("usage:"), "{help_text}");

    let history = sparsh().args(["-c", "history"]).output().unwrap();
    assert!(
        history.status.success(),
        "{}",
        String::from_utf8_lossy(&history.stderr)
    );

    let umask = sparsh().args(["-c", "umask"]).output().unwrap();
    assert!(
        umask.status.success(),
        "{}",
        String::from_utf8_lossy(&umask.stderr)
    );
    let umask_text = String::from_utf8(umask.stdout).unwrap();
    assert_eq!(umask_text.trim().len(), 4, "{umask_text:?}");
    assert!(
        umask_text
            .trim()
            .chars()
            .all(|ch| ('0'..='7').contains(&ch)),
        "{umask_text:?}"
    );

    #[cfg(unix)]
    {
        let ulimit = sparsh().args(["-c", "ulimit -n"]).output().unwrap();
        assert!(
            ulimit.status.success(),
            "{}",
            String::from_utf8_lossy(&ulimit.stderr)
        );
        let value = String::from_utf8(ulimit.stdout).unwrap();
        assert!(
            value.trim() == "unlimited" || value.trim().parse::<u64>().is_ok(),
            "{value:?}"
        );
    }
}

#[test]
fn piped_session_source_spar_persists_function_for_later_named_call() {
    let directory = tempfile::tempdir().unwrap();
    let sourced = directory.path().join("functions.spar");
    std::fs::write(
        &sourced,
        r#"function greet(prefix: str, name: str) -> str { return "${prefix}, ${name}"; };"#,
    )
    .unwrap();

    let mut child = sparsh()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(
        "source \"{}\"\ngreet(prefix: \"Hello\", name: \"OCC\")\n",
        sourced.display()
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Hello, OCC\n");
}

#[test]
fn piped_session_foreign_source_imports_venv_style_environment_and_cwd() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("active");
    std::fs::create_dir(&target).unwrap();
    let activate = directory.path().join("activate");
    std::fs::write(
        &activate,
        format!(
            "export VIRTUAL_ENV='{}'\nexport PATH=\"$VIRTUAL_ENV/bin:$PATH\"\ncd '{}'\n",
            directory.path().display(),
            target.display()
        ),
    )
    .unwrap();

    let mut child = sparsh()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(
        "source --shell sh \"{}\"\nprintf '%s\\n' $VIRTUAL_ENV\npwd\n",
        activate.display()
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n{}\n", directory.path().display(), target.display())
    );
}

#[test]
fn executable_spar_program_can_feed_an_external_pipeline_from_cli() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("main.spar");
    std::fs::write(
        &script,
        r#"function main() -> int { println(message: "info ready"); println(message: "error found"); return 0; };"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = sparsh()
        .current_dir(directory.path())
        .args(["-c", "./main.spar | grep error"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"error found\n");
}

#[test]
fn chmod_is_resolved_as_an_external_utility() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("script");
    std::fs::write(&file, "payload").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let output = sparsh()
        .current_dir(directory.path())
        .args(["-c", "chmod +x script"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_ne!(
        std::fs::metadata(file).unwrap().permissions().mode() & 0o111,
        0
    );
}

#[test]
fn piped_structured_results_are_plain_data_not_decorated_tables() {
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
        .write_all(
            b"import pkg { collectTable, take } from \"std/data\";\nstruct User { name: str = \"\"; age: int = 0; };\nvar people: [User] = [User(name: \"Obi\", age: 24), User(name: \"Ada\", age: 31)];\npeople |> collectTable() |> take(1)\n_ |> take(2)\n",
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "{\"name\":\"Obi\",\"age\":24}\n{\"name\":\"Obi\",\"age\":24}\n"
    );
    assert!(!stdout.contains('│') && !stdout.contains("rows"));
}

#[test]
fn command_mode_terminal_encoder_is_plain_compact_bytes() {
    let output = sparsh()
        .args([
            "-c",
            "printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> to json",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"[{\"name\":\"Obi\",\"age\":24},{\"name\":\"Ada\",\"age\":31}]\n"
    );
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn redirected_terminal_encoder_writes_compact_plain_json_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let output = sparsh()
        .current_dir(temp.path())
        .args([
            "-c",
            "printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> to json > people.json",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read(temp.path().join("people.json")).unwrap(),
        b"[{\"name\":\"Obi\",\"age\":24},{\"name\":\"Ada\",\"age\":31}]\n"
    );
}

#[test]
fn mixed_byte_and_value_pipeline_can_be_typed_directly() {
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
        .write_all(
            b"import pkg { where } from \"std/data\";\nprintf '%s\\n' '{\"n\":\"web\",\"s\":\"up\"}' '{\"n\":\"db\",\"s\":\"down\"}' | from jsonl |> where(fn(c) => c.s == \"up\") |> to jsonl\n",
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"{\"n\":\"web\",\"s\":\"up\"}\n");
}

#[test]
fn spar_script_output_appears_while_the_script_is_still_running() {
    use std::io::{BufRead, BufReader};
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("slow.spar");
    std::fs::write(
        &script,
        r#"import pkg { run } from "std/process";
function main() -> int {
    println(message: "first");
    run(program: "sleep", args: ["3"]);
    println(message: "second");
    return 0;
};"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = sparsh()
        .current_dir(directory.path())
        .args(["-c", "./slow.spar"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    lines.read_line(&mut first).unwrap();
    assert_eq!(first, "first\n");
    // The script is still sleeping: the first line arrived before it finished.
    assert!(child.try_wait().unwrap().is_none());
    let mut rest = String::new();
    lines.read_line(&mut rest).unwrap();
    assert_eq!(rest, "second\n");
    assert!(child.wait().unwrap().success());
}

#[test]
fn errors_underline_the_typed_command_not_the_hidden_session_source() {
    for (input, line, underlined) in [
        (
            "printf 'a\\n1\\n' | from csv |> bogus(fn(r) => r.a > 0)",
            "printf 'a\\n1\\n' | from csv |> bogus(fn(r) => r.a > 0)",
            "^^^^^",
        ),
        ("var x: int = nowhere;", "var x: int = nowhere;", "^^^^^^^"),
    ] {
        let output = sparsh().args(["-c", input]).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success(), "{input}");
        assert!(stderr.contains("--> <sparsh>:1:"), "{stderr}");
        assert!(stderr.contains(line), "{stderr}");
        assert!(stderr.contains(underlined), "{stderr}");
        // The old output pointed at a line number inside the session source.
        assert!(!stderr.contains("119"), "{stderr}");
    }
}

#[test]
fn a_missing_data_import_in_a_script_says_what_to_import() {
    let output = sparsh()
        .args([
            "-c",
            "printf 'a\\n1\\n' | from csv |> where(fn(r) => r.a > 0)",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("import pkg { where } from \"std/data\";"),
        "{stderr}"
    );
}
