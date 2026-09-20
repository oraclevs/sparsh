use super::{error, success, usage_error, BuiltinContext, BuiltinRegistry, BuiltinResult};

pub(super) fn umask(args: &[String], _: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    match args {
        [] => Ok(success(Some(format!("{:04o}\n", spar_process::current_umask() & 0o7777)))),
        [value] => {
            let mask = u32::from_str_radix(value, 8).map_err(|_| usage_error("umask [0000-0777]"))?;
            if mask > 0o777 { return Err(usage_error("umask [0000-0777]")); }
            spar_process::set_umask(mask);
            Ok(success(None))
        }
        _ => Err(usage_error("umask [0000-0777]")),
    }
}

#[cfg(unix)]
pub(super) fn ulimit(args: &[String], _: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    use spar_process::LimitResource;
    if args == ["-a"] {
        let mut out = String::new();
        for (flag, label, resource, scale) in [
            ("-n", "open files", LimitResource::NoFile, 1u64),
            ("-c", "core file size", LimitResource::Core, 1024u64),
            ("-s", "stack size", LimitResource::Stack, 1024u64),
            ("-u", "max user processes", LimitResource::NProc, 1u64),
        ] {
            let (soft, _) = spar_process::get_limit(resource).map_err(|e| error(format!("ulimit: {e}")))?;
            out.push_str(&format!("{flag} {label:<20} {}\n", format_limit(soft, scale)));
        }
        return Ok(success(Some(out)));
    }
    let (flag, value) = match args {
        [flag] => (flag.as_str(), None),
        [flag, value] => (flag.as_str(), Some(value.as_str())),
        _ => return Err(usage_error("ulimit -a | -n|-c|-s|-u [value|unlimited]")),
    };
    let (resource, scale) = match flag {
        "-n" => (LimitResource::NoFile, 1u64),
        "-c" => (LimitResource::Core, 1024u64),
        "-s" => (LimitResource::Stack, 1024u64),
        "-u" => (LimitResource::NProc, 1u64),
        _ => return Err(usage_error("ulimit -a | -n|-c|-s|-u [value|unlimited]")),
    };
    if let Some(value) = value {
        let target = if value == "unlimited" {
            libc_rlim_infinity()
        } else {
            value.parse::<u64>().map_err(|_| usage_error("ulimit -a | -n|-c|-s|-u [value|unlimited]"))?.saturating_mul(scale)
        };
        spar_process::set_soft_limit(resource, target).map_err(|e| error(format!("ulimit: {e}")))?;
        return Ok(success(None));
    }
    let (soft, _) = spar_process::get_limit(resource).map_err(|e| error(format!("ulimit: {e}")))?;
    Ok(success(Some(format!("{}\n", format_limit(soft, scale)))))
}

#[cfg(unix)]
fn libc_rlim_infinity() -> u64 { u64::MAX }

#[cfg(unix)]
fn format_limit(value: u64, scale: u64) -> String {
    if value == u64::MAX { "unlimited".into() } else { (value / scale).to_string() }
}

#[cfg(not(unix))]
pub(super) fn ulimit(_: &[String], _: &mut BuiltinContext<'_>, _: &BuiltinRegistry) -> BuiltinResult {
    Err(error("ulimit: unsupported on this platform"))
}
