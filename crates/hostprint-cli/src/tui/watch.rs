//! `hostprint watch`: capture on an interval and show what changes, both
//! between consecutive captures (a timeline) and against a reference (the
//! first capture, or a baseline).

use super::{
    bold, capture_job, change_value, dim, draw_footer, draw_help, draw_title, run_view, sig_style, Flow, Job, Status,
    View,
};
use crate::{App, CaptureOptions, WatchArgs};
use anyhow::{bail, Result};
use chrono::{DateTime, Local, Utc};
use hostprint_core::config::parse_duration;
use hostprint_core::Config;
use hostprint_diff::{Change, Diff, DiffOptions, Significance};
use hostprint_model::format::{bytes, duration};
use hostprint_model::{Container, Service, Snapshot};
use hostprint_storage::Kind;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Gauge, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;
use std::collections::VecDeque;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const MIN_INTERVAL: Duration = Duration::from_secs(2);
const TIMELINE_LEN: usize = 200;

pub fn run(app: &App, args: WatchArgs) -> Result<ExitCode> {
    let interval = parse_duration(&args.interval).map_err(anyhow::Error::msg)?;
    if interval < MIN_INTERVAL {
        bail!("--interval must be at least {}s", MIN_INTERVAL.as_secs());
    }
    let config = app.config()?;
    let reference = match &args.baseline {
        Some(name) => Some((app.store.load_from(Kind::Baseline, name)?, format!("baseline '{name}'"))),
        None => None,
    };
    let opts = app.diff_options(&config)?;
    let mut watch = Watch::new(app, config, opts, args.options, interval, reference);
    run_view(&mut watch)?;
    Ok(ExitCode::SUCCESS)
}

pub(crate) struct Watch<'a> {
    app: &'a App,
    config: Config,
    opts: DiffOptions,
    options: CaptureOptions,
    interval: Duration,
    paused: bool,
    reference: Option<Snapshot>,
    reference_label: String,
    latest: Option<Snapshot>,
    /// Reference → latest.
    drift: Option<Diff>,
    /// Changes between consecutive captures, newest first.
    timeline: VecDeque<(DateTime<Utc>, Change)>,
    captures: u32,
    last_duration: Option<Duration>,
    job: Option<Job<Snapshot>>,
    next: Instant,
    status: Option<Status>,
    help: bool,
}

impl<'a> Watch<'a> {
    pub fn new(
        app: &'a App,
        config: Config,
        opts: DiffOptions,
        options: CaptureOptions,
        interval: Duration,
        reference: Option<(Snapshot, String)>,
    ) -> Self {
        let (reference, reference_label) = match reference {
            Some((s, label)) => (Some(s), label),
            None => (None, "first capture".into()),
        };
        Watch {
            app,
            config,
            opts,
            options,
            interval,
            paused: false,
            reference,
            reference_label,
            latest: None,
            drift: None,
            timeline: VecDeque::new(),
            captures: 0,
            last_duration: None,
            job: None,
            next: Instant::now(),
            status: None,
            help: false,
        }
    }

    fn start_capture(&mut self) {
        let name = format!("watch-{}", Utc::now().format("%Y%m%d-%H%M%S"));
        match capture_job(self.app, &self.config, &self.options, name) {
            Ok(job) => self.job = Some(job),
            Err(e) => self.status = Status::error(format!("{e:#}")),
        }
        self.next = Instant::now() + self.interval;
    }

    /// Folds a new capture into the reference, drift and timeline.
    pub fn on_capture(&mut self, snapshot: Snapshot) {
        self.captures += 1;
        let opts = &self.opts;
        if let Some(previous) = &self.latest {
            let step = hostprint_diff::diff(previous, &snapshot, opts);
            for change in step.changes.into_iter().rev().filter(|c| c.significance >= Significance::Low) {
                self.timeline.push_front((snapshot.captured_at, change));
            }
            self.timeline.truncate(TIMELINE_LEN);
        }
        if self.reference.is_none() {
            self.reference_label = format!("first capture at {}", local_time(snapshot.captured_at));
            self.reference = Some(snapshot.clone());
        }
        self.drift = self.reference.as_ref().map(|r| hostprint_diff::diff(r, &snapshot, opts));
        self.latest = Some(snapshot);
    }

