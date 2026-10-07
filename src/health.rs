//! Health engine: turns raw metrics + history into checks, sub-scores, an
//! overall 0-100 score and edge-triggered alerts.

use crate::config::Config;
use crate::history::Histories;
use crate::metrics::Snapshot;
use crate::util::{fmt_bytes, fmt_eta};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Level {
    Ok,
    Warn,
    Crit,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Ok => "OK",
            Level::Warn => "WARN",
            Level::Crit => "CRIT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Sub {
    Cpu,
    Memory,
    Disk,
    Thermal,
    System,
    Network,
    Power,
}

impl Sub {
    pub const ALL: [Sub; 7] = [Sub::Cpu, Sub::Memory, Sub::Disk, Sub::Thermal, Sub::System, Sub::Network, Sub::Power];

    pub fn label(self) -> &'static str {
        match self {
            Sub::Cpu => "CPU",
            Sub::Memory => "Memory",
            Sub::Disk => "Disk",
            Sub::Thermal => "Thermal",
            Sub::System => "System",
            Sub::Network => "Network",
            Sub::Power => "Power",
        }
    }

    fn weight(self) -> f64 {
        match self {
            Sub::Cpu => 0.25,
            Sub::Memory => 0.25,
            Sub::Disk => 0.20,
            Sub::Thermal => 0.12,
            Sub::System => 0.08,
            Sub::Network => 0.05,
            Sub::Power => 0.05,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: String,
    pub sub: Sub,
    pub level: Level,
    pub title: String,
    pub detail: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubScore {
    pub sub: Sub,
    pub score: u8,
    pub level: Level,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub score: u8,
    pub level: Level,
    pub subs: Vec<SubScore>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub ts: f64,
    pub level: Level,
    pub id: String,
    pub title: String,
    pub detail: String,
    pub recovered: bool,
}

pub fn level_for(v: f64, warn: f64, crit: f64) -> Level {
    if v >= crit {
        Level::Crit
    } else if v >= warn {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// Map a "higher is worse" metric to 0..=100: 100->80 up to `warn`,
/// 80->40 between warn and crit, 40->0 between crit and `max`.
pub fn score_high_bad(v: f64, warn: f64, crit: f64, max: f64) -> f64 {
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    let s = if v <= warn {
        100.0 - 20.0 * (v / warn.max(1e-9)).clamp(0.0, 1.0)
    } else if v <= crit {
        80.0 - 40.0 * (v - warn) / (crit - warn).max(1e-9)
    } else {
        40.0 - 40.0 * ((v - crit) / (max - crit).max(1e-9)).clamp(0.0, 1.0)
    };
    s.clamp(0.0, 100.0)
}

/// Stateful so it can fire alerts only on level *transitions*.
#[derive(Debug, Default)]
pub struct Engine {
    prev: HashMap<String, Level>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn evaluate(&mut self, s: &Snapshot, h: &Histories, cfg: &Config) -> (Report, Vec<Alert>) {
        let t = &cfg.thresholds;
        let mut checks: Vec<Check> = Vec::new();
        let mut scores: HashMap<Sub, f64> = HashMap::new();
        let note = |sub: Sub, score: f64, scores: &mut HashMap<Sub, f64>| {
            let e = scores.entry(sub).or_insert(100.0);
            *e = e.min(score);
        };

        let top_cpu = s.top_cpu().map(|p| format!("Top CPU consumer: {} (pid {}, {:.0}%)", p.name, p.pid, p.cpu));
        let top_mem = s.top_mem().map(|p| format!("Top memory consumer: {} (pid {}, {})", p.name, p.pid, fmt_bytes(p.mem)));

        let cpu_avg = h.cpu.mean_over(s.ts, t.sustain_secs.max(1.0)).unwrap_or(s.cpu_total as f64);
        let lvl = level_for(cpu_avg, t.cpu_warn, t.cpu_crit);
        checks.push(Check {
            id: "cpu".into(),
            sub: Sub::Cpu,
            level: lvl,
            title: format!("CPU usage {:.0}%", cpu_avg),
            detail: format!("averaged over {:.0}s (warn {:.0}%, crit {:.0}%)", t.sustain_secs, t.cpu_warn, t.cpu_crit),
            hint: if lvl > Level::Ok { top_cpu.clone() } else { None },
        });
        note(Sub::Cpu, score_high_bad(cpu_avg, t.cpu_warn, t.cpu_crit, 100.0), &mut scores);

        if s.load[1] > 0.0 || s.load[0] > 0.0 {
            let cores = s.cores.len().max(1) as f64;
            let per_core = s.load[1] / cores;
            let lvl = level_for(per_core, t.load_warn, t.load_crit);
            checks.push(Check {
                id: "load".into(),
                sub: Sub::Cpu,
                level: lvl,
                title: format!("Load {:.2} per core (5m)", per_core),
                detail: format!("load average {:.2} / {:.2} / {:.2} on {} cores", s.load[0], s.load[1], s.load[2], s.cores.len()),
                hint: if lvl > Level::Ok { Some("More runnable tasks than cores: the machine is oversubscribed".into()) } else { None },
            });
            note(Sub::Cpu, score_high_bad(per_core, t.load_warn, t.load_crit, t.load_crit * 2.0), &mut scores);
        }

        if let Some((z, sd)) = h.cpu.zscore_latest(30) {
            let now = s.cpu_total as f64;
            let anomalous = z >= 3.5 && z * sd >= 30.0 && now >= 50.0;
            checks.push(Check {
                id: "anomaly_cpu".into(),
                sub: Sub::Cpu,
                level: if anomalous { Level::Warn } else { Level::Ok },
                title: if anomalous { format!("CPU spike: {:.0}% is {:.1} sigma above normal", now, z) } else { "CPU pattern normal".into() },
                detail: format!("z-score {:.1}, stddev {:.1} pts over {} samples", z, sd, h.cpu.len()),
                hint: if anomalous { top_cpu.clone() } else { None },
            });
        }

        let mem = s.mem_pct();
        if s.mem_total > 0 {
            let lvl = level_for(mem, t.mem_warn, t.mem_crit);
            checks.push(Check {
                id: "mem".into(),
                sub: Sub::Memory,
                level: lvl,
                title: format!("Memory {:.0}% used", mem),
                detail: format!("{} available of {}", fmt_bytes(s.mem_avail), fmt_bytes(s.mem_total)),
                hint: if lvl > Level::Ok { top_mem.clone() } else { None },
            });
            note(Sub::Memory, score_high_bad(mem, t.mem_warn, t.mem_crit, 100.0), &mut scores);

            if let Some((z, sd)) = h.mem_pct.zscore_latest(30) {
                let anomalous = z >= 4.0 && z * sd >= 5.0 && mem >= 60.0;
                checks.push(Check {
                    id: "anomaly_mem".into(),
                    sub: Sub::Memory,
                    level: if anomalous { Level::Warn } else { Level::Ok },
                    title: if anomalous { format!("Memory jump: {:.0}% is {:.1} sigma above normal", mem, z) } else { "Memory pattern normal".into() },
                    detail: format!("z-score {:.1}, stddev {:.2} pts", z, sd),
                    hint: if anomalous { top_mem.clone() } else { None },
                });
            }
        }
        if s.swap_total > 0 {
            let sw = s.swap_pct();
            let lvl = level_for(sw, t.swap_warn, t.swap_crit);
            checks.push(Check {
                id: "swap".into(),
                sub: Sub::Memory,
                level: lvl,
                title: format!("Swap {:.0}% used", sw),
                detail: format!("{} of {}", fmt_bytes(s.swap_used), fmt_bytes(s.swap_total)),
                hint: if lvl > Level::Ok { Some("Heavy swapping slows everything down: close apps or add RAM".into()) } else { None },
            });
            note(Sub::Memory, score_high_bad(sw, t.swap_warn, t.swap_crit, 100.0), &mut scores);
        }

        for d in s.disks.iter().filter(|d| d.is_real()) {
            let pct = d.used_pct();
            let lvl = level_for(pct, t.disk_warn, t.disk_crit);
            checks.push(Check {
                id: format!("disk:{}", d.mount),
                sub: Sub::Disk,
                level: lvl,
                title: format!("{} {:.0}% full", d.mount, pct),
                detail: format!("{} free of {}", fmt_bytes(d.avail), fmt_bytes(d.total)),
                hint: if lvl > Level::Ok { Some("Free up space (caches, logs, old downloads) before it fills".into()) } else { None },
            });
            note(Sub::Disk, score_high_bad(pct, t.disk_warn, t.disk_crit, 100.0), &mut scores);
        }
        if let Some(d) = s.primary_disk() {
            if h.disk_used.len() >= 20 && h.disk_used.span() >= 120.0 {
                if let Some(slope) = h.disk_used.slope_since(f64::MIN) {
                    if slope > 1024.0 {
                        let eta = d.avail as f64 / slope;
                        let lvl = if eta < 86_400.0 { Level::Crit } else if eta < 7.0 * 86_400.0 { Level::Warn } else { Level::Ok };
                        checks.push(Check {
                            id: "disk_eta".into(),
                            sub: Sub::Disk,
                            level: lvl,
                            title: format!("{} projected full in ~{}", d.mount, fmt_eta(eta)),
                            detail: format!("growing {}/min (trend over {:.0}s)", fmt_bytes((slope * 60.0) as u64), h.disk_used.span()),
                            hint: if lvl > Level::Ok { Some("Find what is writing: check logs, downloads and build caches".into()) } else { None },
                        });
                    }
                }
            }
        }

        if let Some((label, temp)) = s.max_temp() {
            let temp = temp as f64;
            let lvl = level_for(temp, t.temp_warn, t.temp_crit);
            checks.push(Check {
                id: "temp".into(),
                sub: Sub::Thermal,
                level: lvl,
                title: format!("Hottest sensor {:.0}°C", temp),
                detail: label.to_string(),
                hint: if lvl > Level::Ok { Some("Check airflow, fans and dust; reduce load".into()) } else { None },
            });
            note(Sub::Thermal, score_high_bad(temp, t.temp_warn, t.temp_crit, t.temp_crit + 15.0), &mut scores);
        }

        let z = s.zombies as f64;
        let lvl = level_for(z, 5.0, 50.0);
        checks.push(Check {
            id: "zombies".into(),
            sub: Sub::System,
            level: lvl,
            title: format!("{} zombie process{}", s.zombies, if s.zombies == 1 { "" } else { "es" }),
            detail: format!("{} processes total", s.procs.len()),
            hint: if lvl > Level::Ok { Some("Zombies mean parents are not reaping children; restart the parent".into()) } else { None },
        });
        note(Sub::System, score_high_bad(z, 5.0, 50.0, 200.0), &mut scores);

        if !s.nets.is_empty() {
            let errs: u64 = s.nets.iter().map(|n| n.errors).sum();
            let lvl = if errs > 0 { Level::Warn } else { Level::Ok };
            checks.push(Check {
                id: "net_err".into(),
                sub: Sub::Network,
                level: lvl,
                title: if errs > 0 { format!("{errs} network errors in the last sample") } else { "No network errors".into() },
                detail: format!("{} interfaces", s.nets.len()),
                hint: if errs > 0 { Some("Check cable, driver and link quality".into()) } else { None },
            });
            note(Sub::Network, if errs > 0 { 70.0 } else { 100.0 }, &mut scores);
        }

        if let Some(b) = &s.battery {
            let low = b.discharging();
            let lvl = if low { level_for(t.battery_warn - b.pct as f64, 0.0, t.battery_warn - t.battery_crit) } else { Level::Ok };
            checks.push(Check {
                id: "battery".into(),
                sub: Sub::Power,
                level: lvl,
                title: format!("Battery {:.0}% ({})", b.pct, b.state),
                detail: match b.health {
                    Some(h) => format!("battery health {:.0}% of design capacity", h),
                    None => "capacity data not reported".into(),
                },
                hint: if lvl > Level::Ok { Some("Plug in soon".into()) } else { None },
            });
            note(Sub::Power, match lvl { Level::Ok => 100.0, Level::Warn => 60.0, Level::Crit => 30.0 }, &mut scores);
        }

        let mut subs = Vec::new();
        let (mut wsum, mut acc) = (0.0, 0.0);
        for sub in Sub::ALL {
            if let Some(&sc) = scores.get(&sub) {
                let level = checks.iter().filter(|c| c.sub == sub).map(|c| c.level).max().unwrap_or(Level::Ok);
                subs.push(SubScore { sub, score: sc.round() as u8, level });
                wsum += sub.weight();
                acc += sub.weight() * sc;
            }
        }
        let mut score = if wsum > 0.0 { acc / wsum } else { 100.0 };
        let worst = checks.iter().map(|c| c.level).max().unwrap_or(Level::Ok);
        match worst {
            Level::Crit => score = score.min(59.0),
            Level::Warn => score = score.min(84.0),
            Level::Ok => {}
        }
        checks.sort_by(|a, b| b.level.cmp(&a.level));

        let mut alerts = Vec::new();
        for c in &checks {
            let prev = self.prev.get(&c.id).copied().unwrap_or(Level::Ok);
            if c.level != prev {
                if c.level > prev {
                    alerts.push(Alert { ts: s.ts, level: c.level, id: c.id.clone(), title: c.title.clone(), detail: c.detail.clone(), recovered: false });
                } else if c.level == Level::Ok {
                    alerts.push(Alert { ts: s.ts, level: Level::Ok, id: c.id.clone(), title: format!("Recovered: {}", c.title), detail: c.detail.clone(), recovered: true });
                }
                self.prev.insert(c.id.clone(), c.level);
            }
        }

        (Report { score: score.round() as u8, level: worst, subs, checks }, alerts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &Snapshot) -> (Report, Vec<Alert>, Engine) {
        let cfg = Config::default();
        let mut h = Histories::new(900.0);
        h.ingest(s);
        let mut e = Engine::new();
        let (r, a) = e.evaluate(s, &h, &cfg);
        (r, a, e)
    }

    #[test]
    fn score_is_monotonic_and_bounded() {
        let mut prev = 101.0;
        for v in (0..=100).map(|v| v as f64) {
            let s = score_high_bad(v, 80.0, 95.0, 100.0);
            assert!((0.0..=100.0).contains(&s));
            assert!(s <= prev + 1e-9, "not monotonic at {v}");
            prev = s;
        }
        assert_eq!(score_high_bad(0.0, 80.0, 95.0, 100.0), 100.0);
        assert!((score_high_bad(80.0, 80.0, 95.0, 100.0) - 80.0).abs() < 1e-9);
        assert!((score_high_bad(95.0, 80.0, 95.0, 100.0) - 40.0).abs() < 1e-9);
    }

    #[test]
    fn healthy_machine_scores_high() {
        let (r, alerts, _) = run(&Snapshot::demo());
        assert!(r.score >= 80, "score {}", r.score);
        assert_eq!(r.level, Level::Ok);
        assert!(alerts.is_empty());
    }

    #[test]
    fn full_disk_is_critical_and_caps_score() {
        let mut s = Snapshot::demo();
        s.disks[0].avail = 10 << 30; // 98% full
        let (r, alerts, _) = run(&s);
        assert_eq!(r.level, Level::Crit);
        assert!(r.score <= 59);
        assert!(alerts.iter().any(|a| a.id == "disk:/" && a.level == Level::Crit));
    }

    #[test]
    fn pseudo_filesystems_are_ignored() {
        let mut s = Snapshot::demo();
        s.disks[1].avail = 0; // tmpfs 100% full must not alert
        let (r, _, _) = run(&s);
        assert!(!r.checks.iter().any(|c| c.id == "disk:/run"));
    }

    #[test]
    fn alerts_fire_on_transition_and_recover() {
        let cfg = Config::default();
        let mut h = Histories::new(900.0);
        let mut e = Engine::new();
        let mut s = Snapshot::demo();
        s.mem_avail = 1 << 30; // ~94% used -> warn
        h.ingest(&s);
        let (_, a1) = e.evaluate(&s, &h, &cfg);
        assert!(a1.iter().any(|a| a.id == "mem" && a.level == Level::Warn));
        s.ts += 1.0;
        h.ingest(&s);
        let (_, a2) = e.evaluate(&s, &h, &cfg);
        assert!(a2.iter().all(|a| a.id != "mem"), "must not re-alert while unchanged");
        s.mem_avail = 12 << 30;
        s.ts += 1.0;
        h.ingest(&s);
        let (_, a3) = e.evaluate(&s, &h, &cfg);
        assert!(a3.iter().any(|a| a.id == "mem" && a.recovered));
    }

    #[test]
    fn disk_forecast_uses_growth_trend() {
        let cfg = Config::default();
        let mut h = Histories::new(3600.0);
        let mut s = Snapshot::demo();
        let mut e = Engine::new();
        let mut last = None;
        for i in 0..200 {
            s.ts = 1_700_000_000.0 + i as f64 * 2.0;
            s.disks[0].avail = (120u64 << 30) - i * 50_000_000; // ~25 MB/s growth
            h.ingest(&s);
            last = Some(e.evaluate(&s, &h, &cfg).0);
        }
        let r = last.unwrap();
        let eta = r.checks.iter().find(|c| c.id == "disk_eta").expect("forecast present");
        assert!(eta.level >= Level::Warn);
    }

    #[test]
    fn cpu_spike_flags_anomaly() {
        let cfg = Config::default();
        let mut h = Histories::new(3600.0);
        let mut e = Engine::new();
        let mut s = Snapshot::demo();
        for i in 0..40 {
            s.ts = 1_700_000_000.0 + i as f64;
            s.cpu_total = 8.0 + (i % 4) as f32;
            h.ingest(&s);
            e.evaluate(&s, &h, &cfg);
        }
        s.ts += 1.0;
        s.cpu_total = 97.0;
        h.ingest(&s);
        let (r, _) = e.evaluate(&s, &h, &cfg);
        let c = r.checks.iter().find(|c| c.id == "anomaly_cpu").unwrap();
        assert_eq!(c.level, Level::Warn);
    }
}
