//! Renders right-prompt slot templates from a plain snapshot of the machine
//! and shell state. Nothing here performs I/O (other than `strftime`), so it is
//! deterministic and unit-testable with fixed inputs.

use std::time::Duration;

use sparsh_core::{Piece, Template, Threshold, Thresholds, WidgetKind, WidgetRef};

use crate::sampler::{BatteryState, DiskUsage, SystemSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Normal,
    Warn,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedPiece {
    pub text: String,
    /// The widget that produced this text; `None` for literal template text.
    pub kind: Option<WidgetKind>,
    pub level: Level,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedSlot {
    pub pieces: Vec<RenderedPiece>,
}

impl RenderedSlot {
    pub fn plain(&self) -> String {
        self.pieces.iter().map(|piece| piece.text.as_str()).collect()
    }
}

#[derive(Clone, Copy)]
pub struct LocalTime {
    tm: libc::tm,
}

impl LocalTime {
    pub fn now() -> Option<LocalTime> {
        let mut now = unsafe { libc::time(std::ptr::null_mut()) };
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&mut now, &mut tm) }.is_null() {
            return None;
        }
        Some(LocalTime { tm })
    }

    /// Builds a time from UTC calendar parts (weekday and day-of-year are
    /// computed). Meant for tests that need a fixed, timezone-independent time.
    pub fn from_utc_parts(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> LocalTime {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = year - 1900;
        tm.tm_mon = month as i32 - 1;
        tm.tm_mday = day as i32;
        tm.tm_hour = hour as i32;
        tm.tm_min = min as i32;
        tm.tm_sec = sec as i32;
        let seconds = unsafe { libc::timegm(&mut tm) };
        let mut normalised: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::gmtime_r(&seconds, &mut normalised) };
        LocalTime { tm: normalised }
    }

    /// `strftime`, into a bounded buffer. `None` when the result is empty or
    /// does not fit. Control characters are removed.
    pub fn format(&self, strftime: &str) -> Option<String> {
        let format = std::ffi::CString::new(strftime).ok()?;
        let mut buffer = [0u8; 256];
        let written = unsafe {
            libc::strftime(buffer.as_mut_ptr().cast(), buffer.len(), format.as_ptr(), &self.tm)
        };
        if written == 0 {
            return None;
        }
        let text = sanitize(&String::from_utf8_lossy(&buffer[..written]));
        (!text.is_empty()).then_some(text)
    }
}

pub struct WidgetInputs<'a> {
    pub now: Option<LocalTime>,
    pub last_duration: Option<Duration>,
    pub duration_threshold: Duration,
    pub jobs: usize,
    pub system: &'a SystemSnapshot,
}

/// Renders a slot. `None` means the slot is hidden: it has placeholders and
/// every one of them rendered empty.
pub fn render_slot(
    template: &Template,
    inputs: &WidgetInputs,
    thresholds: &Thresholds,
) -> Option<RenderedSlot> {
    let mut pieces = Vec::new();
    for piece in &template.pieces {
        match piece {
            Piece::Literal(text) => pieces.push(RenderedPiece {
                text: text.clone(),
                kind: None,
                level: Level::Normal,
            }),
            Piece::Widget(widget) => {
                let (text, level) = render_widget(widget, inputs, thresholds);
                pieces.push(RenderedPiece {
                    text: sanitize(&text),
                    kind: Some(widget.kind),
                    level,
                });
            }
        }
    }
    let all_empty = pieces
        .iter()
        .filter(|piece| piece.kind.is_some())
        .all(|piece| piece.text.is_empty());
    if template.has_widgets() && all_empty {
        return None;
    }
    Some(RenderedSlot { pieces })
}

