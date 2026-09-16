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
            "--porcelain=v1",
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
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(2));
    };
    if !status.success() {
        return None;
    }
    let mut output = String::new();
    stdout.read_to_string(&mut output).ok()?;
    Some(output)
}

fn parse_status(output: &str) -> Option<GitState> {
    let mut lines = output.lines();
    let header = lines.next()?.strip_prefix("## ")?;
    if header.starts_with("HEAD ") || header == "HEAD" {
        return None;
    }
    let branch = header
        .split_once("...")
        .map_or(header, |(branch, _)| branch)
        .split_whitespace()
        .next()?;
    if branch.is_empty() {
        return None;
    }
    Some(GitState {
        branch: branch.to_string(),
        dirty: lines.any(|line| !line.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use crate::prompt::GitState;

    use super::{parse_status, GitProbe};

    #[test]
    fn parses_clean_dirty_and_detached_porcelain_headers() {
        assert_eq!(
            parse_status("## main\n"),
            Some(GitState {
                branch: "main".into(),
                dirty: false,
            })
        );
        assert_eq!(
            parse_status("## topic\n M src/main.rs\n"),
            Some(GitState {
                branch: "topic".into(),
                dirty: true,
            })
        );
        assert_eq!(parse_status("## HEAD (no branch)\n"), None);
        assert_eq!(parse_status("not porcelain\n"), None);
    }

    #[test]
    fn caches_probe_result_for_same_cwd_within_ttl() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("fake-git");
        let counter = directory.path().join("count");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\ncount=$(cat '{}' 2>/dev/null || printf 0)\nprintf '%s' \"$((count + 1))\" > '{}'\nprintf '## main\\n'\n",
                counter.display(),
                counter.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut probe = GitProbe::with_executable(
            OsString::from(&executable),
            Duration::from_millis(100),
            Duration::from_secs(1),
        );

        assert_eq!(probe.state(directory.path()).unwrap().branch, "main");
        assert_eq!(probe.state(directory.path()).unwrap().branch, "main");
        assert_eq!(std::fs::read_to_string(counter).unwrap(), "1");
    }

    #[test]
    fn git_probe_kills_timed_out_child() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("slow-git");
        std::fs::write(&executable, "#!/bin/sh\nsleep 2\nprintf '## late\\n'\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut probe = GitProbe::with_executable(
            OsString::from(&executable),
            Duration::from_millis(20),
            Duration::ZERO,
        );

        let started = Instant::now();
        let state = probe.state(directory.path());

        assert_eq!(state, None);
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}
