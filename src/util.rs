//! Small formatting helpers shared by the UI and exporters.

use chrono::TimeZone;

pub fn fmt_bytes(b: u64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, U[i])
}

pub fn fmt_bytes_f(b: f64) -> String {
    fmt_bytes(b.max(0.0) as u64)
}

pub fn fmt_rate(bps: f64) -> String {
    format!("{}/s", fmt_bytes(bps.max(0.0) as u64))
}

pub fn fmt_duration(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Human friendly "time until" for forecasts.
pub fn fmt_eta(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "n/a".into();
    }
    let s = secs as u64;
    if s < 3600 {
        format!("{} min", (s / 60).max(1))
    } else if s < 86_400 {
        format!("{:.1} h", secs / 3600.0)
    } else {
        format!("{:.1} days", secs / 86_400.0)
    }
}

/// Axis label for a time window: 30 -> "30s", 300 -> "5m", 90 -> "1m30s".
pub fn fmt_window(secs: f64) -> String {
    let s = secs.round() as u64;
    if s < 60 {
        format!("{s}s")
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{}m{}s", s / 60, s % 60)
    }
}

/// Unicode bar, e.g. `bar(0.5, 10)` -> "█████░░░░░".
pub fn bar(ratio: f64, width: usize) -> String {
    let r = if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { 0.0 };
    let filled = (r * width as f64).round() as usize;
    let mut s = String::with_capacity(width * 3);
    for i in 0..width {
        s.push(if i < filled { '█' } else { '░' });
    }
    s
}

/// Local wall clock (HH:MM:SS) for a unix timestamp in seconds.
pub fn clock(ts: f64) -> String {
    match chrono::Local.timestamp_opt(ts as i64, 0).single() {
        Some(dt) => dt.format("%H:%M:%S").to_string(),
        None => "--:--:--".into(),
    }
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else if n <= 1 {
        s.chars().take(n).collect()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2.0 KiB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }

    #[test]
    fn durations_and_windows() {
        assert_eq!(fmt_duration(59), "59s");
        assert_eq!(fmt_duration(3661), "1h 1m 1s");
        assert_eq!(fmt_duration(90_000), "1d 1h 0m");
        assert_eq!(fmt_window(300.0), "5m");
        assert_eq!(fmt_window(90.0), "1m30s");
        assert_eq!(fmt_eta(7200.0), "2.0 h");
    }

    #[test]
    fn bars_and_truncate() {
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(f64::NAN, 3), "░░░");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