fn sanitize(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

const SIZE_MODES: [&str; 4] = ["free", "used", "avail", "total"];

fn size_mode(args: &[String]) -> Option<&str> {
    args.iter()
        .map(String::as_str)
        .find(|arg| SIZE_MODES.contains(arg))
}

fn percent(value: f64) -> u32 {
    value.round().clamp(0.0, 100.0) as u32
}

fn level_high(value: f64, threshold: Threshold, default_warn: f64, default_critical: f64) -> Level {
    let warn = threshold.warn.map_or(default_warn, f64::from);
    let critical = threshold.critical.map_or(default_critical, f64::from);
    if value >= critical {
        Level::Critical
    } else if value >= warn {
        Level::Warn
    } else {
        Level::Normal
    }
}

fn disk_used_percent(disk: &DiskUsage) -> f64 {
    let denominator = disk.used + disk.avail;
    if denominator == 0 {
        0.0
    } else {
        disk.used as f64 / denominator as f64 * 100.0
    }
}

fn render_widget(widget: &WidgetRef, inputs: &WidgetInputs, thresholds: &Thresholds) -> (String, Level) {
    let system = inputs.system;
    let args = &widget.args;
    let normal = |text: String| (text, Level::Normal);
    let empty = || (String::new(), Level::Normal);

    match widget.kind {
        WidgetKind::Time | WidgetKind::Date => {
            let default = if widget.kind == WidgetKind::Time { "%H:%M:%S" } else { "%Y-%m-%d" };
            let format = args.first().map_or(default, String::as_str);
            normal(inputs.now.as_ref().and_then(|now| now.format(format)).unwrap_or_default())
        }
        WidgetKind::Duration => match inputs.last_duration {
            Some(duration) if duration >= inputs.duration_threshold => {
                normal(format_duration(duration))
            }
            _ => empty(),
        },
        WidgetKind::Cpu => {
            let Some(busy) = system.cpu_busy else { return empty() };
            let used = percent(busy);
            let shown = if args.first().is_some_and(|arg| arg == "free") { 100 - used } else { used };
            (shown.to_string(), level_high(busy, thresholds.cpu, 70.0, 90.0))
        }
        WidgetKind::Ram => {
            let Some(memory) = system.ram else { return empty() };
            if memory.total == 0 {
                return empty();
            }
            let used_bytes = memory.total.saturating_sub(memory.avail);
            let used = used_bytes as f64 / memory.total as f64 * 100.0;
            let level = level_high(used, thresholds.ram, 80.0, 90.0);
            let text = match size_mode(args) {
                None => percent(used).to_string(),
                Some("free") => (100 - percent(used)).to_string(),
                Some("used") => human_size(used_bytes),
                Some("avail") => human_size(memory.avail),
                Some(_) => human_size(memory.total),
            };
            (text, level)
        }
        WidgetKind::Disk => {
            let disk = match args.iter().find(|arg| arg.starts_with('/')) {
                Some(path) => system.disk_paths.get(path),
                None => system.disk_cwd.as_ref(),
            };
            let Some(disk) = disk else { return empty() };
            let used = disk_used_percent(disk);
            let level = level_high(used, thresholds.disk, 85.0, 95.0);
            let text = match size_mode(args) {
                None => percent(used).to_string(),
                Some("free") => (100 - percent(used)).to_string(),
                Some("used") => human_size(disk.used),
                Some("avail") => human_size(disk.avail),
                Some(_) => human_size(disk.total),
            };
            (text, level)
        }
        WidgetKind::Battery => {
            let Some(battery) = system.battery else { return empty() };
            let level = if matches!(battery.state, BatteryState::Charging | BatteryState::Full) {
                Level::Normal
            } else {
                let warn = thresholds.battery.warn.unwrap_or(30);
                let critical = thresholds.battery.critical.unwrap_or(15);
                let charge = u32::from(battery.percent);
                if charge <= critical {
                    Level::Critical
                } else if charge <= warn {
                    Level::Warn
                } else {
                    Level::Normal
                }
            };
            let text = match args.first().map(String::as_str) {
                Some("state") => match battery.state {
                    BatteryState::Charging => "charging",
                    BatteryState::Discharging => "discharging",
                    BatteryState::Full => "full",
                    BatteryState::Unknown => "unknown",
                }
                .to_string(),
                Some("icon") => battery_icon(
                    battery.percent,
                    matches!(battery.state, BatteryState::Charging),
                )
                .to_string(),
                _ => battery.percent.to_string(),
            };
            (text, level)
        }
        WidgetKind::Load => {
            let Some(load) = system.load else { return empty() };
            let index = match args.first().map(String::as_str) {
                Some("5") => 1,
                Some("15") => 2,
                _ => 0,
            };
            let cores = system.cores.max(1) as f64;
            let value = load[index];
            (
                format!("{value:.2}"),
                level_high(value, thresholds.load, cores, cores * 2.0),
            )
        }
        WidgetKind::Uptime => match system.uptime_secs {
            Some(seconds) => normal(format_uptime(seconds)),
            None => empty(),
        },
        WidgetKind::User => normal(system.user.clone().unwrap_or_default()),
        WidgetKind::Host => match &system.host {
            Some(host) if args.first().is_some_and(|arg| arg == "full") => normal(host.clone()),
            Some(host) => normal(host.split('.').next().unwrap_or(host).to_string()),
            None => empty(),
        },
        WidgetKind::Jobs => {
            if inputs.jobs > 0 {
                normal(inputs.jobs.to_string())
            } else {
                empty()
            }
        }
    }
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds.fract() < 0.05 {
        format!("{}s", seconds.round() as u64)
    } else {
        format!("{seconds:.1}s")
    }
}

pub fn format_uptime(seconds: u64) -> String {
    let (days, hours, minutes) = (seconds / 86_400, seconds % 86_400 / 3_600, seconds % 3_600 / 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

pub fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes < KIB {
        format!("{bytes}B")
    } else if bytes < MIB {
        format!("{}K", bytes / KIB)
    } else if bytes < GIB {
        format!("{}M", bytes / MIB)
    } else {
        format!("{:.1}G", bytes as f64 / GIB as f64)
    }
}

/// Nerd Font (Material Design) battery glyph for a charge level, or the
/// charging glyph.
pub fn battery_icon(percent: u8, charging: bool) -> char {
    const LEVELS: [char; 11] = [
        '\u{f008e}', '\u{f007a}', '\u{f007b}', '\u{f007c}', '\u{f007d}', '\u{f007e}',
        '\u{f007f}', '\u{f0080}', '\u{f0081}', '\u{f0082}', '\u{f0079}',
    ];
    if charging {
        '\u{f0084}'
    } else {
        LEVELS[usize::from(percent.min(100) / 10)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampler::{BatteryReading, MemUsage};

    fn inputs_with(system: &SystemSnapshot) -> WidgetInputs<'_> {
        WidgetInputs {
            now: Some(LocalTime::from_utc_parts(2026, 9, 20, 17, 31, 46)),
            last_duration: None,
            duration_threshold: Duration::from_secs(2),
            jobs: 0,
            system,
        }
    }

    fn render_with(src: &str, inputs: &WidgetInputs) -> Option<String> {
        render_slot(&Template::parse(src).unwrap(), inputs, &Thresholds::default())
            .map(|slot| slot.plain())
    }

    fn render(src: &str, system: &SystemSnapshot) -> Option<String> {
        render_with(src, &inputs_with(system))
    }

    fn level(src: &str, system: &SystemSnapshot, thresholds: &Thresholds) -> Level {
        render_slot(&Template::parse(src).unwrap(), &inputs_with(system), thresholds)
            .unwrap()
            .pieces
            .iter()
            .find(|piece| piece.kind.is_some())
            .unwrap()
            .level
    }

    #[test]
    fn time_and_date_use_strftime() {
        let s = SystemSnapshot::default();
        assert_eq!(render("{time:%H:%M}", &s).unwrap(), "17:31");
        assert_eq!(render("{time:%I:%M %p}", &s).unwrap(), "05:31 PM");
        assert_eq!(render("{date:%a %d %b}", &s).unwrap(), "Sun 20 Sep");
        assert_eq!(render("{date}", &s).unwrap(), "2026-09-20");
        assert_eq!(render("{time}", &s).unwrap(), "17:31:46");
        assert_eq!(render("{date:%a, %d %b}", &s).unwrap(), "Sun, 20 Sep");
    }

    #[test]
    fn percent_widgets_print_bare_numbers_and_modes() {
        let s = SystemSnapshot {
            cpu_busy: Some(12.4),
            ram: Some(MemUsage { total: 16 << 30, avail: 4 << 30 }),
            disk_cwd: Some(DiskUsage { total: 100 << 30, avail: 5 << 30, used: 95 << 30 }),
            ..Default::default()
        };
        assert_eq!(render("{cpu}%", &s).unwrap(), "12%");
        assert_eq!(render("{cpu:free}%", &s).unwrap(), "88%");
        assert_eq!(render("{ram}%", &s).unwrap(), "75%");
        assert_eq!(render("{ram:free}%", &s).unwrap(), "25%");
        assert_eq!(render("{ram:used}", &s).unwrap(), "12.0G");
        assert_eq!(render("{ram:total}", &s).unwrap(), "16.0G");
        assert_eq!(render("{disk}%", &s).unwrap(), "95%");
        assert_eq!(render("{disk:free}%", &s).unwrap(), "5%");
        assert_eq!(render("{disk:avail}", &s).unwrap(), "5.0G");
    }

    #[test]
    fn explicit_disk_path_uses_its_own_reading() {
        let mut s = SystemSnapshot::default();
        s.disk_paths.insert("/home".into(), DiskUsage { total: 200, avail: 50, used: 150 });
        assert_eq!(render("{disk:/home}%", &s).unwrap(), "75%");
        assert!(render("{disk:/nowhere}%", &s).is_none());
    }

    #[test]
    fn duration_hides_under_threshold_and_formats_above() {
        let s = SystemSnapshot::default();
        let mut i = inputs_with(&s);
        i.last_duration = Some(Duration::from_millis(500));
        assert!(render_with("{duration}", &i).is_none());
        i.last_duration = Some(Duration::from_millis(2400));
        assert_eq!(render_with("{duration}", &i).unwrap(), "2.4s");
        i.last_duration = Some(Duration::from_secs(3));
        assert_eq!(render_with("{duration}", &i).unwrap(), "3s");
    }

    #[test]
    fn battery_variants() {
        let mut s = SystemSnapshot {
            battery: Some(BatteryReading { percent: 83, state: BatteryState::Charging }),
            ..Default::default()
        };
        assert_eq!(render("{battery}%", &s).unwrap(), "83%");
        assert_eq!(render("{battery:state}", &s).unwrap(), "charging");
        assert_eq!(render("{battery:icon}", &s).unwrap().chars().count(), 1);
        s.battery = None;
        assert!(render("{battery}%", &s).is_none(), "no battery hides the slot");
    }

    #[test]
    fn load_uptime_user_host_jobs() {
        let s = SystemSnapshot {
            load: Some([0.5, 1.25, 2.0]),
            uptime_secs: Some(3 * 86_400 + 4 * 3_600 + 5),
            user: Some("occ".into()),
            host: Some("arch.local".into()),
            cores: 8,
            ..Default::default()
        };
        assert_eq!(render("{load}", &s).unwrap(), "0.50");
        assert_eq!(render("{load:5}", &s).unwrap(), "1.25");
        assert_eq!(render("{uptime}", &s).unwrap(), "3d 4h");
        assert_eq!(render("{user}@{host}", &s).unwrap(), "occ@arch");
        assert_eq!(render("{host:full}", &s).unwrap(), "arch.local");
        assert!(render("{jobs}", &s).is_none());
        let mut i = inputs_with(&s);
        i.jobs = 2;
        assert_eq!(render_with("bg:{jobs}", &i).unwrap(), "bg:2");
    }

    #[test]
    fn slot_with_only_empty_placeholders_hides_including_literals_but_literal_only_slots_show() {
        let s = SystemSnapshot::default();
        assert!(render("  {jobs}", &s).is_none());
        assert_eq!(render("hello", &s).unwrap(), "hello");
        assert_eq!(render("[{jobs}{time:%H}]", &s).unwrap(), "[17]");
    }

    #[test]
    fn threshold_levels_are_computed_from_stress_not_the_displayed_number() {
        let t = Thresholds::default();
        let s = SystemSnapshot {
            cpu_busy: Some(95.0),
            disk_cwd: Some(DiskUsage { total: 100, avail: 4, used: 96 }),
            battery: Some(BatteryReading { percent: 10, state: BatteryState::Discharging }),
            cores: 4,
            ..Default::default()
        };
        assert_eq!(level("{cpu}", &s, &t), Level::Critical);
        assert_eq!(level("{disk:free}", &s, &t), Level::Critical);
        assert_eq!(level("{battery}", &s, &t), Level::Critical);
        let mut charging = s.clone();
        charging.battery = Some(BatteryReading { percent: 10, state: BatteryState::Charging });
        assert_eq!(level("{battery}", &charging, &t), Level::Normal);
        let warn = SystemSnapshot { cpu_busy: Some(75.0), ..Default::default() };
        assert_eq!(level("{cpu}", &warn, &t), Level::Warn);
        let calm = SystemSnapshot { cpu_busy: Some(10.0), ..Default::default() };
        assert_eq!(level("{cpu}", &calm, &t), Level::Normal);
    }

    #[test]
    fn custom_thresholds_and_load_defaults_scale_with_cores() {
        let mut t = Thresholds::default();
        t.cpu = Threshold { warn: Some(10), critical: Some(20) };
        let s = SystemSnapshot {
            cpu_busy: Some(15.0),
            load: Some([9.0, 0.0, 0.0]),
            cores: 4,
            ..Default::default()
        };
        assert_eq!(level("{cpu}", &s, &t), Level::Warn);
        // load default: warn = cores (4), critical = 2 * cores (8)
        assert_eq!(level("{load}", &s, &Thresholds::default()), Level::Critical);
    }

    #[test]
    fn control_characters_never_reach_the_output() {
        let s = SystemSnapshot { user: Some("a\nb\u{1b}c".into()), ..Default::default() };
        assert_eq!(render("{user}", &s).unwrap(), "abc");
    }

    #[test]
    fn helpers() {
        assert_eq!(human_size(12), "12B");
        assert_eq!(human_size(900 * 1024), "900K");
        assert_eq!(human_size(512 << 20), "512M");
        assert_eq!(human_size(5u64 << 30), "5.0G");
        assert_eq!(format_uptime(12 * 60 + 3), "12m");
        assert_eq!(format_uptime(4 * 3_600 + 12 * 60), "4h 12m");
        assert_ne!(battery_icon(90, false), battery_icon(10, false));
        assert_ne!(battery_icon(50, true), battery_icon(50, false));
        assert!(battery_icon(0, false) as u32 >= 0xF0000);
    }
}
