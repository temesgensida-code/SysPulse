//! All rendering. Nothing here mutates health data; it only reads `App`.

use crate::app::{App, Mode, SortKey, Tab};
use crate::health::Level;
use crate::history::Series;
use crate::metrics::Snapshot;
use crate::theme::Theme;
use crate::util::*;
use ratatui::prelude::*;
use ratatui::symbols::Marker;
use ratatui::widgets::block::Title;
use ratatui::widgets::{
    Axis, Bar, BarChart, BarGroup, Block, Borders, Cell, Chart, Clear, Dataset, Gauge, GraphType, Paragraph, Row,
    Sparkline, Table, TableState, Wrap,
};

fn vsplit(area: Rect, c: &[Constraint]) -> std::rc::Rc<[Rect]> {
    Layout::default().direction(Direction::Vertical).constraints(c.to_vec()).split(area)
}

fn hsplit(area: Rect, c: &[Constraint]) -> std::rc::Rc<[Rect]> {
    Layout::default().direction(Direction::Horizontal).constraints(c.to_vec()).split(area)
}

fn level_color(t: &Theme, l: Level) -> Color {
    match l {
        Level::Ok => t.ok,
        Level::Warn => t.warn,
        Level::Crit => t.crit,
    }
}

fn pct_color(t: &Theme, v: f64, warn: f64, crit: f64) -> Color {
    level_color(t, crate::health::level_for(v, warn, crit))
}

fn score_color(t: &Theme, score: u8) -> Color {
    if score >= 80 { t.ok } else if score >= 50 { t.warn } else { t.crit }
}

fn block<'a>(t: &Theme, title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.dim))
        .title(title.into().style(Style::default().fg(t.accent).add_modifier(Modifier::BOLD)))
}

fn dim(t: &Theme) -> Style {
    Style::default().fg(t.dim)
}

