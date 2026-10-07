//! Application state and input handling (no rendering here).

use crate::config::Config;
use crate::export;
use crate::health::{Alert, Engine, Report};
use crate::history::Histories;
use crate::metrics::{Cmd, Msg, ProcInfo, Snapshot};
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Cpu,
    Memory,
    Disk,
    Network,
    Processes,
    Alerts,
}

impl Tab {
    pub const ALL: [Tab; 7] = [Tab::Overview, Tab::Cpu, Tab::Memory, Tab::Disk, Tab::Network, Tab::Processes, Tab::Alerts];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Cpu => "CPU",
            Tab::Memory => "Memory",
            Tab::Disk => "Disk",
            Tab::Network => "Network",
            Tab::Processes => "Processes",
            Tab::Alerts => "Alerts",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    ConfirmKill(u32, String),
    Detail(u32),
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Cpu,
    Mem,
    Pid,
    Name,
}

pub const INTERVALS_MS: [u64; 6] = [250, 500, 1000, 2000, 5000, 10_000];
pub const WINDOWS_SECS: [u64; 5] = [60, 300, 900, 1800, 3600];
const MAX_ALERTS: usize = 300;

pub struct App {
    pub cfg: Config,
    pub theme: Theme,
    pub tab: Tab,
    pub mode: Mode,
    pub snap: Option<Snapshot>,
    pub hist: Histories,
    pub engine: Engine,
    pub report: Option<Report>,
    pub alerts: VecDeque<Alert>,
    pub paused: bool,
    pub window_secs: f64,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub filter: String,
    pub show_kernel: bool,
    pub selected_pid: Option<u32>,
    pub proc_offset: usize,
    pub alert_scroll: u16,
    pub notice: Option<(String, Instant)>,
    pub should_quit: bool,
    pub tab_hitboxes: Vec<Rect>,
    pub table_area: Rect,
    interval: Arc<AtomicU64>,
    cmd_tx: Sender<Cmd>,
}

impl App {
    pub fn new(cfg: Config, interval: Arc<AtomicU64>, cmd_tx: Sender<Cmd>) -> Self {
        let theme = Theme::by_name(&cfg.theme);
        let hist = Histories::new(cfg.history_secs as f64);
        let window_secs = cfg.window_secs as f64;
        Self {
            cfg,
            theme,
            tab: Tab::Overview,
            mode: Mode::Normal,
            snap: None,
            hist,
            engine: Engine::new(),
            report: None,
            alerts: VecDeque::new(),
            paused: false,
            window_secs,
            sort: SortKey::Cpu,
            sort_desc: true,
            filter: String::new(),
            show_kernel: false,
            selected_pid: None,
            proc_offset: 0,
            alert_scroll: 0,
            notice: None,
            should_quit: false,
            tab_hitboxes: Vec::new(),
            table_area: Rect::default(),
            interval,
            cmd_tx,
        }
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval.load(Ordering::Relaxed)
    }

    pub fn set_notice(&mut self, msg: impl Into<String>) {
        self.notice = Some((msg.into(), Instant::now()));
    }

