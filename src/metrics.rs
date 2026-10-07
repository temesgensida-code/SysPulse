//! Data collection. A `Collector` owns the `sysinfo` handles and runs on its
//! own thread, sending `Snapshot`s to the UI thread over a channel.

use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Components, Disks, Networks, Pid, ProcessRefreshKind, ProcessStatus, Signal, System, ThreadKind, UpdateKind, Users};

#[derive(Debug, Clone, Serialize, Default)]
pub struct DiskInfo {
    pub name: String,
    pub mount: String,
    pub fs: String,
    pub kind: String,
    pub total: u64,
    pub avail: u64,
}

impl DiskInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.avail)
    }
    pub fn used_pct(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.used() as f64 / self.total as f64 * 100.0 }
    }
    /// Filters out pseudo / read-only image filesystems that would always look "full".
    pub fn is_real(&self) -> bool {
        const PSEUDO: [&str; 9] =
            ["tmpfs", "devtmpfs", "squashfs", "overlay", "efivarfs", "proc", "sysfs", "ramfs", "iso9660"];
        self.total > 0 && !PSEUDO.contains(&self.fs.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct NetInfo {
    pub name: String,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    /// Errors seen since the previous sample.
    pub errors: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub user: String,
    /// Percent of *total* CPU capacity (0..=100).
    pub cpu: f32,
    pub mem: u64,
    pub status: String,
    pub run_time: u64,
    pub zombie: bool,
    /// Kernel thread (kworker, kthreadd...); hidden in the UI unless toggled with `K`.
    pub kernel: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TempInfo {
    pub label: String,
    pub temp: f32,
    pub critical: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Battery {
    pub pct: f32,
    pub state: String,
    /// Full-charge capacity relative to design capacity, when reported.
    pub health: Option<f32>,
}

impl Battery {
    pub fn discharging(&self) -> bool {
        self.state.to_ascii_lowercase().contains("discharging")
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DiskIo {
    pub read_bps: f64,
    pub write_bps: f64,
    pub iops: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Snapshot {
    pub ts: f64,
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub uptime: u64,
    pub cpu_brand: String,
    pub cpu_total: f32,
    pub cores: Vec<f32>,
    pub freqs: Vec<u64>,
    pub load: [f64; 3],
    pub mem_total: u64,
    pub mem_avail: u64,
    pub mem_free: u64,
    pub mem_cached: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub disks: Vec<DiskInfo>,
    pub disk_io: Option<DiskIo>,
    pub nets: Vec<NetInfo>,
    pub procs: Vec<ProcInfo>,
    pub temps: Vec<TempInfo>,
    pub battery: Option<Battery>,
    pub zombies: usize,
}

impl Snapshot {
    pub fn mem_used(&self) -> u64 {
        self.mem_total.saturating_sub(self.mem_avail)
    }
    pub fn mem_pct(&self) -> f64 {
        if self.mem_total == 0 { 0.0 } else { self.mem_used() as f64 / self.mem_total as f64 * 100.0 }
    }
    pub fn swap_pct(&self) -> f64 {
        if self.swap_total == 0 { 0.0 } else { self.swap_used as f64 / self.swap_total as f64 * 100.0 }
    }
    /// Total (rx, tx) bytes/s across interfaces, excluding loopback.
    pub fn net_totals(&self) -> (f64, f64) {
        self.nets
            .iter()
            .filter(|n| n.name != "lo")
            .fold((0.0, 0.0), |(r, t), n| (r + n.rx_rate, t + n.tx_rate))
    }
    pub fn max_temp(&self) -> Option<(&str, f32)> {
        self.temps
            .iter()
            .filter(|t| t.temp.is_finite() && t.temp > 0.0 && t.temp < 150.0)
            .max_by(|a, b| a.temp.partial_cmp(&b.temp).unwrap_or(std::cmp::Ordering::Equal))
            .map(|t| (t.label.as_str(), t.temp))
    }
    /// Root filesystem if present, otherwise the largest real filesystem.
    pub fn primary_disk(&self) -> Option<&DiskInfo> {
        let real = || self.disks.iter().filter(|d| d.is_real());
        real().find(|d| d.mount == "/").or_else(|| real().max_by_key(|d| d.total))
    }
    /// Real (non-kernel-thread) processes; used for "top consumer" rankings.
    pub fn user_procs(&self) -> impl Iterator<Item = &ProcInfo> {
        self.procs.iter().filter(|p| !p.kernel)
    }
    pub fn top_cpu(&self) -> Option<&ProcInfo> {
        self.user_procs().max_by(|a, b| a.cpu.partial_cmp(&b.cpu).unwrap_or(std::cmp::Ordering::Equal))
    }
    pub fn top_mem(&self) -> Option<&ProcInfo> {
        self.user_procs().max_by_key(|p| p.mem)
    }
}

pub enum Msg {
    Snapshot(Box<Snapshot>),
    Notice(String),
}

pub enum Cmd {
    Kill(u32),
    Refresh,
    Quit,
}

pub struct Collector {
    sys: System,
    disks: Disks,
    nets: Networks,
    comps: Components,
    users: Users,
    uid_cache: HashMap<String, String>,
    last: Instant,
    last_io: Option<(Instant, u64, u64, u64)>,
    ncpu: usize,
    tick: u64,
    hostname: String,
    os: String,
    kernel: String,
}

impl Collector {
    /// CPU usage plus the owning user (sysinfo skips the user unless asked).
    fn proc_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::new().with_cpu().with_user(UpdateKind::OnlyIfNotSet)
    }

    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu();
        sys.refresh_memory();
        sys.refresh_processes_specifics(Self::proc_kind());
        let ncpu = sys.cpus().len().max(1);
        Self {
            sys,
            disks: Disks::new_with_refreshed_list(),
            nets: Networks::new_with_refreshed_list(),
            comps: Components::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            uid_cache: HashMap::new(),
            last: Instant::now(),
            last_io: None,
            ncpu,
            tick: 0,
            hostname: System::host_name().unwrap_or_else(|| "unknown".into()),
            os: System::long_os_version().or_else(System::name).unwrap_or_else(|| "unknown".into()),
            kernel: System::kernel_version().unwrap_or_default(),
        }
    }

    pub fn collect(&mut self) -> Snapshot {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64().max(0.05);
        self.last = now;
        self.tick += 1;

        self.sys.refresh_cpu();
        self.sys.refresh_memory();
        self.sys.refresh_processes_specifics(Self::proc_kind());
        if self.tick % 20 == 0 {
            self.disks.refresh_list();
            self.nets.refresh_list();
            self.users.refresh_list();
            self.uid_cache.clear();
        } else {
            self.disks.refresh();
            self.nets.refresh();
        }
        self.comps.refresh();

        let cpus = self.sys.cpus();
        let cores: Vec<f32> = cpus.iter().map(|c| c.cpu_usage()).collect();
        let freqs: Vec<u64> = cpus.iter().map(|c| c.frequency()).collect();
        let cpu_brand = cpus.first().map(|c| c.brand().trim().to_string()).unwrap_or_default();
        let la = System::load_average();

        let mem_total = self.sys.total_memory();
        let mem_avail = self.sys.available_memory().min(mem_total);
        let meminfo = read_meminfo();
        let mem_free = meminfo.get("MemFree").copied().unwrap_or(mem_avail).min(mem_avail);
        let mem_cached = mem_avail.saturating_sub(mem_free);

        let disks: Vec<DiskInfo> = self
            .disks
            .list()
            .iter()
            .map(|d| DiskInfo {
                name: d.name().to_string_lossy().to_string(),
                mount: d.mount_point().to_string_lossy().to_string(),
                fs: d.file_system().to_string_lossy().to_string(),
                kind: d.kind().to_string(),
                total: d.total_space(),
                avail: d.available_space(),
            })
            .collect();

        let mut nets: Vec<NetInfo> = self
            .nets
            .iter()
            .map(|(name, d)| NetInfo {
                name: name.clone(),
                rx_rate: d.received() as f64 / dt,
                tx_rate: d.transmitted() as f64 / dt,
                rx_total: d.total_received(),
                tx_total: d.total_transmitted(),
                errors: d.errors_on_received() + d.errors_on_transmitted(),
            })
            .collect();
        nets.sort_by(|a, b| {
            (b.rx_rate + b.tx_rate)
                .partial_cmp(&(a.rx_rate + a.tx_rate))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });

        let ncpu = self.ncpu as f32;
        let mut zombies = 0usize;
        let mut procs = Vec::with_capacity(self.sys.processes().len() / 2);
        for (pid, p) in self.sys.processes() {
            // Linux exposes every userland thread as a "process" too; drop those or
            // counts and shared memory (RSS) get double-counted. Kernel threads are
            // real PIDs, so they are kept and flagged.
            let kernel = match p.thread_kind() {
                Some(ThreadKind::Userland) => continue,
                Some(ThreadKind::Kernel) => true,
                None => false,
            };
            let zombie = p.status() == ProcessStatus::Zombie;
            if zombie {
                zombies += 1;
            }
            let user = match p.user_id() {
                Some(uid) => {
                    let key = uid.to_string();
                    let users = &self.users;
                    self.uid_cache
                        .entry(key.clone())
                        .or_insert_with(|| {
                            users.get_user_by_id(uid).map(|u| u.name().to_string()).unwrap_or(key)
                        })
                        .clone()
                }
                None => String::new(),
            };
            procs.push(ProcInfo {
                pid: pid.as_u32(),
                name: p.name().to_string(),
                user,
                cpu: (p.cpu_usage() / ncpu).clamp(0.0, 100.0),
                mem: p.memory(),
                status: p.status().to_string(),
                run_time: p.run_time(),
                zombie,
                kernel,
            });
        }

        let temps: Vec<TempInfo> = self
            .comps
            .iter()
            .map(|c| TempInfo { label: c.label().to_string(), temp: c.temperature(), critical: c.critical() })
            .collect();

        let disk_io = self.disk_io(now);

        Snapshot {
            ts: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            hostname: self.hostname.clone(),
            os: self.os.clone(),
            kernel: self.kernel.clone(),
            uptime: System::uptime(),
            cpu_brand,
            cpu_total: self.sys.global_cpu_info().cpu_usage(),
            cores,
            freqs,
            load: [la.one, la.five, la.fifteen],
            mem_total,
            mem_avail,
            mem_free,
            mem_cached,
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            disks,
            disk_io,
            nets,
            procs,
            temps,
            battery: read_battery(),
            zombies,
        }
    }

    fn disk_io(&mut self, now: Instant) -> Option<DiskIo> {
        let (r, w, io) = read_diskstats()?;
        let out = match self.last_io {
            Some((t0, r0, w0, io0)) => {
                let dt = now.duration_since(t0).as_secs_f64().max(0.05);
                DiskIo {
                    read_bps: r.saturating_sub(r0) as f64 / dt,
                    write_bps: w.saturating_sub(w0) as f64 / dt,
                    iops: io.saturating_sub(io0) as f64 / dt,
                }
            }
            None => DiskIo::default(),
        };
        self.last_io = Some((now, r, w, io));
        Some(out)
    }

    fn kill(&mut self, pid: u32) -> String {
        match self.sys.process(Pid::from_u32(pid)) {
            None => format!("Process {pid} no longer exists"),
            Some(p) => {
                let name = p.name().to_string();
                match p.kill_with(Signal::Term) {
                    Some(true) => format!("Sent SIGTERM to {name} ({pid})"),
                    Some(false) => format!("Could not signal {name} ({pid}) - permission denied?"),
                    None => format!("Signals are not supported on this platform ({name})"),
                }
            }
        }
    }

    /// Collector thread body. Sleeps on the command channel so kill requests
    /// and interval changes are handled immediately.
    pub fn run(mut self, interval: Arc<AtomicU64>, tx: Sender<Msg>, rx: Receiver<Cmd>) {
        thread::sleep(Duration::from_millis(300));
        loop {
            let snap = self.collect();
            if tx.send(Msg::Snapshot(Box::new(snap))).is_err() {
                return;
            }
            let deadline = Instant::now() + Duration::from_millis(interval.load(Ordering::Relaxed));
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match rx.recv_timeout(deadline - now) {
                    Ok(Cmd::Kill(pid)) => {
                        let msg = self.kill(pid);
                        let _ = tx.send(Msg::Notice(msg));
                    }
                    Ok(Cmd::Refresh) | Err(RecvTimeoutError::Timeout) => break,
                    Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }
}

fn read_meminfo() -> HashMap<String, u64> {
    let mut m = HashMap::new();
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            if let Some((k, rest)) = line.split_once(':') {
                if let Some(kb) = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()) {
                    m.insert(k.to_string(), kb * 1024);
                }
            }
        }
    }
    m
}

/// Sum of (read bytes, written bytes, completed IOs) over whole block devices.
fn read_diskstats() -> Option<(u64, u64, u64)> {
    let text = std::fs::read_to_string("/proc/diskstats").ok()?;
    let (mut r, mut w, mut io) = (0u64, 0u64, 0u64);
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 {
            continue;
        }
        let name = f[2];
        const SKIP: [&str; 6] = ["loop", "ram", "dm-", "zram", "sr", "md"];
        if SKIP.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        if !Path::new("/sys/block").join(name).exists() {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        r += n(5) * 512;
        w += n(9) * 512;
        io += n(3) + n(7);
    }
    Some((r, w, io))
}

fn read_num(p: &Path) -> Option<f64> {
    std::fs::read_to_string(p).ok()?.trim().parse::<f64>().ok()
}

fn read_battery() -> Option<Battery> {
    for entry in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("BAT") {
            continue;
        }
        let p = entry.path();
        let pct = read_num(&p.join("capacity"))? as f32;
        let state = std::fs::read_to_string(p.join("status")).map(|s| s.trim().to_string()).unwrap_or_default();
        let ratio = |full: &str, design: &str| -> Option<f32> {
            let f = read_num(&p.join(full))?;
            let d = read_num(&p.join(design))?;
            if d > 0.0 { Some((f / d * 100.0) as f32) } else { None }
        };
        let health = ratio("energy_full", "energy_full_design").or_else(|| ratio("charge_full", "charge_full_design"));
        return Some(Battery { pct, state, health });
    }
    None
}

#[cfg(test)]
impl Snapshot {
    /// Deterministic fake data for UI and health tests.
    pub fn demo() -> Snapshot {
        Snapshot {
            ts: 1_700_000_000.0,
            hostname: "demo-host".into(),
            os: "Demo Linux 1.0".into(),
            kernel: "6.1.0".into(),
            uptime: 93_784,
            cpu_brand: "Demo CPU @ 3.0GHz".into(),
            cpu_total: 42.0,
            cores: vec![10.0, 55.0, 90.0, 20.0],
            freqs: vec![3000, 3100, 2900, 3000],
            load: [0.9, 1.1, 1.0],
            mem_total: 16 << 30,
            mem_avail: 6 << 30,
            mem_free: 2 << 30,
            mem_cached: 4 << 30,
            swap_total: 4 << 30,
            swap_used: 1 << 30,
            disks: vec![
                DiskInfo { name: "/dev/sda1".into(), mount: "/".into(), fs: "ext4".into(), kind: "SSD".into(), total: 500 << 30, avail: 120 << 30 },
                DiskInfo { name: "tmpfs".into(), mount: "/run".into(), fs: "tmpfs".into(), kind: "Unknown".into(), total: 1 << 30, avail: 1 << 30 },
            ],
            disk_io: Some(DiskIo { read_bps: 5e6, write_bps: 2e6, iops: 300.0 }),
            nets: vec![
                NetInfo { name: "eth0".into(), rx_rate: 1.5e6, tx_rate: 2e5, rx_total: 5 << 30, tx_total: 1 << 30, errors: 0 },
                NetInfo { name: "lo".into(), rx_rate: 10.0, tx_rate: 10.0, ..Default::default() },
            ],
            procs: (0..40)
                .map(|i| ProcInfo {
                    pid: 100 + i,
                    name: format!("proc-{i}"),
                    user: if i % 2 == 0 { "root".into() } else { "temesgen".into() },
                    cpu: (i as f32 * 1.7) % 30.0,
                    mem: (i as u64 + 1) * (50 << 20),
                    status: "Sleeping".into(),
                    run_time: 1000 * (i as u64 + 1),
                    zombie: false,
                    kernel: false,
                })
                .collect(),
            temps: vec![TempInfo { label: "CPU package".into(), temp: 61.0, critical: Some(100.0) }],
            battery: Some(Battery { pct: 76.0, state: "Discharging".into(), health: Some(91.0) }),
            zombies: 0,
        }
    }
}
