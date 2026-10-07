//! SysPulse: a realtime computer health-check TUI.
//!
//! Threads: a collector thread samples the machine and sends `Snapshot`s over
//! a channel; the main thread owns the terminal, input and rendering.

mod app;
mod config;
mod export;
mod health;
mod history;
mod metrics;
mod theme;
mod ui;
mod util;

use anyhow::Result;
use app::App;
use clap::{Parser, ValueEnum};
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use metrics::{Cmd, Collector, Msg};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{stdout, Stdout};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Format {
    Text,
    Json,
    Md,
}

/// Realtime computer health check with analytical graphs.
#[derive(Parser, Debug)]
#[command(name = "syspulse", version, about)]
struct Cli {
    /// Path to a TOML config file (default: ~/.config/syspulse/config.toml).
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Sampling interval in milliseconds (250-10000).
    #[arg(short, long)]
    interval: Option<u64>,
    /// Colour theme: dark, light or colorblind.
    #[arg(long)]
    theme: Option<String>,
    /// Append alert events as JSON lines to this file.
    #[arg(long)]
    log_file: Option<String>,
    /// Print one health report and exit (no TUI). Useful for scripts and cron.
    #[arg(long)]
    snapshot: bool,
    /// Report format for --snapshot.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Print the default configuration as TOML and exit.
    #[arg(long)]
    print_config: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.print_config {
        print!("{}", config::default_toml());
        return Ok(());
    }
    let mut cfg = config::load(cli.config.as_deref())?;
    if let Some(i) = cli.interval {
        cfg.refresh_ms = i;
    }
    if let Some(t) = cli.theme {
        cfg.theme = t;
    }
    if cli.log_file.is_some() {
        cfg.log_file = cli.log_file;
    }
    cfg.sanitize();

    if cli.snapshot {
        return run_snapshot(&cfg, cli.format);
    }
    run_tui(cfg)
}

/// Headless mode: take a few samples so rates/CPU are meaningful, then report.
fn run_snapshot(cfg: &config::Config, format: Format) -> Result<()> {
    let mut collector = Collector::new();
    thread::sleep(Duration::from_millis(300));
    let mut hist = history::Histories::new(cfg.history_secs as f64);
    let mut engine = health::Engine::new();
    let mut last = None;
    for i in 0..4 {
        let snap = collector.collect();
        hist.ingest(&snap);
        let (report, _) = engine.evaluate(&snap, &hist, cfg);
        last = Some((snap, report));
        if i < 3 {
            thread::sleep(Duration::from_millis(400));
        }
    }
    let (snap, report) = last.expect("at least one sample");
    match format {
        Format::Text => print!("{}", export::to_text(&snap, &report)),
        Format::Json => println!("{}", export::to_json(&snap, &report)?),
        Format::Md => print!("{}", export::to_markdown(&snap, &report)),
    }
    if report.level == health::Level::Crit {
        std::process::exit(2);
    }
    Ok(())
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, crossterm::cursor::Show);
}

fn run_tui(cfg: config::Config) -> Result<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    let (tx, rx) = mpsc::channel::<Msg>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let interval = Arc::new(AtomicU64::new(cfg.refresh_ms));
    let collector = Collector::new();
    let handle = {
        let interval = interval.clone();
        thread::Builder::new().name("collector".into()).spawn(move || collector.run(interval, tx, cmd_rx))?
    };

    let mut app = App::new(cfg, interval, cmd_tx.clone());
    let mut terminal = setup_terminal()?;
    let result = run_loop(&mut terminal, &mut app, &rx);
    restore_terminal();
    let _ = cmd_tx.send(Cmd::Quit);
    let _ = handle.join();
    result
}

fn run_loop(terminal: &mut Term, app: &mut App, rx: &Receiver<Msg>) -> Result<()> {
    let mut dirty = true;
    while !app.should_quit {
        if dirty {
            terminal.draw(|f| ui::draw(f, app))?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                Event::Mouse(m) => app.on_mouse(m),
                _ => {}
            }
            dirty = true;
        }
        while let Ok(msg) = rx.try_recv() {
            app.on_msg(msg);
            dirty = true;
        }
        if app.tick_notice() {
            dirty = true;
        }
    }
    Ok(())
}
