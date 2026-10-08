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
            r#"struct TestAlias {{ name: str; command: List<str>; }};
struct TestEnvironment {{ name: str; value: str; }};
struct TestCompletion {{ enabled: bool = false; }};
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
        r#"    aliases: List<TestAlias> = [TestAlias(name: "configured", command: ["printf", "alias-ok"])];
    environment: List<TestEnvironment> = [TestEnvironment(name: "SPARSH_CONFIG_VALUE", value: "env-ok")];"#,
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
        r#"    environment: List<TestEnvironment> = [TestEnvironment(name: "SPARSH_REMOTE_VALUE", value: "remote-ok")];"#,
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
        r#"    aliases: List<TestAlias> = [TestAlias(name: "ls", command: ["eza", "--icons"])];"#,
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
        r#"function readLog(file: str) -> __shell {
    return __shell { cat "${file}"; };
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
        r#"function main() -> int { println(value: "from-spar"); return 0; };"#,
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
        r#"function main() -> int { println(value: "info ready"); println(value: "error found"); return 0; };"#,
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
            b"import pkg { collectTable, take } from \"std/data\";\nstruct User { name: str = \"\"; age: int = 0; };\nvar people: [User] = [User(name: \"Obi\", age: 24), User(name: \"Ada\", age: 31)];\npeople |> collectTable() |> take(count: 1)\n_ |> take(count: 2)\n",
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
            b"import pkg { where } from \"std/data\";\nprintf '%s\\n' '{\"n\":\"web\",\"s\":\"up\"}' '{\"n\":\"db\",\"s\":\"down\"}' | from jsonl |> where(predicate: fn(value) => value.s == \"up\") |> to jsonl\n",
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
    println(value: "first");
    run(program: "sleep", args: ["3"]);
    println(value: "second");
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
            "printf 'a\\n1\\n' | from csv |> where(predicate: fn(value) => value.a > 0)",
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

#[test]
fn piped_stdin_accepts_canonical_fn_and_const() {
    let mut child = sparsh()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(
        b"const DIVISOR: int = 3;\nfn remainder(value: int) -> int { return value % DIVISOR; };\nremainder(value: 8)\n"
    ).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"2\n");
}

#[test]
fn piped_stdin_runs_while_and_loop_in_spar_session() {
    let mut child = sparsh()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(
        b"var mut total: int = 0;\nwhile total < 3 { total = total + 1; }\nloop { total = total + 2; break; }\ntotal\n"
    ).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"5\n");
}

#[test]
fn piped_stdin_builds_a_program_with_struct_impl_and_method_calls() {
    let mut child = sparsh()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(
        b"export struct Counter { value: int = 2; };\nimpl Counter { fn inc(self) -> int { return self.value + 1; }; };\nvar counter: Counter = Counter();\ncounter.inc()\n"
    ).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"3\n");
}

