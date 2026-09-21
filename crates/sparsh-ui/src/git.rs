use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::prompt::GitState;

struct CachedGitState {
    cwd: PathBuf,
    checked_at: Instant,
    state: Option<GitState>,
}

pub struct GitProbe {
    executable: OsString,
    timeout: Duration,
    ttl: Duration,
    cache: Option<CachedGitState>,
}

impl GitProbe {
    pub fn new() -> Self {
        Self::with_executable(
            OsString::from("git"),
            Duration::from_millis(100),
            Duration::from_secs(1),
        )
    }

    pub fn with_executable(executable: OsString, timeout: Duration, ttl: Duration) -> Self {
        Self {
            executable,
            timeout,
            ttl,
            cache: None,
        }
    }

    pub fn state(&mut self, cwd: &Path) -> Option<GitState> {
        if let Some(cache) = &self.cache {
            if cache.cwd == cwd && cache.checked_at.elapsed() < self.ttl {
                return cache.state.clone();
            }
        }
        let state =
            run_git(&self.executable, cwd, self.timeout).and_then(|output| parse_status(&output));
        self.cache = Some(CachedGitState {
            cwd: cwd.to_path_buf(),
            checked_at: Instant::now(),
            state: state.clone(),
        });
        state
    }
}

impl Default for GitProbe {
    fn default() -> Self {
        Self::new()
    }
}

fn run_git(executable: &OsStr, cwd: &Path, timeout: Duration) -> Option<String> {
    let mut child = Command::new(executable)
        .args([
            "-c",
            "core.fileMode=false",
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Drain stdout concurrently so a repository with a very large status
    // cannot fill the OS pipe and deadlock the child while we wait for it.
    let reader = thread::spawn(move || {
        let mut output = String::new();
        stdout.read_to_string(&mut output).map(|_| output)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return None;
        }
        thread::sleep(Duration::from_millis(2));
    };
    let output = reader.join().ok()?.ok()?;
    if !status.success() {
        return None;
    }
    Some(output)
}

pub(crate) fn parse_status(output: &str) -> Option<GitState> {
    let mut state = GitState::default();
    let mut have_branch = false;
    for line in output.lines() {
        if let Some(branch) = line.strip_prefix("# branch.head ") {
            if branch != "(detached)" {
                state.branch = branch.to_string();
                have_branch = true;
            }
            continue;
        }
        if let Some(ab) = line.strip_prefix("# branch.ab ") {
            for item in ab.split_whitespace() {
                if let Some(value) = item.strip_prefix('+') {
                    state.ahead = value.parse().unwrap_or(0);
                }
                if let Some(value) = item.strip_prefix('-') {
                    state.behind = value.parse().unwrap_or(0);
                }
            }
            continue;
        }
        if line.starts_with("? ") {
            state.untracked += 1;
            continue;
        }
        if line.starts_with("u ") {
            state.conflicts += 1;
            continue;
        }
        if line.starts_with("1 ") || line.starts_with("2 ") {
            let xy = line.split_whitespace().nth(1).unwrap_or("..").as_bytes();
            if xy.first().is_some_and(|value| *value != b'.') {
                state.staged += 1;
            }
            if xy.get(1).is_some_and(|value| *value != b'.') {
                state.modified += 1;
            }
        }
    }
    have_branch.then_some(state)
}

#[cfg(test)]
mod tests {
    use super::parse_status;

    #[cfg(unix)]
    #[test]
    fn large_git_status_is_drained_without_pipe_deadlock() {
        use std::ffi::OsString;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let fake_git = directory.path().join("git");
        std::fs::write(
            &fake_git,
            "#!/bin/sh\nprintf '# branch.head main\n'\ni=0\nwhile [ \"$i\" -lt 12000 ]; do printf '? file%s\n' \"$i\"; i=$((i+1)); done\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut probe = super::GitProbe::with_executable(
            OsString::from(fake_git),
            Duration::from_secs(5),
            Duration::ZERO,
        );
        let state = probe
            .state(directory.path())
            .expect("large status should complete");

        assert_eq!(state.branch, "main");
        assert_eq!(state.untracked, 12_000);
    }

    #[test]
    fn parses_porcelain_v2_status_counters() {
        let input = "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -1\n1 M. N... 100644 100644 100644 a a staged\n1 .M N... 100644 100644 100644 a a modified\n? notes.txt\nu UU N... 100644 100644 100644 100644 a a a conflict\n";
        let state = parse_status(input).unwrap();
        assert_eq!(state.branch, "main");
        assert_eq!(
            (
                state.staged,
                state.modified,
                state.untracked,
                state.conflicts,
                state.ahead,
                state.behind
            ),
            (1, 1, 1, 1, 2, 1)
        );
    }
}