fn gauge(f: &mut Frame, area: Rect, t: &Theme, ratio: f64, label: String, color: Color) {
    let g = Gauge::default()
        .gauge_style(Style::default().fg(color).bg(t.dim))
        .ratio(if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { 0.0 })
        .label(Span::styled(label, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
        .use_unicode(true);
    f.render_widget(g, area);
}

fn message(f: &mut Frame, area: Rect, t: &Theme, title: &str, text: &str) {
    f.render_widget(
        Paragraph::new(text.to_string()).style(dim(t)).wrap(Wrap { trim: true }).block(block(t, title.to_string())),
        area,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

struct Plot {
    name: String,
    color: Color,
    pts: Vec<(f64, f64)>,
}

impl Plot {
    fn new(name: &str, color: Color, pts: Vec<(f64, f64)>) -> Self {
        Self { name: name.to_string(), color, pts }
    }
}

/// Horizontal reference line across the window (used for avg / p95 overlays).
fn hline(window: f64, y: f64) -> Vec<(f64, f64)> {
    vec![(-window, y), (0.0, y)]
}

#[allow(clippy::too_many_arguments)]
fn draw_chart(
    f: &mut Frame,
    area: Rect,
    t: &Theme,
    title: String,
    plots: &[Plot],
    y_max: f64,
    y_fmt: fn(f64) -> String,
    window: f64,
) {
    let datasets: Vec<Dataset> = plots
        .iter()
        .map(|p| {
            let mut d = Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(p.color))
                .data(&p.pts);
            if !p.name.is_empty() {
                d = d.name(p.name.clone());
            }
            d
        })
        .collect();
    let y_max = if y_max.is_finite() && y_max > 0.0 { y_max } else { 1.0 };
    let chart = Chart::new(datasets)
        .block(block(t, title))
        .x_axis(
            Axis::default()
                .style(dim(t))
                .bounds([-window, 0.0])
                .labels(vec![Span::raw(format!("-{}", fmt_window(window))), Span::raw(format!("-{}", fmt_window(window / 2.0))), Span::raw("now")]),
        )
        .y_axis(
            Axis::default()
                .style(dim(t))
                .bounds([0.0, y_max])
                .labels(vec![Span::raw(y_fmt(0.0)), Span::raw(y_fmt(y_max / 2.0)), Span::raw(y_fmt(y_max))]),
        );
    f.render_widget(chart, area);
}

fn pct_label(v: f64) -> String {
    format!("{v:.0}%")
}

fn rate_label(v: f64) -> String {
    fmt_rate(v)
}

fn bytes_label(v: f64) -> String {
    fmt_bytes_f(v)
}

/// Auto y-max for rate charts: max of the plotted data plus 20% headroom.
fn auto_max(plots: &[Plot], floor: f64) -> f64 {
    let m = plots.iter().flat_map(|p| p.pts.iter().map(|&(_, y)| y)).fold(0.0f64, f64::max);
    (m * 1.2).max(floor)
}

fn stats_title(name: &str, s: &Series, now: f64, window: f64, fmt: fn(f64) -> String) -> String {
    match s.stats(now, window) {
        Some(st) => format!(
            " {name}  now {}  avg {}  p95 {}  max {} ",
            fmt(s.last().unwrap_or(0.0)),
            fmt(st.avg),
            fmt(st.p95),
            fmt(st.max)
        ),
        None => format!(" {name} "),
    }
}

fn cpu_chart(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let t = &app.theme;
    let w = app.window_secs;
    let mut plots = vec![Plot::new("cpu", t.cpu, app.hist.cpu.points(snap.ts, w))];
    if let Some(st) = app.hist.cpu.stats(snap.ts, w) {
        plots.push(Plot::new("avg", t.avg, hline(w, st.avg)));
        plots.push(Plot::new("p95", t.warn, hline(w, st.p95)));
    }
    draw_chart(f, area, t, stats_title("CPU %", &app.hist.cpu, snap.ts, w, pct_label), &plots, 100.0, pct_label, w);
}

fn mem_chart(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let t = &app.theme;
    let w = app.window_secs;
    let used = app.hist.mem_used.points(snap.ts, w);
    let stacked: Vec<(f64, f64)> = used
        .iter()
        .zip(app.hist.mem_cached.points(snap.ts, w))
        .map(|(&(x, u), (_, c))| (x, u + c))
        .collect();
    let plots = vec![
        Plot::new("used", t.mem, used),
        Plot::new("+cache", t.cache, stacked),
        Plot::new("total", t.dim, hline(w, snap.mem_total as f64)),
    ];
    let title = format!(
        " Memory  {} used of {} ({:.0}%) ",
        fmt_bytes(snap.mem_used()),
        fmt_bytes(snap.mem_total),
        snap.mem_pct()
    );
    draw_chart(f, area, t, title, &plots, snap.mem_total as f64, bytes_label, w);
}

fn net_chart(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let t = &app.theme;
    let w = app.window_secs;
    let plots = vec![
        Plot::new("down", t.rx, app.hist.rx.points(snap.ts, w)),
        Plot::new("up", t.tx, app.hist.tx.points(snap.ts, w)),
    ];
    let (rx, tx) = snap.net_totals();
    let title = format!(" Network  down {}  up {} ", fmt_rate(rx), fmt_rate(tx));
    let y = auto_max(&plots, 10.0 * 1024.0);
    draw_chart(f, area, t, title, &plots, y, rate_label, w);
}

fn disk_io_chart(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let t = &app.theme;
    let w = app.window_secs;
    match &snap.disk_io {
        None => message(f, area, t, " Disk I/O ", "Per-device I/O counters are not available on this platform."),
        Some(io) => {
            let plots = vec![
                Plot::new("read", t.disk_r, app.hist.disk_r.points(snap.ts, w)),
                Plot::new("write", t.disk_w, app.hist.disk_w.points(snap.ts, w)),
            ];
            let title = format!(" Disk I/O  read {}  write {}  {:.0} IOPS ", fmt_rate(io.read_bps), fmt_rate(io.write_bps), io.iops);
            let y = auto_max(&plots, 100.0 * 1024.0);
            draw_chart(f, area, t, title, &plots, y, rate_label, w);
        }
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.size();
    let t = app.theme;
    f.render_widget(Block::default().style(Style::default().bg(t.bg).fg(t.fg)), area);
    let rows = vsplit(area, &[Constraint::Length(3), Constraint::Min(3), Constraint::Length(1)]);

    draw_header(f, app, rows[0]);

    if app.snap.is_none() {
        message(f, rows[1], &t, " SysPulse ", "Collecting the first sample...");
    } else {
        match app.tab {
            Tab::Overview => draw_overview(f, app, rows[1]),
            Tab::Cpu => draw_cpu(f, app, rows[1]),
            Tab::Memory => draw_memory(f, app, rows[1]),
            Tab::Disk => draw_disk(f, app, rows[1]),
            Tab::Network => draw_network(f, app, rows[1]),
            Tab::Processes => draw_processes(f, app, rows[1]),
            Tab::Alerts => draw_alerts(f, app, rows[1]),
        }
    }
    draw_footer(f, app, rows[2]);

    match app.mode.clone() {
        Mode::Help => draw_help(f, &t, area),
        Mode::ConfirmKill(pid, name) => draw_confirm(f, &t, area, pid, &name),
        Mode::Detail(pid) => draw_detail(f, app, area, pid),
        _ => {}
    }
}

fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme;
    let host = app.snap.as_ref().map(|s| s.hostname.clone()).unwrap_or_default();
    let health = match &app.report {
        Some(r) => Line::from(vec![
            Span::styled(" Health ", dim(&t)),
            Span::styled(
                format!("{} {} ", r.score, r.level.label()),
                Style::default().fg(score_color(&t, r.score)).add_modifier(Modifier::BOLD),
            ),
        ]),
        None => Line::from(Span::styled(" Health -- ", dim(&t))),
    };
    let b = Block::default()
        .borders(Borders::ALL)
        .border_style(dim(&t))
        .title(Title::from(Span::styled(
            format!(" SysPulse  {host} "),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        )))
        .title(Title::from(health).alignment(Alignment::Right));
    let inner = b.inner(area);
    f.render_widget(b, area);

    let mut spans = Vec::new();
    let mut boxes = Vec::new();
    let mut x = inner.x;
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let label = format!(" {} {} ", i + 1, tab.title());
        let w = label.chars().count() as u16;
        let avail = inner.x.saturating_add(inner.width).saturating_sub(x);
        boxes.push(Rect { x, y: inner.y, width: w.min(avail), height: 1 });
        let style = if *tab == app.tab {
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::styled("│", dim(&t)));
        x = x.saturating_add(w + 1);
    }
    app.tab_hitboxes = boxes;
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let mut left = vec![
        Span::styled(" ?", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)),
        Span::styled(" help  ", dim(t)),
        Span::styled("Tab", Style::default().fg(t.accent)),
        Span::styled(" switch  ", dim(t)),
        Span::styled("space", Style::default().fg(t.accent)),
        Span::styled(" pause  ", dim(t)),
        Span::styled("w", Style::default().fg(t.accent)),
        Span::styled(" window  ", dim(t)),
        Span::styled("+/-", Style::default().fg(t.accent)),
        Span::styled(" rate  ", dim(t)),
        Span::styled("e", Style::default().fg(t.accent)),
        Span::styled(" export  ", dim(t)),
        Span::styled("q", Style::default().fg(t.accent)),
        Span::styled(" quit", dim(t)),
    ];
    if let Some((msg, _)) = &app.notice {
        left = vec![Span::styled(format!(" {msg}"), Style::default().fg(t.warn).add_modifier(Modifier::BOLD))];
    }
    let state = if app.paused {
        Span::styled(" PAUSED ", Style::default().fg(Color::Black).bg(t.warn).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(" LIVE ", Style::default().fg(Color::Black).bg(t.ok).add_modifier(Modifier::BOLD))
    };
    let right = format!(" {}ms  window {} ", app.interval_ms(), fmt_window(app.window_secs));
    let right_w = right.chars().count() as u16 + 10;
    let cols = hsplit(area, &[Constraint::Min(1), Constraint::Length(right_w)]);
    f.render_widget(Paragraph::new(Line::from(left)), cols[0]);
    f.render_widget(Paragraph::new(Line::from(vec![state, Span::styled(right, dim(t))])).alignment(Alignment::Right), cols[1]);
}

fn draw_overview(f: &mut Frame, app: &App, area: Rect) {
    let Some(snap) = &app.snap else { return };
    let show_top = area.height >= 26;
    let show_bottom = area.height >= 36;
    let mut cons = Vec::new();
    if show_top {
        cons.push(Constraint::Length(10));
    }
    cons.push(Constraint::Min(8));
    if show_bottom {
        cons.push(Constraint::Length(9));
    }
    let rows = vsplit(area, &cons);
    let mut i = 0;
    if show_top {
        let top = hsplit(rows[0], &[Constraint::Percentage(45), Constraint::Percentage(55)]);
        draw_health_panel(f, app, top[0]);
        draw_sysinfo(f, app, snap, top[1]);
        i = 1;
    }
    let charts = rows[i];
    if charts.width >= 90 {
        let r = vsplit(charts, &[Constraint::Percentage(50), Constraint::Percentage(50)]);
        let a = hsplit(r[0], &[Constraint::Percentage(50), Constraint::Percentage(50)]);
        let b = hsplit(r[1], &[Constraint::Percentage(50), Constraint::Percentage(50)]);
        cpu_chart(f, a[0], app, snap);
        mem_chart(f, a[1], app, snap);
        net_chart(f, b[0], app, snap);
        disk_io_chart(f, b[1], app, snap);
    } else {
        let r = vsplit(charts, &[Constraint::Ratio(1, 4); 4]);
        cpu_chart(f, r[0], app, snap);
        mem_chart(f, r[1], app, snap);
        net_chart(f, r[2], app, snap);
        disk_io_chart(f, r[3], app, snap);
    }
    if show_bottom {
        let b = hsplit(rows[i + 1], &[Constraint::Percentage(50), Constraint::Percentage(50)]);
        draw_issues(f, app, b[0]);
        draw_top_procs(f, app, snap, b[1]);
    }
}

fn draw_health_panel(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let b = block(t, " Health ");
    let inner = b.inner(area);
    f.render_widget(b, area);
    let Some(r) = &app.report else { return };
    let rows = vsplit(inner, &[Constraint::Length(1), Constraint::Min(1)]);
    gauge(f, rows[0], t, r.score as f64 / 100.0, format!("Overall {}/100  {}", r.score, r.level.label()), score_color(t, r.score));
    let bar_w = (inner.width as usize).saturating_sub(24).clamp(4, 24);
    let lines: Vec<Line> = r
        .subs
        .iter()
        .map(|s| {
            let c = score_color(t, s.score);
            Line::from(vec![
                Span::raw(format!("{:<8}", s.sub.label())),
                Span::styled(bar(s.score as f64 / 100.0, bar_w), Style::default().fg(c)),
                Span::styled(format!(" {:>3} ", s.score), Style::default().fg(c).add_modifier(Modifier::BOLD)),
                Span::styled(s.level.label(), Style::default().fg(level_color(t, s.level))),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rows[1]);
}

fn draw_sysinfo(f: &mut Frame, app: &App, s: &Snapshot, area: Rect) {
    let t = &app.theme;
    let kv = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<10}"), dim(t)), Span::raw(v)]);
    let mut lines = vec![
        kv("Host", s.hostname.clone()),
        kv("OS", format!("{} (kernel {})", s.os, s.kernel)),
        kv("Uptime", fmt_duration(s.uptime)),
        kv("CPU", format!("{} x{}", s.cpu_brand, s.cores.len())),
        kv("Load", format!("{:.2} {:.2} {:.2}", s.load[0], s.load[1], s.load[2])),
        kv("Processes", format!("{} ({} zombie)", s.procs.len(), s.zombies)),
    ];
    if let Some((label, temp)) = s.max_temp() {
        lines.push(kv("Hottest", format!("{temp:.0}°C  {label}")));
    }
    if let Some(b) = &s.battery {
        lines.push(kv("Battery", format!("{:.0}% {}", b.pct, b.state)));
    }
    f.render_widget(Paragraph::new(lines).block(block(t, " System ")), area);
}

fn draw_issues(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let mut lines: Vec<Line> = Vec::new();
    if let Some(r) = &app.report {
        for c in r.checks.iter().filter(|c| c.level > Level::Ok) {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<5}", c.level.label()), Style::default().fg(level_color(t, c.level)).add_modifier(Modifier::BOLD)),
                Span::raw(c.title.clone()),
            ]));
            if let Some(h) = &c.hint {
                lines.push(Line::from(Span::styled(format!("     {h}"), dim(t))));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("✔ All checks passing", Style::default().fg(t.ok))));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block(t, " Issues ")), area);
}

