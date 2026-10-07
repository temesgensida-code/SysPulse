//! Configuration: thresholds, refresh rate, theme. Loaded from TOML.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    pub cpu_warn: f64,
    pub cpu_crit: f64,
    pub mem_warn: f64,
    pub mem_crit: f64,
    pub swap_warn: f64,
    pub swap_crit: f64,
    pub disk_warn: f64,
    pub disk_crit: f64,
    pub temp_warn: f64,
    pub temp_crit: f64,
    /// 5-minute load average divided by core count.
    pub load_warn: f64,
    pub load_crit: f64,
    pub battery_warn: f64,
    pub battery_crit: f64,
    /// CPU must stay above a threshold this long (seconds) before it alerts.
    pub sustain_secs: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            cpu_warn: 80.0,
            cpu_crit: 95.0,
            mem_warn: 85.0,
            mem_crit: 95.0,
            swap_warn: 50.0,
            swap_crit: 80.0,
            disk_warn: 85.0,
            disk_crit: 95.0,
            temp_warn: 75.0,
            temp_crit: 90.0,
            load_warn: 1.5,
            load_crit: 3.0,
            battery_warn: 20.0,
            battery_crit: 10.0,
            sustain_secs: 15.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Sampling interval in milliseconds (250..=10000).
    pub refresh_ms: u64,
    /// How much history to keep in memory, in seconds.
    pub history_secs: u64,
    /// Initial chart window in seconds.
    pub window_secs: u64,
    /// "dark", "light" or "colorblind".
    pub theme: String,
    /// Append alert events (JSON lines) to this file.
    pub log_file: Option<String>,
    /// Directory for exported reports (defaults to the current directory).
    pub export_dir: Option<String>,
    pub thresholds: Thresholds,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_ms: 1000,
            history_secs: 900,
            window_secs: 300,
            theme: "dark".into(),
            log_file: None,
            export_dir: None,
            thresholds: Thresholds::default(),
        }
    }
}

impl Config {
    /// Clamp values into sane ranges so a bad config can't break the UI.
    pub fn sanitize(&mut self) {
        self.refresh_ms = self.refresh_ms.clamp(250, 10_000);
        self.history_secs = self.history_secs.clamp(60, 24 * 3600);
        self.window_secs = self.window_secs.clamp(30, self.history_secs);
        let t = &mut self.thresholds;
        if t.cpu_warn >= t.cpu_crit { t.cpu_warn = t.cpu_crit - 1.0; }
        if t.mem_warn >= t.mem_crit { t.mem_warn = t.mem_crit - 1.0; }
        if t.swap_warn >= t.swap_crit { t.swap_warn = t.swap_crit - 1.0; }
        if t.disk_warn >= t.disk_crit { t.disk_warn = t.disk_crit - 1.0; }
        if t.temp_warn >= t.temp_crit { t.temp_warn = t.temp_crit - 1.0; }
        if t.load_warn >= t.load_crit { t.load_warn = t.load_crit - 0.5; }
        t.sustain_secs = t.sustain_secs.max(0.0);
    }
}

pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("syspulse").join("config.toml"))
}

/// Load config. An explicit path must exist; the default path is optional.
pub fn load(explicit: Option<&Path>) -> Result<Config> {
    let (path, required) = match explicit {
        Some(p) => (Some(p.to_path_buf()), true),
        None => (default_path(), false),
    };
    let mut cfg = match path {
        Some(p) if p.exists() => {
            let text = std::fs::read_to_string(&p)
                .with_context(|| format!("reading config {}", p.display()))?;
            toml::from_str::<Config>(&text)
                .with_context(|| format!("parsing config {}", p.display()))?
        }
        Some(p) if required => anyhow::bail!("config file not found: {}", p.display()),
        _ => Config::default(),
    };
    cfg.sanitize();
    Ok(cfg)
}

pub fn default_toml() -> String {
    toml::to_string_pretty(&Config::default()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_toml_uses_defaults() {
        let cfg: Config = toml::from_str("refresh_ms = 500\n[thresholds]\ncpu_warn = 70.0\n").unwrap();
        assert_eq!(cfg.refresh_ms, 500);
        assert_eq!(cfg.thresholds.cpu_warn, 70.0);
        assert_eq!(cfg.thresholds.cpu_crit, 95.0);
    }

    #[test]
    fn sanitize_clamps() {
        let mut cfg = Config { refresh_ms: 5, window_secs: 99999, ..Config::default() };
        cfg.thresholds.cpu_warn = 99.0;
        cfg.sanitize();
        assert_eq!(cfg.refresh_ms, 250);
        assert!(cfg.window_secs <= cfg.history_secs);
        assert!(cfg.thresholds.cpu_warn < cfg.thresholds.cpu_crit);
    }

    #[test]
    fn default_roundtrips() {
        let text = default_toml();
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg.refresh_ms, 1000);
    }
}
