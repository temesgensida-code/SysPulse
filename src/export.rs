//! Report exporters (text / Markdown / JSON) and the alert event log.

use crate::health::{Alert, Report};
use crate::metrics::Snapshot;
use crate::util::{clock, fmt_bytes, fmt_duration, fmt_rate};
use anyhow::Result;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct Doc<'a> {
    generated_unix: f64,
    report: &'a Report,
    snapshot: Snapshot,
}

/// JSON with the process list trimmed to the 25 busiest entries.
pub fn to_json(snap: &Snapshot, report: &Report) -> Result<String> {
    let mut s = snap.clone();
    s.procs.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.pid.cmp(&b.pid)));
    s.procs.truncate(25);
    Ok(serde_json::to_string_pretty(&Doc { generated_unix: snap.ts, report, snapshot: s })?)
}

pub fn to_text(snap: &Snapshot, report: &Report) -> String {
    let mut o = String::new();
    o.push_str(&format!(
        "SysPulse health report - {} ({})\n{}\n\n",
        snap.hostname,
        clock(snap.ts),
        "=".repeat(48)
    ));
    o.push_str(&format!("Overall health: {}/100 [{}]\n", report.score, report.level.label()));
    for s in &report.subs {
        o.push_str(&format!("  {:<8} {:>3}  {}\n", s.sub.label(), s.score, s.level.label()));
    }
    o.push_str("\nChecks\n");
    for c in &report.checks {
        o.push_str(&format!("  [{:<4}] {} - {}\n", c.level.label(), c.title, c.detail));
        if let Some(h) = &c.hint {
            o.push_str(&format!("         hint: {h}\n"));
        }
    }
    let (rx, tx) = snap.net_totals();
    o.push_str(&format!(
        "\nSystem: {} | kernel {} | up {}\nCPU: {} ({} cores) at {:.1}%, load {:.2} {:.2} {:.2}\nMemory: {} used of {} | swap {} of {}\nNetwork: down {} / up {}\n",
        snap.os,
        snap.kernel,
        fmt_duration(snap.uptime),
        snap.cpu_brand,
        snap.cores.len(),
        snap.cpu_total,
        snap.load[0],
        snap.load[1],
        snap.load[2],
        fmt_bytes(snap.mem_used()),
        fmt_bytes(snap.mem_total),
        fmt_bytes(snap.swap_used),
        fmt_bytes(snap.swap_total),
        fmt_rate(rx),
        fmt_rate(tx),
    ));
    for d in snap.disks.iter().filter(|d| d.is_real()) {
        o.push_str(&format!("Disk {}: {:.0}% of {}\n", d.mount, d.used_pct(), fmt_bytes(d.total)));
    }
    let mut procs: Vec<_> = snap.user_procs().collect();
    procs.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.pid.cmp(&b.pid)));
    o.push_str("\nTop processes by CPU\n");
    for p in procs.iter().take(5) {
        o.push_str(&format!("  {:>7}  {:<20} {:>5.1}%  {}\n", p.pid, p.name, p.cpu, fmt_bytes(p.mem)));
    }
    o
}

pub fn to_markdown(snap: &Snapshot, report: &Report) -> String {
    let mut o = format!("# SysPulse report: {}\n\n", snap.hostname);
    o.push_str(&format!("**Overall health: {}/100 ({})**\n\n", report.score, report.level.label()));
    o.push_str("| Subsystem | Score | Status |\n|---|---:|---|\n");
    for s in &report.subs {
        o.push_str(&format!("| {} | {} | {} |\n", s.sub.label(), s.score, s.level.label()));
    }
    o.push_str("\n## Checks\n\n");
    for c in &report.checks {
        o.push_str(&format!("- **{}** {}: {}", c.level.label(), c.title, c.detail));
        if let Some(h) = &c.hint {
            o.push_str(&format!(" _(hint: {h})_"));
        }
        o.push('\n');
    }
    o
}

/// Write `<dir>/syspulse-<unix>.json` and `.md`; returns the JSON path.
pub fn save(dir: Option<&str>, snap: &Snapshot, report: &Report) -> Result<PathBuf> {
    let dir = dir.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    let stem = format!("syspulse-{}", snap.ts as u64);
    let json = dir.join(format!("{stem}.json"));
    std::fs::write(&json, to_json(snap, report)?)?;
    std::fs::write(dir.join(format!("{stem}.md")), to_markdown(snap, report))?;
    Ok(json)
}

/// Append one alert as a JSON line.
pub fn log_alert(path: &Path, alert: &Alert) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(alert)?)?;
    Ok(())
}
