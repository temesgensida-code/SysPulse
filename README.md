# SysPulse

A realtime computer **health-check TUI** for the terminal, written in Rust with
[ratatui](https://ratatui.rs). It samples your machine, scores its health from 0 to 100,
draws live analytical charts, and raises alerts when something goes wrong.

![TERMINAL DEMO](asset/demo.gif)

## Installation

one linear install
```bash
git clone https://github.com/temesgensida-code/SysPulse.git && cd SysPulse && ./install.sh
```

Run the automated installer to build and install `syspulse` directly to your PATH:


```bash
./install.sh
```

Or install with `cargo`:

```bash
cargo install --path .
```

Once installed, simply run `syspulse` from any terminal:

```bash
syspulse
```

## Quick Start

```bash
syspulse                            # interactive TUI
syspulse --interval 500 --theme colorblind
syspulse --snapshot                 # one health report, no TUI (exit code 2 if CRITICAL)
syspulse --snapshot --format json
syspulse --print-config > ~/.config/syspulse/config.toml
```

## Features

| Area | What you get |
|---|---|
| Metrics | CPU total + per-core + frequency + load, RAM/cache/swap, per-mount disk usage, disk read/write + IOPS (Linux), per-interface network rates + errors, processes, temperatures, battery (Linux) |
| Health engine | 0-100 score from weighted subsystem scores; OK / WARN / CRIT thresholds; sustained-CPU checks (no alerts on 1-second spikes); load-per-core; swap pressure; zombies; temperature; battery |
| Analytics | rolling window stats (min / avg / p95 / max), linear-regression **disk-fill forecast**, memory trend (%/min), **z-score anomaly detection** for CPU and memory spikes |
| Graphs | braille line charts with avg/p95 overlays, stacked-style memory chart, dual-axis network chart, disk I/O chart, per-core bar chart, sparklines, gauges |
| Alerts | edge-triggered (fires when a check *changes* level, plus a "recovered" event), history panel, optional JSON-lines log file |
| Processes | sort (c/m/i/n), reverse, live filter, details popup, kill with confirmation (SIGTERM), kernel-thread toggle (`K`) |
| UX | 7 tabs, mouse (tabs, rows, wheel), pause, adjustable refresh (250 ms - 10 s), chart window (1m-1h), 3 themes incl. colour-blind palette, help overlay, responsive layout down to tiny terminals |
| Export | `e` saves a `.json` + `.md` report; `--snapshot` prints text / json / md |



## Notes

Developed against Rust 1.75, so `Cargo.lock` pins some transitive crates to older versions. On a newer
toolchain you can bump ratatui/crossterm/sysinfo and run `cargo update`.