    fn draw_gauges(&self, frame: &mut Frame, area: Rect) {
        let cells = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(area);
        let Some(r) = self.latest.as_ref().and_then(|s| s.resources.as_ref()) else {
            frame.render_widget(Paragraph::new(" waiting for the first capture…").style(dim()), area);
            return;
        };
        let cpu = r.cpu.usage_percent.unwrap_or(0.0) / 100.0;
        let mem = r.memory.used_bytes as f64 / r.memory.total_bytes.max(1) as f64;
        let fullest = r.disks.iter().filter_map(|d| Some((d, d.usage_ratio()?))).max_by(|a, b| a.1.total_cmp(&b.1));
        let load = r.load.map(|l| l.one / f64::from(r.cpu.logical_cores.max(1)));
        let gauges = [
            ("CPU", cpu, format!("{:.0}%", cpu * 100.0)),
            ("Memory", mem, format!("{:.0}% of {}", mem * 100.0, bytes(r.memory.total_bytes))),
            (
                "Disk",
                fullest.map_or(0.0, |f| f.1),
                fullest.map_or("n/a".into(), |(d, ratio)| format!("{} {:.0}%", d.mount_point, ratio * 100.0)),
            ),
            (
                "Load",
                load.unwrap_or(0.0).min(1.0),
                match (r.load, load) {
                    (Some(l), Some(per_core)) => format!("{:.2} ({per_core:.1}/core)", l.one),
                    _ => "n/a".into(),
                },
            ),
        ];
        for ((title, ratio, label), cell) in gauges.into_iter().zip(cells.iter()) {
            let color = if ratio >= 0.9 {
                Color::Red
            } else if ratio >= 0.7 {
                Color::Yellow
            } else {
                Color::Green
            };
            let gauge = Gauge::default()
                .block(Block::bordered().title(format!(" {title} ")))
                .gauge_style(Style::new().fg(color))
                .ratio(ratio.clamp(0.0, 1.0))
                .label(label);
            frame.render_widget(gauge, *cell);
        }
    }

    fn draw_health(&self, frame: &mut Frame, area: Rect) {
        let [left, right] = Layout::horizontal([Constraint::Percentage(50); 2]).areas(area);
        let latest = self.latest.as_ref();
        let unavailable = |collector: &str| -> String {
            latest
                .and_then(|s| s.collector(collector))
                .and_then(|r| r.message.clone())
                .unwrap_or_else(|| "waiting for data…".into())
        };

        match latest.and_then(|s| s.services.as_ref()) {
            Some(services) => {
                let mut list: Vec<&Service> =
                    services.iter().filter(|s| s.active_state != "inactive" || s.restarts.unwrap_or(0) > 0).collect();
                list.sort_by_key(|s| (service_rank(s), s.name.clone()));
                let failed = services.iter().filter(|s| s.active_state == "failed").count();
                let items: Vec<ListItem> = list.iter().map(|s| service_item(s)).collect();
                let title = format!(" Services · {} active · {failed} failed ", list.len() - failed);
                frame.render_widget(List::new(items).block(Block::bordered().title(title)), left);
            }
            None => frame.render_widget(
                Paragraph::new(unavailable("services")).style(dim()).block(Block::bordered().title(" Services ")),
                left,
            ),
        }

        match latest.and_then(|s| s.docker.as_ref()) {
            Some(docker) => {
                let mut list: Vec<&Container> = docker.containers.iter().collect();
                list.sort_by_key(|c| (container_rank(c), c.name.clone()));
                let running = list.iter().filter(|c| c.state == "running").count();
                let items: Vec<ListItem> = list.iter().map(|c| container_item(c)).collect();
                let title = format!(" Containers · {running}/{} running ", list.len());
                frame.render_widget(List::new(items).block(Block::bordered().title(title)), right);
            }
            None => frame.render_widget(
                Paragraph::new(unavailable("docker")).style(dim()).block(Block::bordered().title(" Containers ")),
                right,
            ),
        }
    }

    fn draw_drift(&self, frame: &mut Frame, area: Rect) {
        let changes: Vec<&Change> =
            self.drift.as_ref().map(|d| d.at_least(Significance::Low).collect()).unwrap_or_default();
        let title = match &self.drift {
            Some(d) => format!(
                " Changed since {} · {} high · {} medium · {} low ",
                self.reference_label, d.summary.high, d.summary.medium, d.summary.low
            ),
            None => format!(" Changed since {} ", self.reference_label),
        };
        if changes.is_empty() {
            let msg = if self.drift.is_some() { "No changes at LOW or above." } else { "" };
            frame.render_widget(Paragraph::new(msg).style(dim()).block(Block::bordered().title(title)), area);
            return;
        }
        let rows: Vec<Row> = changes
            .iter()
            .map(|c| {
                Row::new(vec![
                    Cell::from(c.significance.label()).style(sig_style(c.significance)),
                    Cell::from(c.category.label()).style(dim()),
                    Cell::from(c.subject.clone()).style(bold()),
                    Cell::from(c.field.clone().unwrap_or_default()),
                    Cell::from(change_value(c)),
                ])
            })
            .collect();
        let widths = [
            Constraint::Length(6),
            Constraint::Length(13),
            Constraint::Min(18),
            Constraint::Length(18),
            Constraint::Min(30),
        ];
        frame.render_widget(Table::new(rows, widths).block(Block::bordered().title(title)), area);
    }

