//! Time-series storage and the maths on top of it: window statistics,
//! percentiles, linear-regression trends and z-score anomaly detection.

use crate::metrics::Snapshot;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub p95: f64,
    pub n: usize,
}

/// A bounded time series of `(unix_seconds, value)`; old points are pruned by age.
#[derive(Debug, Clone)]
pub struct Series {
    data: VecDeque<(f64, f64)>,
    max_age: f64,
}

impl Series {
    pub fn new(max_age_secs: f64) -> Self {
        Self { data: VecDeque::new(), max_age: max_age_secs }
    }

    pub fn push(&mut self, t: f64, v: f64) {
        let v = if v.is_finite() { v } else { 0.0 };
        self.data.push_back((t, v));
        let cutoff = t - self.max_age;
        while matches!(self.data.front(), Some(&(t0, _)) if t0 < cutoff) {
            self.data.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.data.clear();
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn last(&self) -> Option<f64> {
        self.data.back().map(|&(_, v)| v)
    }

    fn window(&self, now: f64, secs: f64) -> impl Iterator<Item = &(f64, f64)> {
        let start = now - secs;
        self.data.iter().filter(move |(t, _)| *t >= start)
    }

    /// Chart-ready points with x = seconds relative to `now` (<= 0).
    pub fn points(&self, now: f64, secs: f64) -> Vec<(f64, f64)> {
        self.window(now, secs).map(|&(t, v)| (t - now, v)).collect()
    }

    pub fn values(&self, now: f64, secs: f64) -> Vec<f64> {
        self.window(now, secs).map(|&(_, v)| v).collect()
    }

    pub fn stats(&self, now: f64, secs: f64) -> Option<Stats> {
        let mut vals = self.values(now, secs);
        if vals.is_empty() {
            return None;
        }
        let n = vals.len();
        let avg = vals.iter().sum::<f64>() / n as f64;
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = (((n as f64) * 0.95).ceil() as usize).clamp(1, n) - 1;
        Some(Stats { min: vals[0], max: vals[n - 1], avg, p95: vals[idx], n })
    }

    pub fn mean_over(&self, now: f64, secs: f64) -> Option<f64> {
        let v = self.values(now, secs);
        if v.is_empty() { None } else { Some(v.iter().sum::<f64>() / v.len() as f64) }
    }

    /// Last `n` values scaled by `scale` into u64 (for `Sparkline`).
    pub fn tail_u64(&self, n: usize, scale: f64) -> Vec<u64> {
        let skip = self.data.len().saturating_sub(n);
        self.data.iter().skip(skip).map(|&(_, v)| (v * scale).max(0.0) as u64).collect()
    }

    /// Time span covered by the stored points, in seconds.
    pub fn span(&self) -> f64 {
        match (self.data.front(), self.data.back()) {
            (Some(a), Some(b)) => b.0 - a.0,
            _ => 0.0,
        }
    }

    /// Least-squares slope (units per second) over points with t >= `since`.
    pub fn slope_since(&self, since: f64) -> Option<f64> {
        let pts: Vec<(f64, f64)> = self.data.iter().filter(|(t, _)| *t >= since).copied().collect();
        let n = pts.len();
        if n < 2 {
            return None;
        }
        let t0 = pts[0].0;
        let nf = n as f64;
        let (mut st, mut sv, mut stt, mut stv) = (0.0, 0.0, 0.0, 0.0);
        for &(t, v) in &pts {
            let x = t - t0;
            st += x;
            sv += v;
            stt += x * x;
            stv += x * v;
        }
        let denom = nf * stt - st * st;
        if denom.abs() < 1e-9 {
            return None;
        }
        Some((nf * stv - st * sv) / denom)
    }

    /// z-score of the latest sample against all earlier samples.
    /// Returns `(z, stddev)`; `None` when there is too little or flat data.
    pub fn zscore_latest(&self, min_n: usize) -> Option<(f64, f64)> {
        let n = self.data.len();
        if n < min_n.max(3) {
            return None;
        }
        let last = self.data.back()?.1;
        let prior: Vec<f64> = self.data.iter().take(n - 1).map(|&(_, v)| v).collect();
        let m = prior.iter().sum::<f64>() / prior.len() as f64;
        let var = prior.iter().map(|v| (v - m).powi(2)).sum::<f64>() / prior.len() as f64;
        let sd = var.sqrt();
        if sd < 1e-9 {
            return None;
        }
        Some(((last - m) / sd, sd))
    }
}

/// All the series the UI and health engine need.
#[derive(Debug, Clone)]
pub struct Histories {
    max_age: f64,
    pub cpu: Series,
    pub cores: Vec<Series>,
    pub load1: Series,
    pub mem_pct: Series,
    pub mem_used: Series,
    pub mem_cached: Series,
    pub swap_pct: Series,
    pub rx: Series,
    pub tx: Series,
    pub disk_r: Series,
    pub disk_w: Series,
    pub temp: Series,
    /// Used bytes of the primary disk (for fill-time forecasting).
    pub disk_used: Series,
    disk_primary: Option<String>,
}

impl Histories {
    pub fn new(max_age_secs: f64) -> Self {
        let s = || Series::new(max_age_secs);
        Self {
            max_age: max_age_secs,
            cpu: s(),
            cores: Vec::new(),
            load1: s(),
            mem_pct: s(),
            mem_used: s(),
            mem_cached: s(),
            swap_pct: s(),
            rx: s(),
            tx: s(),
            disk_r: s(),
            disk_w: s(),
            temp: s(),
            disk_used: s(),
            disk_primary: None,
        }
    }

    pub fn ingest(&mut self, snap: &Snapshot) {
        let t = snap.ts;
        self.cpu.push(t, snap.cpu_total as f64);
        if self.cores.len() != snap.cores.len() {
            self.cores = snap.cores.iter().map(|_| Series::new(self.max_age)).collect();
        }
        for (series, v) in self.cores.iter_mut().zip(&snap.cores) {
            series.push(t, *v as f64);
        }
        self.load1.push(t, snap.load[0]);
        self.mem_pct.push(t, snap.mem_pct());
        self.mem_used.push(t, snap.mem_used() as f64);
        self.mem_cached.push(t, snap.mem_cached as f64);
        self.swap_pct.push(t, snap.swap_pct());
        let (rx, tx) = snap.net_totals();
        self.rx.push(t, rx);
        self.tx.push(t, tx);
        if let Some(io) = &snap.disk_io {
            self.disk_r.push(t, io.read_bps);
            self.disk_w.push(t, io.write_bps);
        }
        if let Some(max) = snap.max_temp() {
            self.temp.push(t, max.1 as f64);
        }
        match snap.primary_disk() {
            Some(d) => {
                if self.disk_primary.as_deref() != Some(d.mount.as_str()) {
                    self.disk_used.clear();
                    self.disk_primary = Some(d.mount.clone());
                }
                self.disk_used.push(t, d.used() as f64);
            }
            None => self.disk_primary = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(vals: &[f64]) -> Series {
        let mut s = Series::new(1000.0);
        for (i, v) in vals.iter().enumerate() {
            s.push(i as f64, *v);
        }
        s
    }

    #[test]
    fn prunes_by_age() {
        let mut s = Series::new(10.0);
        for i in 0..100 {
            s.push(i as f64, 1.0);
        }
        assert!(s.len() <= 11);
    }

    #[test]
    fn stats_and_p95() {
        let s = series(&(1..=100).map(|v| v as f64).collect::<Vec<_>>());
        let st = s.stats(99.0, 1000.0).unwrap();
        assert_eq!(st.min, 1.0);
        assert_eq!(st.max, 100.0);
        assert!((st.avg - 50.5).abs() < 1e-9);
        assert_eq!(st.p95, 95.0);
    }

    #[test]
    fn window_limits_points() {
        let s = series(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(s.values(4.0, 2.0), vec![3.0, 4.0, 5.0]);
        let pts = s.points(4.0, 1.0);
        assert_eq!(pts, vec![(-1.0, 4.0), (0.0, 5.0)]);
    }

    #[test]
    fn slope_is_linear_fit() {
        let s = series(&[0.0, 2.0, 4.0, 6.0, 8.0]);
        assert!((s.slope_since(0.0).unwrap() - 2.0).abs() < 1e-9);
        let flat = series(&[3.0; 5]);
        assert!(flat.slope_since(0.0).unwrap().abs() < 1e-9);
        assert!(series(&[1.0]).slope_since(0.0).is_none());
    }

    #[test]
    fn zscore_detects_spike() {
        let mut vals: Vec<f64> = (0..40).map(|i| 10.0 + (i % 3) as f64).collect();
        vals.push(90.0);
        let (z, sd) = series(&vals).zscore_latest(30).unwrap();
        assert!(z > 10.0);
        assert!(sd > 0.0);
        assert!(series(&[5.0; 40]).zscore_latest(30).is_none());
    }
}