fn draw_top_procs(f: &mut Frame, app: &App, s: &Snapshot, area: Rect) {
    let t = &app.theme;
    let mut procs: Vec<_> = s.user_procs().collect();
    procs.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.pid.cmp(&b.pid)));
    let rows: Vec<Row> = procs
        .iter()
        .take(area.height.saturating_sub(3) as usize)
        .map(|p| {
            Row::new(vec![
                Cell::from(p.pid.to_string()),
                Cell::from(truncate(&p.name, 22)),
                Cell::from(format!("{:.1}%", p.cpu)).style(Style::default().fg(pct_color(t, p.cpu as f64, 40.0, 80.0))),
                Cell::from(fmt_bytes(p.mem)),
            ])
        })
        .collect();
    let table = Table::new(rows, [Constraint::Length(7), Constraint::Min(10), Constraint::Length(7), Constraint::Length(10)])
        .header(Row::new(vec!["PID", "NAME", "CPU", "MEM"]).style(dim(t)))
        .block(block(t, " Top processes "));
    f.render_widget(table, area);
}

fn draw_cpu(f: &mut Frame, app: &App, area: Rect) {
    let Some(snap) = &app.snap else { return };
    let t = &app.theme;
    let th = &app.cfg.thresholds;
    let rows = vsplit(area, &[Constraint::Length(8), Constraint::Min(6), Constraint::Length(9)]);
    let top = hsplit(rows[0], &[Constraint::Percentage(60), Constraint::Percentage(40)]);

    let b = block(t, format!(" {} ", if snap.cpu_brand.is_empty() { "CPU" } else { &snap.cpu_brand }));
    let inner = b.inner(top[0]);
    f.render_widget(b, top[0]);
    let l = vsplit(inner, &[Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)]);
    gauge(f, l[0], t, snap.cpu_total as f64 / 100.0, format!("Total {:.1}%", snap.cpu_total), pct_color(t, snap.cpu_total as f64, th.cpu_warn, th.cpu_crit));
    let spark = app.hist.cpu.tail_u64(l[1].width as usize, 1.0);
    f.render_widget(Sparkline::default().data(&spark).max(100).style(Style::default().fg(t.cpu)), l[1]);
    let stats = app.hist.cpu.stats(snap.ts, app.window_secs);
    let avg_freq = if snap.freqs.is_empty() { 0 } else { snap.freqs.iter().sum::<u64>() / snap.freqs.len() as u64 };
    let mut text = vec![
        Line::from(format!("Load avg  {:.2}  {:.2}  {:.2}   ({} cores)", snap.load[0], snap.load[1], snap.load[2], snap.cores.len())),
        Line::from(format!("Frequency {} MHz avg", avg_freq)),
    ];
    if let Some(s) = stats {
        text.push(Line::from(format!("Window    min {:.0}%  avg {:.0}%  p95 {:.0}%  max {:.0}%", s.min, s.avg, s.p95, s.max)));
    }
    f.render_widget(Paragraph::new(text), l[2]);

    let mut temps: Vec<_> = snap.temps.iter().filter(|x| x.temp.is_finite() && x.temp > 0.0).collect();
    temps.sort_by(|a, b| b.temp.partial_cmp(&a.temp).unwrap_or(std::cmp::Ordering::Equal));
    let lines: Vec<Line> = if temps.is_empty() {
        vec![Line::from(Span::styled("No temperature sensors detected", dim(t)))]
    } else {
        temps
            .iter()
            .take(top[1].height.saturating_sub(2) as usize)
            .map(|x| {
                Line::from(vec![
                    Span::styled(format!("{:>5.0}°C ", x.temp), Style::default().fg(pct_color(t, x.temp as f64, th.temp_warn, th.temp_crit))),
                    Span::raw(truncate(&x.label, 28)),
                ])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines).block(block(t, " Sensors ")), top[1]);

    cpu_chart(f, rows[1], app, snap);

    let n = snap.cores.len().max(1);
    let w = rows[2].width.saturating_sub(2) as usize;
    let slot = (w / n).max(1);
    let (bw, gap) = if slot >= 3 { ((slot - 1).min(9), 1) } else { (slot, 0) };
    let bars: Vec<Bar> = snap
        .cores
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let col = pct_color(t, *c as f64, th.cpu_warn, th.cpu_crit);
            let mut bar = Bar::default().value(c.round() as u64).style(Style::default().fg(col)).value_style(Style::default().fg(Color::Black).bg(col));
            if bw >= 2 {
                bar = bar.label(Line::from(i.to_string()));
            }
            bar
        })
        .collect();
    let chart = BarChart::default()
        .block(block(t, " Per-core usage "))
        .data(BarGroup::default().bars(&bars))
        .bar_width(bw as u16)
        .bar_gap(gap)
        .max(100);
    f.render_widget(chart, rows[2]);
}