fn run_c(script: &str) -> String {
    let output = sparsh().args(["-c", script]).output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_c_stderr(script: &str) -> String {
    let output = sparsh().args(["-c", script]).output().unwrap();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn mixed_line_runs_commands_and_spar_statements() {
    let out = run_c("echo hi; var a: int = 2; a + 1");
    assert!(out.contains("hi") && out.trim_end().ends_with('3'), "{out}");
}

#[test]
fn shell_command_inside_an_if_block_runs() {
    assert!(run_c("if true { echo yes }").contains("yes"));
}

#[test]
fn shell_command_inside_a_for_block_runs() {
    let out = run_c("for i in [1, 2] { echo n${i} }");
    assert!(out.contains("n1") && out.contains("n2"), "{out}");
}

#[test]
fn a_failing_command_does_not_stop_later_statements() {
    assert!(run_c("ls /definitely/not/here; echo after").contains("after"));
}

#[test]
fn a_failing_spar_statement_does_not_stop_later_statements() {
    let out = run_c("var a: int = 1; a = 2; echo after");
    assert!(out.contains("after"), "{out}");
}

#[test]
fn variable_shadows_command_and_tilde_reaches_it() {
    let out = run_c("var ls: List<int> = [1, 2]; ls; ~ ls /");
    assert!(out.contains("1") && out.contains("bin"), "{out}");
}

#[test]
fn existing_behaviors_are_unchanged() {
    assert!(run_c("var n: int = 3; n").contains('3'));
    assert!(run_c("echo a | cat").contains('a'));
    let piped = run_c(
        "import pkg { where } from \"std/data\"; printf '%s\\n' '{\"n\":\"web\",\"s\":\"up\"}' '{\"n\":\"db\",\"s\":\"down\"}' | from jsonl |> where(predicate: fn(value) => value.s == \"up\") |> to jsonl",
    );
    assert!(piped.contains("web") && !piped.contains("db"), "{piped}");
}

#[test]
fn spar_error_in_rewritten_block_points_at_typed_text() {
    let err = run_c_stderr("if true { echo ok; nope(1) }");
    // The caret line must sit under what the user typed, not under an
    // inserted `~ ` marker or `;` terminator.
    let lines: Vec<&str> = err.lines().collect();
    let code = lines
        .iter()
        .position(|l| l.contains("if true {"))
        .expect(&err);
    assert!(
        !lines[code].contains("~ "),
        "inserted marker leaked into the rendered source: {err}"
    );
}

fn run_piped_in(directory: &Path, input: &str) -> std::process::Output {
    let mut child = sparsh()
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn log_lines(directory: &Path) -> Vec<String> {
    std::fs::read_to_string(directory.join("log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn an_if_block_typed_at_the_prompt_runs_once() {
    let directory = tempfile::tempdir().unwrap();
    let output = run_piped_in(
        directory.path(),
        "if true { echo x >> log }\necho done\necho done2\n1 + 1\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(log_lines(directory.path()), vec!["x"], "{stdout}");
}

#[test]
fn a_block_and_a_command_on_one_line_run_the_block_once() {
    let directory = tempfile::tempdir().unwrap();
    let output = sparsh()
        .current_dir(directory.path())
        .args(["-c", "if true { echo x >> log }; echo done"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("done"));
    assert_eq!(log_lines(directory.path()), vec!["x"]);
}

#[test]
fn for_and_while_blocks_typed_at_the_prompt_run_once() {
    let directory = tempfile::tempdir().unwrap();
    let output = run_piped_in(
        directory.path(),
        "for k in [1, 2] { echo f${k} >> log }\necho a\nvar mut i: int = 0\nwhile i < 1 { echo w >> log; i = i + 1 }\necho b\ni\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(log_lines(directory.path()), vec!["f1", "f2", "w"], "{stdout}");
    assert!(stdout.trim_end().ends_with('1'), "{stdout}");
}

#[test]
fn block_mutations_persist_without_replaying_the_block() {
    let directory = tempfile::tempdir().unwrap();
    let output = run_piped_in(
        directory.path(),
        "var mut t: int = 0\nfor k in [1, 2] { t = t + k; echo t >> log }\nt\nt = t + 10\nt\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(log_lines(directory.path()), vec!["t", "t"], "{stdout}");
    assert_eq!(stdout, "3\n13\n", "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn a_declaration_initializer_with_a_side_effect_runs_once() {
    let directory = tempfile::tempdir().unwrap();
    let output = run_piped_in(
        directory.path(),
        "fn f() -> int { print(value: \"side\"); return 1; }\nvar x: int = f()\necho a\nvar y: int = 2\nx + y\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.matches("side").count(), 1, "{stdout}");
    assert!(stdout.trim_end().ends_with('3'), "{stdout}");
}

#[test]
fn struct_string_and_map_mutations_persist_across_submissions() {
    let directory = tempfile::tempdir().unwrap();
    let output = run_piped_in(
        directory.path(),
        "struct P { x: int = 1; y: int = 2; };\nvar mut p: P = P();\np.x = 9\necho a\np.x\nfor k in [1, 2] { p.x = p.x + k }\necho b\np.x\nvar mut s: str = \"a\"\ns = s + \"!\"\necho c\ns\nvar mut m: Map<str, int> = { a: 1; };\nm.insert(key: \"b\", value: 2)\necho d\nm.length()\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("9\n"), "{stdout}");
    assert!(stdout.contains("12\n"), "{stdout}");
    assert!(stdout.contains("a!"), "{stdout}");
    assert!(stdout.trim_end().ends_with('2'), "{stdout}");
}

#[test]
fn command_output_before_a_block_is_written_before_the_block_output() {
    let home = tempfile::tempdir().unwrap();
    let output = sparsh()
        .env("HOME", home.path())
        .args(["-c", "echo pre; if true { echo once }"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "pre\nonce\n");
}

#[test]
fn a_map_literal_in_a_for_header_does_not_panic() {
    let home = tempfile::tempdir().unwrap();
    let output = sparsh()
        .env("HOME", home.path())
        .args(["-c", "for k in {\"a\": 1} { print(value: \"x\") }"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(output.status.code(), Some(101), "{stderr}");
    assert!(!stderr.contains("panicked") && !stderr.contains("unreachable"), "{stderr}");
}

#[test]
fn a_set_literal_in_a_for_header_does_not_leak_the_marker() {
    let home = tempfile::tempdir().unwrap();
    let output = sparsh()
        .env("HOME", home.path())
        .args(["-c", "for k in { a: 1; } { print(value: \"x\") }"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("found \u{27}~\u{27}"), "{stderr}");
}

fn run_c_in_temp_home(script: &str) -> (String, String) {
    let home = tempfile::tempdir().unwrap();
    let output = sparsh()
        .env("HOME", home.path())
        .args(["-c", script])
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn removed_shell_block_forms_show_the_migration_message() {
    for script in ["shell { echo hi }", "exec shell { echo hi }"] {
        let (_, stderr) = run_c_in_temp_home(script);
        assert!(stderr.contains("ShellResult"), "{script}: {stderr}");
        assert!(!stderr.contains("bare brace"), "{script}: {stderr}");
    }
}

#[test]
fn an_open_quote_in_a_command_says_unterminated_quote() {
    let (_, stderr) = run_c_in_temp_home("echo it\u{27}s");
    assert!(stderr.contains("unterminated quote"), "{stderr}");
    assert!(!stderr.contains("shell block"), "{stderr}");
}

#[test]
fn a_struct_with_an_empty_option_field_keeps_later_submissions_working() {
    let directory = tempfile::tempdir().unwrap();
    let output = run_piped_in(
        directory.path(),
        "struct S { o: Option<int> = none<int>(); c: int = 1; };\nvar mut s: S = S();\necho a\ns.c = 5\necho b\ns.c\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("a\n") && stdout.contains("b\n"), "{stdout}");
    assert!(stdout.trim_end().ends_with('5'), "{stdout}");
}

#[test]
fn editor_snapshot_completes_import_names_relative_to_the_session_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("lib.spar"),
        "export var port: int = 80;\nvar hidden: int = 1;\n",
    )
    .unwrap();
    let mut session = sparsh_core::ShellSession::new();
    session.submit(&format!("cd {}", dir.path().display())).unwrap();
    let snapshot = session.completion_snapshot();
    let line = "import { } from \"lib.spar\";";
    let items = sparsh_core::complete(
        &snapshot,
        sparsh_core::CompletionRequest {
            line,
            cursor: "import { ".len(),
        },
    );
    let names: Vec<_> = items.iter().map(|item| item.replacement.as_str()).collect();
    assert_eq!(names, vec!["port"]);
}

#[test]
fn editor_snapshot_completes_members_of_session_variables() {
    let mut session = sparsh_core::ShellSession::new();
    session
        .submit("struct P { name: str = \"\"; port: int = 0; };")
        .unwrap();
    session.submit("var p: P = P();").unwrap();
    let snapshot = session.completion_snapshot();
    let complete = |line: &str| -> Vec<String> {
        sparsh_core::complete(
            &snapshot,
            sparsh_core::CompletionRequest { line, cursor: line.len() },
        )
        .into_iter()
        .map(|item| item.replacement)
        .collect()
    };
    let all = complete("p.");
    assert!(all.contains(&"name".to_string()) && all.contains(&"port".to_string()), "{all:?}");
    assert_eq!(complete("p.na"), vec!["name"]);
    assert!(!complete("echo p.").contains(&"name".to_string()));
}

#[test]
fn editor_snapshot_completes_scope_names_in_spar_positions_only() {
    let mut session = sparsh_core::ShellSession::new();
    session.submit("var count: int = 1;").unwrap();
    let snapshot = session.completion_snapshot();
    let complete = |line: &str| -> Vec<String> {
        sparsh_core::complete(
            &snapshot,
            sparsh_core::CompletionRequest { line, cursor: line.len() },
        )
        .into_iter()
        .map(|item| item.replacement)
        .collect()
    };
    assert!(complete("var x = co").contains(&"count".to_string()));
    assert!(complete("echo ${co").contains(&"count".to_string()));
    assert!(complete("for i in [1, 2] { echo ${i").contains(&"i".to_string()));
    assert!(!complete("co").contains(&"count".to_string()));
}

#[test]
fn editor_snapshot_shows_the_signature_of_session_functions() {
    let mut session = sparsh_core::ShellSession::new();
    session
        .submit("fn build(profile: str, release: bool = false) -> int { return 0; };")
        .unwrap();
    let snapshot = session.completion_snapshot();
    let line = "build(profile: ";
    assert_eq!(
        sparsh_core::signature_hint(&snapshot, line, line.len()).as_deref(),
        Some("build(profile: str, release: bool = false)")
    );
    let hint = sparsh_core::signature_hint_info(&snapshot, "build(profile: \"x\", ", 19).unwrap();
    assert_eq!(hint.active, 1);
    assert_eq!(sparsh_core::signature_hint(&snapshot, "build", 5), None);
    assert_eq!(sparsh_core::signature_hint(&snapshot, "echo build(", 11), None);
}