    fn draw_timeline(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .timeline
            .iter()
            .map(|(at, c)| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", local_time(*at)), dim()),
                    Span::styled(format!("{:<6} ", c.significance.label()), sig_style(c.significance)),
                    Span::styled(format!("{} ", c.subject), bold()),
                    Span::raw(format!("{} ", c.field.clone().unwrap_or_default())),
                    Span::raw(change_value(c)),
                ]))
            })
            .collect();
        let block = Block::bordered().title(" Recent state changes ");
        if items.is_empty() {
            let msg = if self.captures < 2 {
                "Changes between captures will appear here."
            } else {
                "Nothing has changed yet."
            };
            frame.render_widget(Paragraph::new(msg).style(dim()).block(block), area);
        } else {
            frame.render_widget(List::new(items).block(block), area);
        }
    }
}

impl View for Watch<'_> {
    fn tick(&mut self) {
        if let Some(result) = self.job.as_ref().and_then(Job::poll) {
            let started = self.job.take().expect("checked above").started;
            match result {
                Ok(snapshot) => {
                    self.last_duration = Some(started.elapsed());
                    self.on_capture(snapshot);
                }
                Err(e) => self.status = Status::error(e),
            }
        }
        if !self.paused && self.job.is_none() && Instant::now() >= self.next {
            self.start_capture();
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [title, gauges, health, drift, timeline, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let host = self.latest.as_ref().and_then(|s| s.hostname()).unwrap_or("…").to_string();
        let state = if self.job.is_some() {
            "capturing…".to_string()
        } else if self.paused {
            "paused".to_string()
        } else {
            format!("next in {}s", self.next.saturating_duration_since(Instant::now()).as_secs())
        };
        let took = self.last_duration.map(|d| format!(" ({:.1}s)", d.as_secs_f64())).unwrap_or_default();
        let detail = format!(
            "{host} · every {} · capture #{}{took} · {state}",
            duration(self.interval.as_secs()),
            self.captures
        );
        draw_title(frame, title, "WATCH", &detail);
        self.draw_gauges(frame, gauges);
        self.draw_health(frame, health);
        self.draw_drift(frame, drift);
        self.draw_timeline(frame, timeline);
        let hints: &[(&str, &str)] = &[
            ("space", "capture now"),
            ("p", if self.paused { "resume" } else { "pause" }),
            ("r", "reset reference"),
            ("s", "save snapshot"),
            ("?", "help"),
            ("q", "quit"),
        ];
        draw_footer(frame, footer, hints, &mut self.status);
        if self.help {
            draw_help(frame, HELP);
        }
    }

    fn key(&mut self, key: KeyEvent) -> Flow {
        if self.help {
            self.help = false;
            return Flow::Continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                if !self.paused {
                    self.next = Instant::now();
                }
            }
            KeyCode::Char(' ') if self.job.is_none() => self.start_capture(),
            KeyCode::Char('r') => {
                if let Some(latest) = &self.latest {
                    self.reference = Some(latest.clone());
                    self.reference_label = format!("reset at {}", local_time(latest.captured_at));
                    self.drift = None;
                    self.status = Status::info("Reference reset to the latest capture");
                }
            }
            KeyCode::Char('s') => {
                self.status = match &self.latest {
                    Some(s) => match self.app.store.save(s, false) {
                        Ok(_) => Status::info(format!("Saved snapshot {} (hostprint show {})", s.name, s.name)),
                        Err(e) => Status::error(e.to_string()),
                    },
                    None => Status::error("Nothing captured yet"),
                };
            }
            _ => {}
        }
        Flow::Continue
    }
}

const HELP: &[(&str, &str)] = &[
    ("space", "capture now"),
    ("p", "pause or resume capturing"),
    ("r", "use the latest capture as the reference"),
    ("s", "save the latest capture as a snapshot"),
    ("q  esc", "quit"),
];