fn draw_memory(f: &mut Frame, app: &App, area: Rect) {
    let Some(s) = &app.snap else { return };
    let t = &app.theme;
    let th = &app.cfg.thresholds;
    let rows = vsplit(area, &[Constraint::Length(8), Constraint::Min(6), Constraint::Length(9)]);
    let top = hsplit(rows[0], &[Constraint::Percentage(55), Constraint::Percentage(45)]);

    let b = block(t, " Usage ");
    let inner = b.inner(top[0]);
    f.render_widget(b, top[0]);
    let g = vsplit(inner, &[Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]);
    gauge(f, g[0], t, s.mem_pct() / 100.0, format!("RAM {:.1}%  ({} / {})", s.mem_pct(), fmt_bytes(s.mem_used()), fmt_bytes(s.mem_total)), pct_color(t, s.mem_pct(), th.mem_warn, th.mem_crit));
    if s.swap_total > 0 {
        gauge(f, g[2], t, s.swap_pct() / 100.0, format!("Swap {:.1}%  ({} / {})", s.swap_pct(), fmt_bytes(s.swap_used), fmt_bytes(s.swap_total)), pct_color(t, s.swap_pct(), th.swap_warn, th.swap_crit));
    } else {
        f.render_widget(Paragraph::new(Span::styled("No swap configured", dim(t))), g[2]);
    }

    let trend = app
        .hist
        .mem_pct
        .slope_since(s.ts - app.window_secs.min(300.0))
        .map(|sl| format!("{:+.2} %/min", sl * 60.0))
        .unwrap_or_else(|| "collecting...".into());
    let lines = vec![
        Line::from(format!("Total     {}", fmt_bytes(s.mem_total))),
        Line::from(format!("Used      {}", fmt_bytes(s.mem_used()))),
        Line::from(format!("Cache     {}", fmt_bytes(s.mem_cached))),
        Line::from(format!("Free      {}", fmt_bytes(s.mem_free))),
        Line::from(format!("Available {}", fmt_bytes(s.mem_avail))),
        Line::from(format!("Trend     {trend}")),
    ];
    f.render_widget(Paragraph::new(lines).block(block(t, " Details ")), top[1]);

    mem_chart(f, rows[1], app, s);

    let mut procs: Vec<_> = s.user_procs().collect();
    procs.sort_by(|a, b| b.mem.cmp(&a.mem).then_with(|| a.pid.cmp(&b.pid)));
    let table_rows: Vec<Row> = procs
        .iter()
        .take(rows[2].height.saturating_sub(3) as usize)
        .map(|p| {
            Row::new(vec![
                Cell::from(p.pid.to_string()),
                Cell::from(truncate(&p.name, 30)),
                Cell::from(fmt_bytes(p.mem)),
                Cell::from(format!("{:.1}%", p.mem as f64 / s.mem_total.max(1) as f64 * 100.0)),
            ])
        })
        .collect();
    let table = Table::new(table_rows, [Constraint::Length(7), Constraint::Min(10), Constraint::Length(11), Constraint::Length(7)])
        .header(Row::new(vec!["PID", "NAME", "MEM", "%"]).style(dim(t)))
        .block(block(t, " Top memory consumers "));
    f.render_widget(table, rows[2]);
}

