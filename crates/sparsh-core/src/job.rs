use std::collections::{BTreeMap, VecDeque};

use spar_process::{ProcessGroupId, SpawnedJob, WaitState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Stopped,
    Done(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellJob {
    pub id: JobId,
    pub pgid: ProcessGroupId,
    pub command_text: String,
    pub state: JobState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProcessRecordState {
    Running,
    Stopped(i32),
    Exited(i32),
    Signaled(i32),
}

impl ProcessRecordState {
    fn terminal_code(&self) -> Option<i32> {
        match self {
            Self::Exited(code) => Some(*code),
            Self::Signaled(signal) => Some(128 + *signal),
            Self::Running | Self::Stopped(_) => None,
        }
    }

    fn is_terminal(&self) -> bool {
        self.terminal_code().is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessRecord {
    pid: u32,
    state: ProcessRecordState,
}

#[derive(Clone, Debug)]
struct JobRecord {
    id: JobId,
    pgid: ProcessGroupId,
    command_text: String,
    processes: Vec<ProcessRecord>,
    state: JobState,
}

impl JobRecord {
    fn snapshot(&self) -> ShellJob {
        ShellJob {
            id: self.id,
            pgid: self.pgid,
            command_text: self.command_text.clone(),
            state: self.state.clone(),
        }
    }

    fn recompute_state(&mut self) -> JobState {
        let state = if self
            .processes
            .iter()
            .all(|process| process.state.is_terminal())
        {
            let code = self
                .processes
                .iter()
                .rev()
                .filter_map(|process| process.state.terminal_code())
                .find(|code| *code != 0)
                .unwrap_or(0);
            JobState::Done(code)
        } else if self
            .processes
            .iter()
            .filter(|process| !process.state.is_terminal())
            .all(|process| matches!(&process.state, ProcessRecordState::Stopped(_)))
        {
            JobState::Stopped
        } else {
            JobState::Running
        };
        self.state = state.clone();
        state
    }
}

#[derive(Default)]
pub(crate) struct JobTable {
    next_id: u64,
    jobs: BTreeMap<JobId, JobRecord>,
    order: Vec<JobId>,
    notifications: VecDeque<String>,
}

impl JobTable {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            ..Self::default()
        }
    }

    pub(crate) fn insert(&mut self, spawned: SpawnedJob, command_text: String) -> JobId {
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let id = JobId(self.next_id);
        self.next_id += 1;
        let processes = spawned
            .processes
            .iter()
            .map(|process| ProcessRecord {
                pid: process.pid,
                state: ProcessRecordState::Running,
            })
            .collect();
        self.jobs.insert(
            id,
            JobRecord {
                id,
                pgid: spawned.pgid,
                command_text,
                processes,
                state: JobState::Running,
            },
        );
        self.order.push(id);
        id
    }

    pub(crate) fn snapshots(&self) -> Vec<ShellJob> {
        self.order
            .iter()
            .filter_map(|id| self.jobs.get(id).map(JobRecord::snapshot))
            .collect()
    }

    pub(crate) fn get(&self, id: JobId) -> Option<ShellJob> {
        self.jobs.get(&id).map(JobRecord::snapshot)
    }

    pub(crate) fn current_id(&self) -> Option<JobId> {
        self.order.iter().rev().copied().find(|id| {
            self.jobs
                .get(id)
                .is_some_and(|job| !matches!(&job.state, JobState::Done(_)))
        })
    }

    pub(crate) fn resolve_id(&self, requested: Option<JobId>) -> Option<JobId> {
        requested.or_else(|| self.current_id())
    }

    pub(crate) fn pgid(&self, id: JobId) -> Option<ProcessGroupId> {
        self.jobs.get(&id).map(|job| job.pgid)
    }

    pub(crate) fn all_owned_pids(&self) -> Vec<u32> {
        self.jobs
            .values()
            .flat_map(|job| job.processes.iter())
            .filter(|process| !process.state.is_terminal())
            .map(|process| process.pid)
            .collect()
    }

    pub(crate) fn apply_wait_state(&mut self, state: WaitState) -> Option<(JobId, JobState)> {
        let pid = match state {
            WaitState::Continued { pid }
            | WaitState::Stopped { pid, .. }
            | WaitState::Exited { pid, .. }
            | WaitState::Signaled { pid, .. } => pid,
        };
        let id = self.order.iter().copied().find(|id| {
            self.jobs
                .get(id)
                .is_some_and(|job| job.processes.iter().any(|process| process.pid == pid))
        })?;

        let (previous, current, command_text) = {
            let job = self.jobs.get_mut(&id)?;
            let previous = job.state.clone();
            let process = job
                .processes
                .iter_mut()
                .find(|process| process.pid == pid)?;
            process.state = match state {
                WaitState::Continued { .. } => ProcessRecordState::Running,
                WaitState::Stopped { signal, .. } => ProcessRecordState::Stopped(signal),
                WaitState::Exited { code, .. } => ProcessRecordState::Exited(code),
                WaitState::Signaled { signal, .. } => ProcessRecordState::Signaled(signal),
            };
            let current = job.recompute_state();
            (previous, current, job.command_text.clone())
        };

        if current != previous {
            match &current {
                JobState::Stopped => self
                    .notifications
                    .push_back(format!("[{}] Stopped  {}", id.0, command_text)),
                JobState::Done(code) => self
                    .notifications
                    .push_back(format!("[{}] Done({code})  {}", id.0, command_text)),
                JobState::Running if matches!(&previous, JobState::Stopped) => self
                    .notifications
                    .push_back(format!("[{}] Running  {}", id.0, command_text)),
                JobState::Running => {}
            }
        }
        Some((id, current))
    }

    pub(crate) fn live_pids(&self, id: JobId) -> Option<Vec<u32>> {
        self.jobs.get(&id).map(|job| {
            job.processes
                .iter()
                .filter(|process| !process.state.is_terminal())
                .map(|process| process.pid)
                .collect()
        })
    }

    pub(crate) fn ids(&self) -> Vec<JobId> {
        self.order.clone()
    }

    pub(crate) fn poll(&mut self) -> std::io::Result<()> {
        let pids = self.all_owned_pids();
        for pid in pids {
            loop {
                match spar_process::wait_pid(pid, true) {
                    Ok(Some(state)) => {
                        let terminal =
                            matches!(state, WaitState::Exited { .. } | WaitState::Signaled { .. });
                        self.apply_wait_state(state);
                        if terminal {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    pub(crate) fn wait_until_stable(&mut self, id: JobId) -> std::io::Result<Option<JobState>> {
        loop {
            let Some(snapshot) = self.get(id) else {
                return Ok(None);
            };
            if matches!(&snapshot.state, JobState::Done(_) | JobState::Stopped) {
                return Ok(Some(snapshot.state));
            }
            let Some(pids) = self.live_pids(id) else {
                return Ok(None);
            };
            if pids.is_empty() {
                return Ok(self.get(id).map(|job| job.state));
            }
            for pid in pids {
                if let Some(state) = spar_process::wait_pid(pid, false)? {
                    self.apply_wait_state(state);
                }
                if self
                    .get(id)
                    .is_some_and(|job| matches!(&job.state, JobState::Done(_) | JobState::Stopped))
                {
                    break;
                }
            }
        }
    }

    pub(crate) fn foreground(
        &mut self,
        id: JobId,
        use_terminal: bool,
    ) -> std::io::Result<Option<JobState>> {
        let Some(snapshot) = self.get(id) else {
            return Ok(None);
        };
        if matches!(&snapshot.state, JobState::Done(_)) {
            return Ok(Some(snapshot.state));
        }
        if matches!(&snapshot.state, JobState::Stopped) {
            self.resume(id)?;
        }

        let lease = if use_terminal && spar_process::stdin_is_tty() {
            let pgid = self.pgid(id).ok_or_else(|| {
                std::io::Error::other("job disappeared before foreground handoff")
            })?;
            Some(JobTerminalLease::acquire(pgid)?)
        } else {
            None
        };
        let waited = self.wait_until_stable(id);
        let restored = match lease {
            Some(lease) => lease.finish(),
            None => Ok(()),
        };
        let state = waited?;
        restored?;
        Ok(state)
    }

    pub(crate) fn resume(&mut self, id: JobId) -> std::io::Result<Option<()>> {
        let Some(pgid) = self.pgid(id) else {
            return Ok(None);
        };
        spar_process::continue_process_group(pgid)?;
        self.mark_running(id);
        Ok(Some(()))
    }

    pub(crate) fn mark_running(&mut self, id: JobId) -> Option<()> {
        let job = self.jobs.get_mut(&id)?;
        for process in &mut job.processes {
            if matches!(&process.state, ProcessRecordState::Stopped(_)) {
                process.state = ProcessRecordState::Running;
            }
        }
        job.state = JobState::Running;
        Some(())
    }

    pub(crate) fn remove(&mut self, id: JobId) -> Option<ShellJob> {
        let record = self.jobs.remove(&id)?;
        self.order.retain(|candidate| *candidate != id);
        Some(record.snapshot())
    }

    pub(crate) fn take_notifications(&mut self) -> Vec<String> {
        self.notifications.drain(..).collect()
    }
}

struct JobTerminalLease {
    shell_pgid: spar_process::ProcessGroupId,
    terminal_state: spar_process::TerminalState,
    active: bool,
}

impl JobTerminalLease {
    fn acquire(job_pgid: spar_process::ProcessGroupId) -> std::io::Result<Self> {
        let shell_pgid = spar_process::current_process_group_id()?;
        let terminal_state = spar_process::capture_terminal_state(0)?;
        spar_process::set_terminal_foreground_pgid(0, job_pgid)?;
        Ok(Self {
            shell_pgid,
            terminal_state,
            active: true,
        })
    }

    fn finish(mut self) -> std::io::Result<()> {
        let ownership = spar_process::set_terminal_foreground_pgid(0, self.shell_pgid);
        let terminal = spar_process::restore_terminal_state(0, &self.terminal_state);
        self.active = false;
        ownership?;
        terminal
    }
}

impl Drop for JobTerminalLease {
    fn drop(&mut self) {
        if self.active {
            let _ = spar_process::set_terminal_foreground_pgid(0, self.shell_pgid);
            let _ = spar_process::restore_terminal_state(0, &self.terminal_state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawned(pgid: i32, pids: &[u32]) -> SpawnedJob {
        SpawnedJob {
            pgid: ProcessGroupId(pgid),
            processes: pids
                .iter()
                .map(|pid| spar_process::SpawnedProcess { pid: *pid })
                .collect(),
        }
    }

    #[test]
    fn ids_are_monotonic_and_current_prefers_latest_live_job() {
        let mut jobs = JobTable::new();
        let first = jobs.insert(spawned(10, &[10]), "one".into());
        let second = jobs.insert(spawned(20, &[20]), "two".into());
        assert_eq!(first, JobId(1));
        assert_eq!(second, JobId(2));
        assert_eq!(jobs.current_id(), Some(second));
        jobs.apply_wait_state(WaitState::Exited { pid: 20, code: 0 });
        assert_eq!(jobs.current_id(), Some(first));
    }

    #[test]
    fn pipeline_state_uses_rightmost_nonzero_exit() {
        let mut jobs = JobTable::new();
        let id = jobs.insert(spawned(10, &[10, 11, 12]), "pipe".into());
        jobs.apply_wait_state(WaitState::Exited { pid: 10, code: 3 });
        jobs.apply_wait_state(WaitState::Exited { pid: 11, code: 7 });
        jobs.apply_wait_state(WaitState::Exited { pid: 12, code: 0 });
        assert_eq!(jobs.get(id).unwrap().state, JobState::Done(7));
    }

    #[test]
    fn stopped_and_continued_transitions_are_aggregated() {
        let mut jobs = JobTable::new();
        let id = jobs.insert(spawned(10, &[10, 11]), "pipe".into());
        jobs.apply_wait_state(WaitState::Stopped {
            pid: 10,
            signal: 20,
        });
        assert_eq!(jobs.get(id).unwrap().state, JobState::Running);
        jobs.apply_wait_state(WaitState::Stopped {
            pid: 11,
            signal: 20,
        });
        assert_eq!(jobs.get(id).unwrap().state, JobState::Stopped);
        jobs.apply_wait_state(WaitState::Continued { pid: 10 });
        assert_eq!(jobs.get(id).unwrap().state, JobState::Running);
    }

    #[test]
    fn disown_removes_record_without_reusing_id() {
        let mut jobs = JobTable::new();
        let first = jobs.insert(spawned(10, &[10]), "one".into());
        assert!(jobs.remove(first).is_some());
        let second = jobs.insert(spawned(20, &[20]), "two".into());
        assert_eq!(second, JobId(2));
    }
}