fn local_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M:%S").to_string()
}

/// Problems first.
fn service_rank(s: &Service) -> u8 {
    match (s.active_state.as_str(), s.sub_state.as_str()) {
        ("failed", _) => 0,
        ("activating", "auto-restart") => 1,
        ("active", "running") => 3,
        _ => 2,
    }
}

fn service_item(s: &Service) -> ListItem<'static> {
    let style = match service_rank(s) {
        0 => Style::new().fg(Color::Red),
        1 | 2 => Style::new().fg(Color::Yellow),
        _ => Style::new(),
    };
    let restarts = s.restarts.filter(|r| *r > 0).map(|r| format!("  ↻{r}")).unwrap_or_default();
    ListItem::new(Line::from(vec![
        Span::styled(format!("{:<22} ", s.name.trim_end_matches(".service")), style),
        Span::styled(format!("{} ({}){restarts}", s.active_state, s.sub_state), style),
    ]))
}

fn container_rank(c: &Container) -> u8 {
    match (c.state.as_str(), c.health.as_deref()) {
        (_, Some("unhealthy")) | ("restarting", _) | ("dead", _) => 0,
        ("running", Some("starting")) => 1,
        ("running", _) => 3,
        _ => 2,
    }
}

fn container_item(c: &Container) -> ListItem<'static> {
    let style = match container_rank(c) {
        0 => Style::new().fg(Color::Red),
        1 | 2 => Style::new().fg(Color::Yellow),
        _ => Style::new(),
    };
    let health = c.health.as_deref().map(|h| format!(" {h}")).unwrap_or_default();
    let restarts = if c.restart_count > 0 { format!("  ↻{}", c.restart_count) } else { String::new() };
    ListItem::new(Line::from(vec![
        Span::styled(format!("{:<22} ", c.name), style),
        Span::styled(format!("{}{health}{restarts}", c.state), style),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style as TextStyle;
    use crate::tui::fixtures::{broken, snapshot};
    use crate::tui::testing::{char, render};
    use hostprint_storage::Store;

    fn app(tag: &str) -> App {
        let root = std::env::temp_dir().join(format!("hostprint-watch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        App { store: Store::new(root), style: TextStyle::plain(), err_style: TextStyle::plain(), policy_file: None }
    }

    fn watch(app: &App) -> Watch<'_> {
        let mut w = Watch::new(
            app,
            Config::default(),
            DiffOptions::default(),
            CaptureOptions::default(),
            Duration::from_secs(10),
            None,
        );
        w.paused = true; // tests feed captures by hand
        w
    }

    #[test]
    fn shows_health_drift_and_timeline() {
        let app = app("timeline");
        let mut w = watch(&app);
        let screen = render(&mut w, 120, 40);
        assert!(screen.contains("waiting for the first capture"), "{screen}");

        w.on_capture(snapshot("t1", 0));
        let screen = render(&mut w, 120, 40);
        assert!(screen.contains("CPU") && screen.contains("38%"), "{screen}");
        assert!(screen.contains("No changes at LOW or above"), "{screen}");
        assert!(screen.contains("Changes between captures will appear here"), "{screen}");

        w.on_capture(broken("t2", 10));
        let screen = render(&mut w, 120, 40);
        assert!(screen.contains("4 high · 1 medium"), "{screen}");
        assert!(screen.contains("restart count"), "{screen}");
        assert!(screen.contains("0 → 17"), "{screen}");
        // Problems are listed first, in red: redis before api.
        let redis = screen.find("restarting unhealthy").expect(&screen);
        let api = screen.find("running healthy").expect(&screen);
        assert!(redis < api, "{screen}");
        assert!(screen.contains("failed (failed)"), "{screen}");
        assert_eq!(w.timeline.front().unwrap().1.significance, Significance::High);
    }

    #[test]
    fn reset_reference_and_save() {
        let app = app("keys");
        let mut w = watch(&app);
        w.on_capture(snapshot("t1", 0));
        w.on_capture(broken("t2", 10));
        assert!(w.drift.as_ref().unwrap().summary.high > 0);
        w.key(char('r'));
        w.on_capture(broken("t3", 20));
        assert_eq!(w.drift.as_ref().unwrap().changes, [], "drift is measured from the new reference");
        w.key(char('s'));
        assert!(app.store.exists("t3"));
        assert_eq!(w.key(char('q')), Flow::Quit);
        let _ = std::fs::remove_dir_all(app.store.root());
    }
}