fn draw_disk(f: &mut Frame, app: &App, area: Rect) {
    let Some(s) = &app.snap else { return };
    let t = &app.theme;
    let th = &app.cfg.thresholds;
    let disks: Vec<_> = s.disks.iter().filter(|d| d.is_real()).collect();
    let table_h = (disks.len() as u16 + 3).clamp(4, 12);
    let rows = vsplit(area, &[Constraint::Length(table_h), Constraint::Min(6), Constraint::Length(3)]);

    let bar_w = (rows[0].width as usize).saturating_sub(62).clamp(6, 30);
    let trows: Vec<Row> = disks
        .iter()
        .map(|d| {
            let c = pct_color(t, d.used_pct(), th.disk_warn, th.disk_crit);
            Row::new(vec![
                Cell::from(truncate(&d.mount, 22)),
                Cell::from(truncate(&d.name, 16)),
                Cell::from(d.fs.clone()),
                Cell::from(d.kind.clone()),
                Cell::from(fmt_bytes(d.total)),
                Cell::from(fmt_bytes(d.avail)),
                Cell::from(format!("{} {:>3.0}%", bar(d.used_pct() / 100.0, bar_w), d.used_pct())).style(Style::default().fg(c)),
            ])
        })
        .collect();
    let table = Table::new(
        trows,
        [Constraint::Length(22), Constraint::Length(16), Constraint::Length(7), Constraint::Length(5), Constraint::Length(10), Constraint::Length(10), Constraint::Min(10)],
    )
    .header(Row::new(vec!["MOUNT", "DEVICE", "FS", "TYPE", "SIZE", "FREE", "USED"]).style(dim(t)))
    .column_spacing(1)
    .block(block(t, " Filesystems "));
    f.render_widget(table, rows[0]);

    disk_io_chart(f, rows[1], app, s);

    let forecast = app.report.as_ref().and_then(|r| r.checks.iter().find(|c| c.id == "disk_eta"));
    let line = match forecast {
        Some(c) => Line::from(vec![Span::styled(c.title.clone(), Style::default().fg(level_color(t, c.level)).add_modifier(Modifier::BOLD)), Span::styled(format!("  {}", c.detail), dim(t))]),
        None => Line::from(Span::styled("Fill-time forecast: no significant growth detected yet (needs a few minutes of samples)", dim(t))),
    };
    f.render_widget(Paragraph::new(line).block(block(t, " Forecast ")), rows[2]);
}

