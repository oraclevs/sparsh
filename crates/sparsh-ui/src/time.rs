pub(crate) fn format_local_time(format: &str) -> Result<String, String> {
    #[cfg(unix)]
    {
        let mut now = unsafe { libc::time(std::ptr::null_mut()) };
        let mut local: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&mut now, &mut local) }.is_null() {
            return Err("failed to read local time".into());
        }
        return format_tm(format, &local);
    }
    #[cfg(not(unix))]
    {
        let _ = format;
        Err("local prompt time is unsupported on this platform".into())
    }
}

#[cfg(unix)]
fn format_tm(format: &str, tm: &libc::tm) -> Result<String, String> {
    let hour24 = tm.tm_hour;
    let minute = tm.tm_min;
    let second = tm.tm_sec;
    match format {
        "HH:mm" => Ok(format!("{hour24:02}:{minute:02}")),
        "HH:mm:ss" => Ok(format!("{hour24:02}:{minute:02}:{second:02}")),
        "hh:mm:ss a" => {
            let period = if hour24 < 12 { "AM" } else { "PM" };
            let hour12 = match hour24 % 12 { 0 => 12, value => value };
            Ok(format!("{hour12:02}:{minute:02}:{second:02} {period}"))
        }
        other => Err(format!("unsupported time format: {other}")),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn deterministic_time_formats() {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_hour = 17;
        tm.tm_min = 31;
        tm.tm_sec = 46;
        assert_eq!(super::format_tm("HH:mm:ss", &tm).unwrap(), "17:31:46");
        assert_eq!(super::format_tm("hh:mm:ss a", &tm).unwrap(), "05:31:46 PM");
    }
}
