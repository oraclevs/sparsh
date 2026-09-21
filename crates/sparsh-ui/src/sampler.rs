//! System readings for right-prompt widgets (cpu, ram, disk, battery, load,
//! uptime, user, host).
//!
//! Readings are taken when a prompt is built (no background threads). Only the
//! widgets that the configured templates use are sampled. Every reader failure
//! makes its widget empty; nothing here prints or errors. The Linux readers use
//! `/proc`, `/sys` and `statvfs` directly, so there are no extra dependencies;
//! on other platforms those readings are simply absent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use sparsh_core::{NeededWidgets, WidgetKind};

/// How long a `statvfs` may take before the disk widget gives up for this
/// prompt (a stale network mount can block indefinitely).
const DISK_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuTimes {
    pub total: u64,
    /// Idle time including iowait.
    pub idle: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemUsage {
    pub total: u64,
    pub avail: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskUsage {
    pub total: u64,
    pub avail: u64,
    pub used: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryState {
    Charging,
    Discharging,
    Full,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryReading {
    pub percent: u8,
    pub state: BatteryState,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemSnapshot {
    /// Busy percentage, 0-100.
    pub cpu_busy: Option<f64>,
    pub ram: Option<MemUsage>,
    /// The filesystem holding the current directory.
    pub disk_cwd: Option<DiskUsage>,
    /// Filesystems requested by explicit path (`{disk:/home}`).
    pub disk_paths: HashMap<String, DiskUsage>,
    pub battery: Option<BatteryReading>,
    pub load: Option<[f64; 3]>,
    pub uptime_secs: Option<u64>,
    pub user: Option<String>,
    /// The full hostname; widgets derive the short form.
    pub host: Option<String>,
    pub cores: usize,
}

pub(crate) fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|field| field.parse().ok())
        .collect::<Option<Vec<u64>>>()?;
    if fields.len() < 4 {
        return None;
    }
    let get = |index: usize| fields.get(index).copied().unwrap_or(0);
    // user nice system idle iowait irq softirq steal
    let total: u64 = (0..8).map(get).sum();
    Some(CpuTimes {
        total,
        idle: get(3) + get(4),
    })
}

/// Busy percentage since `prev`, or since boot when there is no previous
/// sample. `None` when no time has elapsed (the caller reuses the last value).
pub(crate) fn cpu_busy_percent(prev: Option<CpuTimes>, now: CpuTimes) -> Option<f64> {
    let (total, idle) = match prev {
        Some(prev) => (
            now.total.checked_sub(prev.total)?,
            now.idle.checked_sub(prev.idle)?,
        ),
        None => (now.total, now.idle),
    };
    if total == 0 {
        return None;
    }
    Some((total.saturating_sub(idle)) as f64 / total as f64 * 100.0)
}

pub(crate) fn parse_meminfo(text: &str) -> Option<MemUsage> {
    let kb = |name: &str| -> Option<u64> {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
    };
    let total = kb("MemTotal")?;
    let avail = kb("MemAvailable").or_else(|| {
        Some(kb("MemFree")? + kb("Buffers").unwrap_or(0) + kb("Cached").unwrap_or(0))
    })?;
    Some(MemUsage {
        total: total * 1024,
        avail: avail * 1024,
    })
}

pub(crate) fn parse_loadavg(text: &str) -> Option<[f64; 3]> {
    let mut fields = text.split_whitespace();
    Some([
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    ])
}

pub(crate) fn parse_uptime(text: &str) -> Option<u64> {
    let seconds: f64 = text.split_whitespace().next()?.parse().ok()?;
    Some(seconds as u64)
}

pub(crate) fn read_battery(sys_root: &Path) -> Option<BatteryReading> {
    let supplies = sys_root.join("class/power_supply");
    let mut names: Vec<String> = std::fs::read_dir(supplies)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("BAT"))
        .collect();
    names.sort();
    let battery = sys_root.join("class/power_supply").join(names.first()?);
    let percent: u8 = std::fs::read_to_string(battery.join("capacity"))
        .ok()?
        .trim()
        .parse::<u8>()
        .ok()?
        .min(100);
    let state = match std::fs::read_to_string(battery.join("status"))
        .unwrap_or_default()
        .trim()
    {
        "Charging" => BatteryState::Charging,
        "Discharging" => BatteryState::Discharging,
        "Full" => BatteryState::Full,
        _ => BatteryState::Unknown,
    };
    Some(BatteryReading { percent, state })
}

/// `statvfs` with `df` semantics: used = blocks - free blocks; availability is
/// what an unprivileged user can use.
pub(crate) fn disk_usage(path: &Path) -> Option<DiskUsage> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize as u64;
    let total = stat.f_blocks as u64 * block;
    if total == 0 {
        return None;
    }
    Some(DiskUsage {
        total,
        avail: stat.f_bavail as u64 * block,
        used: (stat.f_blocks as u64).saturating_sub(stat.f_bfree as u64) * block,
    })
}

/// Runs [`disk_usage`] on a helper thread and gives up after `timeout`. The
/// thread is left to finish on its own if the filesystem is hung.
pub(crate) fn disk_usage_with_timeout(path: &Path, timeout: Duration) -> Option<DiskUsage> {
    let (sender, receiver) = mpsc::channel();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = sender.send(disk_usage(&path));
    });
    receiver.recv_timeout(timeout).ok().flatten()
}