fn draw_network(f: &mut Frame, app: &App, area: Rect) {
    let Some(s) = &app.snap else { return };
    let t = &app.theme;
    let table_h = (s.nets.len() as u16 + 3).clamp(4, 12);
    let rows = vsplit(area, &[Constraint::Length(table_h), Constraint::Min(6), Constraint::Length(4)]);

    let trows: Vec<Row> = s
        .nets
        .iter()
        .map(|n| {
            Row::new(vec![
                Cell::from(n.name.clone()),
                Cell::from(fmt_rate(n.rx_rate)).style(Style::default().fg(t.rx)),
                Cell::from(fmt_rate(n.tx_rate)).style(Style::default().fg(t.tx)),
                Cell::from(fmt_bytes(n.rx_total)),
                Cell::from(fmt_bytes(n.tx_total)),
                Cell::from(n.errors.to_string()).style(if n.errors > 0 { Style::default().fg(t.warn) } else { dim(t) }),
            ])
        })
        .collect();
    let table = Table::new(
        trows,
        [Constraint::Min(10), Constraint::Length(13), Constraint::Length(13), Constraint::Length(11), Constraint::Length(11), Constraint::Length(7)],
    )
    .header(Row::new(vec!["INTERFACE", "DOWN", "UP", "TOTAL RX", "TOTAL TX", "ERRORS"]).style(dim(t)))
    .block(block(t, " Interfaces "));
    f.render_widget(table, rows[0]);

    net_chart(f, rows[1], app, s);

    let w = app.window_secs;
    let fmt_s = |name: &str, ser: &Series| match ser.stats(s.ts, w) {
        Some(st) => format!("{name:<5} min {}   avg {}   p95 {}   max {}", fmt_rate(st.min), fmt_rate(st.avg), fmt_rate(st.p95), fmt_rate(st.max)),
        None => format!("{name:<5} collecting..."),
    };
    let lines = vec![
        Line::from(Span::styled(fmt_s("down", &app.hist.rx), Style::default().fg(t.rx))),
        Line::from(Span::styled(fmt_s("up", &app.hist.tx), Style::default().fg(t.tx))),
    ];
    f.render_widget(Paragraph::new(lines).block(block(t, format!(" Statistics ({}) ", fmt_window(w)))), rows[2]);
}

