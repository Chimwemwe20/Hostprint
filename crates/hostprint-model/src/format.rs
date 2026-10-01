//! Human-readable formatting shared by the diff engine and the CLI.

/// Formats a byte count with binary units: `0 B`, `512 B`, `1.5 KiB`, `5.4 GiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Formats a duration in seconds compactly: `45s`, `12m`, `4h 21m`, `3d 2h`.
pub fn duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// Relative change from `before` to `after` as a signed percentage string,
/// e.g. `+180%` or `-82%`. `None` when `before` is zero.
pub fn percent_change(before: f64, after: f64) -> Option<String> {
    if before == 0.0 {
        return None;
    }
    let pct = (after - before) / before * 100.0;
    Some(format!("{}{:.0}%", if pct >= 0.0 { "+" } else { "" }, pct))
}

/// Signed integer delta, e.g. `+17` or `-3`.
pub fn signed(delta: i64) -> String {
    if delta >= 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(5_798_205_849), "5.4 GiB");
        assert_eq!(bytes(200 * 1024 * 1024), "200 MiB");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(12 * 60), "12m");
        assert_eq!(duration(4 * 3600 + 21 * 60 + 5), "4h 21m");
        assert_eq!(duration(3 * 86_400 + 2 * 3600), "3d 2h");
    }

    #[test]
    fn formats_changes() {
        assert_eq!(percent_change(5.3, 0.92).as_deref(), Some("-83%"));
        assert_eq!(percent_change(100.0, 280.0).as_deref(), Some("+180%"));
        assert_eq!(percent_change(0.0, 5.0), None);
        assert_eq!(signed(17), "+17");
        assert_eq!(signed(-3), "-3");
    }
}