    /// Expire the status-bar notice; returns true if the screen needs a redraw.
    pub fn tick_notice(&mut self) -> bool {
        if matches!(&self.notice, Some((_, t)) if t.elapsed() > Duration::from_secs(4)) {
            self.notice = None;
            return true;
        }
        false
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Snapshot(s) => self.on_snapshot(*s),
            Msg::Notice(n) => self.set_notice(n),
        }
    }

    pub fn on_snapshot(&mut self, snap: Snapshot) {
        if self.paused {
            return;
        }
        self.hist.ingest(&snap);
        let (report, new_alerts) = self.engine.evaluate(&snap, &self.hist, &self.cfg);
        for a in new_alerts {
            if let Some(path) = &self.cfg.log_file {
                let _ = export::log_alert(std::path::Path::new(path), &a);
            }
            self.alerts.push_front(a);
        }
        self.alerts.truncate(MAX_ALERTS);
        self.report = Some(report);
        self.snap = Some(snap);
    }

    /// Filtered + sorted view of the process list.
    pub fn visible_procs(&self) -> Vec<&ProcInfo> {
        let Some(snap) = &self.snap else { return Vec::new() };
        let f = self.filter.to_lowercase();
        let mut v: Vec<&ProcInfo> = snap
            .procs
            .iter()
            .filter(|p| self.show_kernel || !p.kernel)
            .filter(|p| {
                f.is_empty()
                    || p.name.to_lowercase().contains(&f)
                    || p.user.to_lowercase().contains(&f)
                    || p.pid.to_string().contains(&f)
            })
            .collect();
        use std::cmp::Ordering::Equal;
        v.sort_by(|a, b| {
            let primary = match self.sort {
                SortKey::Cpu => a.cpu.partial_cmp(&b.cpu).unwrap_or(Equal),
                SortKey::Mem => a.mem.cmp(&b.mem),
                SortKey::Pid => a.pid.cmp(&b.pid),
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            };
            // Ties (very common: most processes idle at 0%) must not reshuffle every tick.
            primary.then_with(|| a.pid.cmp(&b.pid))
        });
        if self.sort_desc {
            v.reverse();
        }
        v
    }

    /// Index of the selected process within the visible list (defaults to 0).
    #[allow(dead_code)]
    pub fn selected_index(&self) -> usize {
        let v = self.visible_procs();
        self.selected_pid.and_then(|pid| v.iter().position(|p| p.pid == pid)).unwrap_or(0)
    }

    fn move_selection(&mut self, delta: isize) {
        let (len, idx, pids): (usize, usize, Vec<u32>) = {
            let v = self.visible_procs();
            let idx = self.selected_pid.and_then(|pid| v.iter().position(|p| p.pid == pid)).unwrap_or(0);
            (v.len(), idx, v.iter().map(|p| p.pid).collect())
        };
        if len == 0 {
            self.selected_pid = None;
            return;
        }
        let new = (idx as isize + delta).clamp(0, len as isize - 1) as usize;
        self.selected_pid = Some(pids[new]);
    }

    fn selected_proc(&self) -> Option<(u32, String)> {
        let v = self.visible_procs();
        let idx = self.selected_pid.and_then(|pid| v.iter().position(|p| p.pid == pid)).unwrap_or(0);
        v.get(idx).map(|p| (p.pid, p.name.clone()))
    }

    fn set_sort(&mut self, key: SortKey) {
        if self.sort == key {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort = key;
            self.sort_desc = matches!(key, SortKey::Cpu | SortKey::Mem);
        }
    }

    #[allow(dead_code)]
    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }

    fn step_tab(&mut self, delta: isize) {
        let n = Tab::ALL.len() as isize;
        let i = (self.tab.index() as isize + delta).rem_euclid(n) as usize;
        self.tab = Tab::ALL[i];
    }

    fn step_interval(&mut self, up: bool) {
        let cur = self.interval_ms();
        let idx = INTERVALS_MS.iter().position(|&v| v >= cur).unwrap_or(2);
        let new = if up { (idx + 1).min(INTERVALS_MS.len() - 1) } else { idx.saturating_sub(1) };
        self.interval.store(INTERVALS_MS[new], Ordering::Relaxed);
        let _ = self.cmd_tx.send(Cmd::Refresh);
        self.set_notice(format!("Refresh interval: {} ms", INTERVALS_MS[new]));
    }

    fn cycle_window(&mut self) {
        let max = self.cfg.history_secs as f64;
        let cur = self.window_secs;
        let next = WINDOWS_SECS.iter().map(|&w| w as f64).find(|&w| w > cur && w <= max).unwrap_or(WINDOWS_SECS[0] as f64);
        self.window_secs = next;
        self.set_notice(format!("Chart window: {}", crate::util::fmt_window(next)));
    }

    fn export(&mut self) {
        match (&self.snap, &self.report) {
            (Some(s), Some(r)) => match export::save(self.cfg.export_dir.as_deref(), s, r) {
                Ok(p) => self.set_notice(format!("Saved report: {}", p.display())),
                Err(e) => self.set_notice(format!("Export failed: {e}")),
            },
            _ => self.set_notice("Nothing to export yet"),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match self.mode.clone() {
            Mode::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::F(1) | KeyCode::Enter) {
                    self.mode = Mode::Normal;
                }
            }
            Mode::Filter => match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = Mode::Normal;
                }
                KeyCode::Enter => self.mode = Mode::Normal,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            },
            Mode::ConfirmKill(pid, _) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let _ = self.cmd_tx.send(Cmd::Kill(pid));
                    self.mode = Mode::Normal;
                }
                _ => {
                    self.mode = Mode::Normal;
                    self.set_notice("Kill cancelled");
                }
            },
            Mode::Detail(_) => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    self.mode = Mode::Normal;
                }
            }
            Mode::Normal => self.on_key_normal(key),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') | KeyCode::F(1) => self.mode = Mode::Help,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.step_tab(1),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => self.step_tab(-1),
            KeyCode::Char(c @ '1'..='7') => self.tab = Tab::ALL[(c as u8 - b'1') as usize],
            KeyCode::Char(' ') | KeyCode::Char('f') => {
                self.paused = !self.paused;
                self.set_notice(if self.paused { "Paused: sampling frozen" } else { "Resumed" });
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.step_interval(true),
            KeyCode::Char('-') | KeyCode::Char('_') => self.step_interval(false),
            KeyCode::Char('w') => self.cycle_window(),
            KeyCode::Char('e') => self.export(),
            _ => match self.tab {
                Tab::Processes => self.on_key_processes(key),
                Tab::Alerts => match key.code {
                    KeyCode::Down | KeyCode::Char('j') => self.alert_scroll = self.alert_scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => self.alert_scroll = self.alert_scroll.saturating_sub(1),
                    KeyCode::Home | KeyCode::Char('g') => self.alert_scroll = 0,
                    _ => {}
                },
                _ => {}
            },
        }
    }

    fn on_key_processes(&mut self, key: KeyEvent) {
        let page = self.table_area.height.saturating_sub(3).max(1) as isize;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(page),
            KeyCode::PageUp => self.move_selection(-page),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Esc => self.filter.clear(),
            KeyCode::Char('c') => self.set_sort(SortKey::Cpu),
            KeyCode::Char('m') => self.set_sort(SortKey::Mem),
            KeyCode::Char('i') => self.set_sort(SortKey::Pid),
            KeyCode::Char('n') => self.set_sort(SortKey::Name),
            KeyCode::Char('r') => self.sort_desc = !self.sort_desc,
            KeyCode::Char('K') => {
                self.show_kernel = !self.show_kernel;
                self.set_notice(if self.show_kernel { "Showing kernel threads" } else { "Hiding kernel threads" });
            }
            KeyCode::Enter => {
                if let Some((pid, _)) = self.selected_proc() {
                    self.mode = Mode::Detail(pid);
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some((pid, name)) = self.selected_proc() {
                    self.mode = Mode::ConfirmKill(pid, name);
                }
            }
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.mode != Mode::Normal {
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self.tab_hitboxes.iter().position(|r| {
                    m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height
                });
                if let Some(i) = hit {
                    self.tab = Tab::ALL[i];
                } else if self.tab == Tab::Processes {
                    let a = self.table_area;
                    if m.column >= a.x && m.column < a.x + a.width && m.row > a.y + 1 && m.row < a.y + a.height {
                        let row = (m.row - a.y - 2) as usize + self.proc_offset;
                        let pid = self.visible_procs().get(row).map(|p| p.pid);
                        if pid.is_some() {
                            self.selected_pid = pid;
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => match self.tab {
                Tab::Processes => self.move_selection(3),
                Tab::Alerts => self.alert_scroll = self.alert_scroll.saturating_add(2),
                _ => {}
            },
            MouseEventKind::ScrollUp => match self.tab {
                Tab::Processes => self.move_selection(-3),
                Tab::Alerts => self.alert_scroll = self.alert_scroll.saturating_sub(2),
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use std::sync::mpsc;

    pub(super) fn test_app() -> App {
        let (tx, _rx) = mpsc::channel();
        let cfg = Config::default();
        let interval = Arc::new(AtomicU64::new(cfg.refresh_ms));
        let mut app = App::new(cfg, interval, tx);
        app.on_snapshot(Snapshot::demo());
        app
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent { code: c, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    #[test]
    fn tab_navigation_wraps() {
        let mut app = test_app();
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.tab, Tab::Alerts);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Overview);
        app.on_key(key(KeyCode::Char('6')));
        assert_eq!(app.tab, Tab::Processes);
    }

    #[test]
    fn pause_drops_snapshots() {
        let mut app = test_app();
        app.on_key(key(KeyCode::Char(' ')));
        assert!(app.paused);
        let before = app.hist.cpu.len();
        app.on_snapshot(Snapshot::demo());
        assert_eq!(app.hist.cpu.len(), before);
    }

    #[test]
    fn filter_sort_and_selection() {
        let mut app = test_app();
        app.set_tab(Tab::Processes);
        assert_eq!(app.visible_procs().len(), 40);
        app.on_key(key(KeyCode::Char('/')));
        for c in "proc-1".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.visible_procs().len(), 11); // proc-1 and proc-10..19
        app.on_key(key(KeyCode::Char('n')));
        assert_eq!(app.sort, SortKey::Name);
        app.on_key(key(KeyCode::Down));
        let pid = app.selected_pid.unwrap();
        assert_eq!(app.visible_procs()[app.selected_index()].pid, pid);
    }

    #[test]
    fn kill_requires_confirmation() {
        let mut app = test_app();
        app.set_tab(Tab::Processes);
        app.on_key(key(KeyCode::Char('x')));
        assert!(matches!(app.mode, Mode::ConfirmKill(_, _)));
        app.on_key(key(KeyCode::Char('n')));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn interval_and_window_cycle() {
        let mut app = test_app();
        app.on_key(key(KeyCode::Char('+')));
        assert_eq!(app.interval_ms(), 2000);
        app.on_key(key(KeyCode::Char('-')));
        app.on_key(key(KeyCode::Char('-')));
        assert_eq!(app.interval_ms(), 500);
        let w = app.window_secs;
        app.on_key(key(KeyCode::Char('w')));
        assert!(app.window_secs > w);
    }
}

#[cfg(test)]
mod order_tests {
    use super::tests::test_app;

    #[test]
    fn equal_values_keep_a_stable_order() {
        let mut app = test_app();
        // Force a massive tie on the sort key and shuffle the source order.
        if let Some(s) = app.snap.as_mut() {
            for p in s.procs.iter_mut() {
                p.cpu = 0.0;
            }
            s.procs.reverse();
        }
        let a: Vec<u32> = app.visible_procs().iter().map(|p| p.pid).collect();
        if let Some(s) = app.snap.as_mut() {
            s.procs.rotate_left(7);
        }
        let b: Vec<u32> = app.visible_procs().iter().map(|p| p.pid).collect();
        assert_eq!(a, b, "ties must not depend on input order");
    }
}

#[cfg(test)]
mod kernel_tests {
    use super::tests::test_app;
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    #[test]
    fn kernel_threads_hidden_until_toggled() {
        let mut app = test_app();
        if let Some(s) = app.snap.as_mut() {
            s.procs[0].kernel = true;
            s.procs[1].kernel = true;
        }
        app.set_tab(Tab::Processes);
        assert_eq!(app.visible_procs().len(), 38);
        let k = KeyEvent { code: KeyCode::Char('K'), modifiers: KeyModifiers::SHIFT, kind: KeyEventKind::Press, state: KeyEventState::NONE };
        app.on_key(k);
        assert_eq!(app.visible_procs().len(), 40);
        app.on_key(k);
        assert_eq!(app.visible_procs().len(), 38);
    }
}