fn draw_processes(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme;
    let total_mem = app.snap.as_ref().map(|s| s.mem_total).unwrap_or(1).max(1);
    let show_filter = app.mode == Mode::Filter || !app.filter.is_empty();
    let parts = vsplit(area, &[Constraint::Length(if show_filter { 3 } else { 0 }), Constraint::Min(3)]);
    if show_filter {
        let cursor = if app.mode == Mode::Filter { "█" } else { "" };
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled("/ ", Style::default().fg(t.accent)), Span::raw(app.filter.clone()), Span::styled(cursor, Style::default().fg(t.accent))]))
                .block(block(&t, " Filter (Enter apply, Esc clear) ")),
            parts[0],
        );
    }
    let table_area = parts[1];

    let (rows, selected, count) = {
        let procs = app.visible_procs();
        let sel = app.selected_pid.and_then(|pid| procs.iter().position(|p| p.pid == pid)).unwrap_or(0);
        let rows: Vec<Row<'static>> = procs
            .iter()
            .map(|p| {
                let cpu_c = pct_color(&t, p.cpu as f64, 40.0, 80.0);
                let mem_pct = p.mem as f64 / total_mem as f64 * 100.0;
                let state_style = if p.zombie { Style::default().fg(t.crit) } else { Style::default() };
                Row::new(vec![
                    Cell::from(p.pid.to_string()),
                    Cell::from(truncate(&p.user, 12)),
                    Cell::from(truncate(&p.name, 36)),
                    Cell::from(p.status.clone()).style(state_style),
                    Cell::from(format!("{:>5.1}", p.cpu)).style(Style::default().fg(cpu_c)),
                    Cell::from(fmt_bytes(p.mem)),
                    Cell::from(format!("{mem_pct:>4.1}")),
                    Cell::from(fmt_duration(p.run_time)),
                ])
            })
            .collect();
        let n = rows.len();
        (rows, sel, n)
    };

    let arrow = |k: SortKey| if app.sort == k { if app.sort_desc { "▼" } else { "▲" } } else { "" };
    let header = Row::new(vec![
        format!("PID{}", arrow(SortKey::Pid)),
        "USER".to_string(),
        format!("NAME{}", arrow(SortKey::Name)),
        "STATE".to_string(),
        format!("CPU%{}", arrow(SortKey::Cpu)),
        format!("MEM{}", arrow(SortKey::Mem)),
        "MEM%".to_string(),
        "UPTIME".to_string(),
    ])
    .style(Style::default().fg(t.accent).add_modifier(Modifier::BOLD));

    let title = format!(" Processes ({count}) - c/m/i/n sort  / filter  K kernel  Enter info  x kill ");
    let table = Table::new(
        rows,
        [Constraint::Length(8), Constraint::Length(12), Constraint::Min(12), Constraint::Length(9), Constraint::Length(7), Constraint::Length(11), Constraint::Length(6), Constraint::Length(11)],
    )
    .header(header)
    .highlight_style(Style::default().bg(t.sel_bg).add_modifier(Modifier::BOLD))
    .highlight_symbol("▶ ")
    .block(block(&t, title));

    let mut state = TableState::default().with_offset(app.proc_offset).with_selected(if count == 0 { None } else { Some(selected) });
    f.render_stateful_widget(table, table_area, &mut state);
    app.proc_offset = state.offset();
    app.table_area = table_area;
}

fn draw_alerts(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let checks = app.report.as_ref().map(|r| r.checks.len()).unwrap_or(0) as u16;
    let rows = vsplit(area, &[Constraint::Length((checks + 3).clamp(5, 16)), Constraint::Min(4)]);

    let trows: Vec<Row> = app
        .report
        .as_ref()
        .map(|r| {
            r.checks
                .iter()
                .map(|c| {
                    Row::new(vec![
                        Cell::from(c.level.label()).style(Style::default().fg(level_color(t, c.level)).add_modifier(Modifier::BOLD)),
                        Cell::from(c.sub.label()),
                        Cell::from(c.title.clone()),
                        Cell::from(c.hint.clone().unwrap_or_else(|| c.detail.clone())).style(dim(t)),
                    ])
                })
                .collect()
        })
        .unwrap_or_default();
    let table = Table::new(trows, [Constraint::Length(5), Constraint::Length(8), Constraint::Percentage(40), Constraint::Min(10)])
        .header(Row::new(vec!["", "AREA", "CHECK", "DETAIL / HINT"]).style(dim(t)))
        .block(block(t, " Current checks "));
    f.render_widget(table, rows[0]);

    let lines: Vec<Line> = if app.alerts.is_empty() {
        vec![Line::from(Span::styled("No alerts yet. Alerts fire when a check changes state.", dim(t)))]
    } else {
        app.alerts
            .iter()
            .map(|a| {
                let c = if a.recovered { t.ok } else { level_color(t, a.level) };
                Line::from(vec![
                    Span::styled(format!("{}  ", clock(a.ts)), dim(t)),
                    Span::styled(format!("{:<5}", if a.recovered { "OK" } else { a.level.label() }), Style::default().fg(c).add_modifier(Modifier::BOLD)),
                    Span::raw(format!("{} ", a.title)),
                    Span::styled(a.detail.clone(), dim(t)),
                ])
            })
            .collect()
    };
    let scroll = app.alert_scroll.min(lines.len().saturating_sub(1) as u16);
    f.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).block(block(t, format!(" Alert history ({}) - j/k scroll ", app.alerts.len()))),
        rows[1],
    );
}

fn draw_help(f: &mut Frame, t: &Theme, area: Rect) {
    let r = centered(area, 68, 25);
    f.render_widget(Clear, r);
    let key = |k: &str, d: &str| Line::from(vec![Span::styled(format!("{k:<18}"), Style::default().fg(t.accent).add_modifier(Modifier::BOLD)), Span::raw(d.to_string())]);
    let lines = vec![
        Line::from(Span::styled("Global", Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED))),
        key("Tab / h l / 1-7", "switch tab (mouse: click tabs)"),
        key("space / f", "pause / resume sampling"),
        key("+  -", "slower / faster refresh"),
        key("w", "cycle chart window"),
        key("e", "export report (.json + .md)"),
        key("? / q", "help / quit"),
        Line::from(""),
        Line::from(Span::styled("Processes", Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED))),
        key("j k / arrows", "move (PgUp PgDn g G, mouse wheel)"),
        key("c m i n", "sort by CPU / mem / pid / name"),
        key("r", "reverse sort"),
        key("K", "show / hide kernel threads"),
        key("/", "filter by name, user or pid"),
        key("Enter", "process details"),
        key("x / Del", "terminate (SIGTERM, asks first)"),
        Line::from(""),
        Line::from(Span::styled("Alerts", Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED))),
        key("j k", "scroll history"),
        Line::from(""),
        Line::from(Span::styled("Esc / ? / Enter to close", dim(t))),
    ];
    f.render_widget(Paragraph::new(lines).block(block(t, " Help ").style(Style::default().bg(t.bg))), r);
}