fn current_user() -> Option<String> {
    if let Ok(user) = std::env::var("USER") {
        if !user.is_empty() {
            return Some(user);
        }
    }
    let mut buffer = vec![0u8; 1024];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut passwd,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(passwd.pw_name) };
    Some(name.to_string_lossy().into_owned())
}

fn current_host() -> Option<String> {
    let mut buffer = vec![0u8; 256];
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    let host = String::from_utf8_lossy(&buffer[..end]).into_owned();
    (!host.is_empty()).then_some(host)
}

pub struct SystemSampler {
    proc_root: PathBuf,
    sys_root: PathBuf,
    prev_cpu: Option<CpuTimes>,
    last_cpu: Option<f64>,
    user: Option<Option<String>>,
    host: Option<Option<String>>,
}

impl Default for SystemSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemSampler {
    pub fn new() -> Self {
        Self::with_roots(PathBuf::from("/proc"), PathBuf::from("/sys"))
    }

    /// For tests: read from fixture directories instead of `/proc` and `/sys`.
    pub fn with_roots(proc_root: PathBuf, sys_root: PathBuf) -> Self {
        SystemSampler {
            proc_root,
            sys_root,
            prev_cpu: None,
            last_cpu: None,
            user: None,
            host: None,
        }
    }

    pub fn sample(&mut self, needed: &NeededWidgets, cwd: &Path) -> SystemSnapshot {
        let mut snapshot = SystemSnapshot {
            cores: std::thread::available_parallelism().map_or(1, |cores| cores.get()),
            ..SystemSnapshot::default()
        };
        let read = |name: &str| std::fs::read_to_string(self.proc_root.join(name)).ok();

        if needed.kinds.contains(&WidgetKind::Cpu) {
            if let Some(now) = read("stat").as_deref().and_then(parse_proc_stat) {
                self.last_cpu = cpu_busy_percent(self.prev_cpu, now).or(self.last_cpu);
                self.prev_cpu = Some(now);
            }
            snapshot.cpu_busy = self.last_cpu;
        }
        if needed.kinds.contains(&WidgetKind::Ram) {
            snapshot.ram = read("meminfo").as_deref().and_then(parse_meminfo);
        }
        if needed.kinds.contains(&WidgetKind::Load) {
            snapshot.load = read("loadavg").as_deref().and_then(parse_loadavg);
        }
        if needed.kinds.contains(&WidgetKind::Uptime) {
            snapshot.uptime_secs = read("uptime").as_deref().and_then(parse_uptime);
        }
        if needed.kinds.contains(&WidgetKind::Battery) {
            snapshot.battery = read_battery(&self.sys_root);
        }
        if needed.kinds.contains(&WidgetKind::Disk) {
            snapshot.disk_cwd = disk_usage_with_timeout(cwd, DISK_TIMEOUT);
            for path in &needed.disk_paths {
                if let Some(usage) = disk_usage_with_timeout(Path::new(path), DISK_TIMEOUT) {
                    snapshot.disk_paths.insert(path.clone(), usage);
                }
            }
        }
        if needed.kinds.contains(&WidgetKind::User) {
            snapshot.user = self.user.get_or_insert_with(current_user).clone();
        }
        if needed.kinds.contains(&WidgetKind::Host) {
            snapshot.host = self.host.get_or_insert_with(current_host).clone();
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat_and_computes_busy_delta() {
        let a = parse_proc_stat("cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 1 1 1\n").unwrap();
        assert_eq!(
            a,
            CpuTimes {
                total: 1000,
                idle: 850
            }
        );
        assert!((cpu_busy_percent(None, a).unwrap() - 15.0).abs() < 1e-9);
        let b = parse_proc_stat("cpu  150 0 100 800 50 0 0 0 0 0\n").unwrap();
        assert!((cpu_busy_percent(Some(a), b).unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(cpu_busy_percent(Some(b), b), None);
        assert!(parse_proc_stat("garbage").is_none());
    }

    #[test]
    fn parses_meminfo_with_and_without_memavailable() {
        let m = parse_meminfo(
            "MemTotal:       16000000 kB\nMemFree: 1000000 kB\nMemAvailable:    8000000 kB\n",
        )
        .unwrap();
        assert_eq!(
            m,
            MemUsage {
                total: 16_000_000 * 1024,
                avail: 8_000_000 * 1024
            }
        );
        let f =
            parse_meminfo("MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 150 kB\n")
                .unwrap();
        assert_eq!(
            f,
            MemUsage {
                total: 1000 * 1024,
                avail: 300 * 1024
            }
        );
        assert!(parse_meminfo("Nothing: 1 kB").is_none());
    }

    #[test]
    fn parses_loadavg_and_uptime() {
        assert_eq!(
            parse_loadavg("0.52 0.61 0.70 1/512 12345\n"),
            Some([0.52, 0.61, 0.70])
        );
        assert_eq!(parse_uptime("12345.67 9999.00\n"), Some(12345));
        assert!(parse_loadavg("x").is_none() && parse_uptime("").is_none());
    }

    #[test]
    fn reads_battery_from_a_fake_sysfs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_battery(dir.path()).is_none());
        let bat = dir.path().join("class/power_supply/BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("capacity"), "83\n").unwrap();
        std::fs::write(bat.join("status"), "Charging\n").unwrap();
        std::fs::create_dir_all(dir.path().join("class/power_supply/AC")).unwrap();
        assert_eq!(
            read_battery(dir.path()),
            Some(BatteryReading {
                percent: 83,
                state: BatteryState::Charging
            })
        );
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();
        assert_eq!(
            read_battery(dir.path()).unwrap().state,
            BatteryState::Discharging
        );
        std::fs::write(bat.join("status"), "Not charging\n").unwrap();
        assert_eq!(
            read_battery(dir.path()).unwrap().state,
            BatteryState::Unknown
        );
    }

    #[test]
    fn statvfs_reports_sane_numbers_for_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        let d = disk_usage(dir.path()).expect("statvfs on a temp dir");
        assert!(d.total > 0 && d.avail <= d.total && d.used <= d.total);
        assert!(disk_usage(Path::new("/definitely/not/here")).is_none());
        assert!(disk_usage_with_timeout(dir.path(), Duration::from_secs(2)).is_some());
    }

    #[test]
    fn sampler_reads_only_what_is_needed_and_reuses_cpu_delta() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("stat"), "cpu  100 0 50 800 50 0 0 0 0 0\n").unwrap();
        let mut sampler =
            SystemSampler::with_roots(root.path().to_path_buf(), root.path().to_path_buf());
        let mut needed = NeededWidgets::default();
        needed.kinds.insert(WidgetKind::Cpu);
        let s1 = sampler.sample(&needed, root.path());
        assert!((s1.cpu_busy.unwrap() - 15.0).abs() < 1e-9);
        assert!(
            s1.ram.is_none() && s1.load.is_none(),
            "unneeded widgets must not be sampled"
        );
        let s2 = sampler.sample(&needed, root.path());
        assert!(
            (s2.cpu_busy.unwrap() - 15.0).abs() < 1e-9,
            "zero delta reuses the last value"
        );
        std::fs::write(
            root.path().join("stat"),
            "cpu  150 0 100 800 50 0 0 0 0 0\n",
        )
        .unwrap();
        let s3 = sampler.sample(&needed, root.path());
        assert!((s3.cpu_busy.unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn missing_files_make_widgets_empty_not_errors() {
        let empty = tempfile::tempdir().unwrap();
        let mut sampler =
            SystemSampler::with_roots(empty.path().to_path_buf(), empty.path().to_path_buf());
        let mut needed = NeededWidgets::default();
        for kind in [
            WidgetKind::Cpu,
            WidgetKind::Ram,
            WidgetKind::Load,
            WidgetKind::Uptime,
            WidgetKind::Battery,
        ] {
            needed.kinds.insert(kind);
        }
        let s = sampler.sample(&needed, empty.path());
        assert!(s.cpu_busy.is_none() && s.ram.is_none() && s.load.is_none());
        assert!(s.uptime_secs.is_none() && s.battery.is_none());
    }

    #[test]
    fn user_and_host_are_cached_across_samples() {
        let empty = tempfile::tempdir().unwrap();
        let mut sampler =
            SystemSampler::with_roots(empty.path().to_path_buf(), empty.path().to_path_buf());
        let mut needed = NeededWidgets::default();
        needed.kinds.insert(WidgetKind::User);
        needed.kinds.insert(WidgetKind::Host);
        let first = sampler.sample(&needed, empty.path());
        let second = sampler.sample(&needed, empty.path());
        assert_eq!(first.user, second.user);
        assert_eq!(first.host, second.host);
    }
}