fn draw_confirm(f: &mut Frame, t: &Theme, area: Rect, pid: u32, name: &str) {
    let r = centered(area, 52, 5);
    f.render_widget(Clear, r);
    let lines = vec![
        Line::from(format!("Send SIGTERM to {} (pid {pid})?", truncate(name, 28))),
        Line::from(""),
        Line::from(vec![Span::styled("y", Style::default().fg(t.crit).add_modifier(Modifier::BOLD)), Span::raw(" confirm    any other key cancels")]),
    ];
    f.render_widget(Paragraph::new(lines).block(block(t, " Terminate process ").style(Style::default().bg(t.bg))), r);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect, pid: u32) {
    let t = &app.theme;
    let r = centered(area, 56, 12);
    f.render_widget(Clear, r);
    let body = match app.snap.as_ref().and_then(|s| s.procs.iter().find(|p| p.pid == pid).map(|p| (p, s.mem_total))) {
        Some((p, total)) => vec![
            Line::from(format!("Name     {}", p.name)),
            Line::from(format!("PID      {}", p.pid)),
            Line::from(format!("User     {}", p.user)),
            Line::from(format!("State    {}", p.status)),
            Line::from(format!("CPU      {:.1}% of total capacity", p.cpu)),
            Line::from(format!("Memory   {} ({:.1}% of RAM)", fmt_bytes(p.mem), p.mem as f64 / total.max(1) as f64 * 100.0)),
            Line::from(format!("Running  {}", fmt_duration(p.run_time))),
            Line::from(""),
            Line::from(Span::styled("Esc / Enter to close", dim(t))),
        ],
        None => vec![Line::from("This process has exited."), Line::from(""), Line::from(Span::styled("Esc to close", dim(t)))],
    };
    f.render_widget(Paragraph::new(body).block(block(t, format!(" Process {pid} ")).style(Style::default().bg(t.bg))), r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Tab;
    use ratatui::backend::TestBackend;

    fn app_with_history() -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let cfg = crate::config::Config::default();
        let interval = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1000));
        let mut app = App::new(cfg, interval, tx);
        let mut s = Snapshot::demo();
        for i in 0..120 {
            s.ts = 1_700_000_000.0 + i as f64;
            s.cpu_total = 30.0 + 30.0 * ((i as f32) / 10.0).sin().abs();
            s.mem_avail = (6u64 << 30) - (i as u64) * 10_000_000;
            s.nets[0].rx_rate = 1e6 + (i as f64) * 1e4;
            app.on_snapshot(s.clone());
        }
        app
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<Vec<_>>().join("")
    }

    #[test]
    fn every_tab_renders_at_many_sizes() {
        let mut app = app_with_history();
        for tab in Tab::ALL {
            app.set_tab(tab);
            for (w, h) in [(160, 50), (120, 40), (100, 30), (80, 24), (60, 18), (40, 12), (20, 6), (1, 1), (5, 3)] {
                let _ = render(&mut app, w, h); // must not panic
            }
        }
    }

    #[test]
    fn overview_shows_health_and_charts() {
        let mut app = app_with_history();
        let out = render(&mut app, 140, 46);
        for needle in ["SysPulse", "Health", "Overall", "CPU %", "Memory", "Network", "Disk I/O", "Top processes"] {
            assert!(out.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn popups_render() {
        let mut app = app_with_history();
        app.set_tab(Tab::Processes);
        for mode in [Mode::Help, Mode::ConfirmKill(100, "proc-0".into()), Mode::Detail(101), Mode::Detail(999_999), Mode::Filter] {
            app.mode = mode;
            let _ = render(&mut app, 100, 30);
        }
    }

    #[test]
    fn processes_tab_records_table_area_and_tab_hitboxes() {
        let mut app = app_with_history();
        app.set_tab(Tab::Processes);
        let out = render(&mut app, 120, 30);
        assert!(out.contains("Processes (40)"));
        assert_eq!(app.tab_hitboxes.len(), 7);
        assert!(app.table_area.height > 0);
    }

    #[test]
    fn empty_state_renders_before_first_sample() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let cfg = crate::config::Config::default();
        let mut app = App::new(cfg, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1000)), tx);
        let out = render(&mut app, 80, 24);
        assert!(out.contains("Collecting"));
    }

    #[test]
    fn all_themes_render() {
        for theme in ["dark", "light", "colorblind"] {
            let mut app = app_with_history();
            app.theme = Theme::by_name(theme);
            let _ = render(&mut app, 120, 40);
        }
    }

    #[test]
    fn sub_labels_are_unique() {
        let mut names: Vec<_> = crate::health::Sub::ALL.iter().map(|s| s.label()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), crate::health::Sub::ALL.len());
    }
}
