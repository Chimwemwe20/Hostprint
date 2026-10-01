//! `hostprint tui`: browse snapshots and baselines, open one, compare two (or
//! one with the live system), filter the diff, inspect a change, bundle it.

use super::{
    bold, capture_job, change_value, dim, draw_footer, draw_help, draw_title, popup, run_view, selected, sig_style,
    step, Flow, Job, Status, View,
};
use crate::style::clip;
use crate::{bundle, capture, show, App, CaptureOptions};
use anyhow::Result;
use hostprint_core::Config;
use hostprint_diff::{Category, Change, Diff, DiffOptions, Significance};
use hostprint_model::format::{bytes, duration};
use hostprint_model::{CollectorStatus, Snapshot};
use hostprint_storage::{Entry, Kind};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Tabs, Wrap};
use ratatui::Frame;
use std::process::ExitCode;

const SECTIONS: [&str; 9] =
    ["Overview", "Processes", "Ports", "Services", "Containers", "Disks", "Env", "Logs", "Collectors"];

pub fn run(app: &App) -> Result<ExitCode> {
    let config = app.config()?;
    let mut browser = Browser::new(app, config)?;
    run_view(&mut browser)?;
    Ok(ExitCode::SUCCESS)
}

pub(crate) struct Browser<'a> {
    app: &'a App,
    config: Config,
    opts: DiffOptions,
    tab: Kind,
    entries: Vec<Entry>,
    table: TableState,
    /// Name of the entry marked as the "from" side of a comparison.
    marked: Option<String>,
    screen: Screen,
    help: bool,
    status: Option<Status>,
    job: Option<(Pending, Job<Snapshot>)>,
}

enum Screen {
    List,
    Snapshot(Box<SnapshotView>),
    Diff(Box<DiffView>),
}

/// Header, column widths and rows of a table section.
type TableData = (Vec<&'static str>, Vec<Constraint>, Vec<Vec<String>>);

/// What to do with a finished capture.
enum Pending {
    Save,
    CompareWithNow(Box<Snapshot>),
}

struct SnapshotView {
    snapshot: Snapshot,
    section: usize,
    table: TableState,
    scroll: u16,
    filter: String,
    editing: bool,
}

struct DiffView {
    diff: Diff,
    from: Snapshot,
    to: Snapshot,
    min: Significance,
    category: Option<Category>,
    table: TableState,
    detail: bool,
}

impl<'a> Browser<'a> {
    pub fn new(app: &'a App, config: Config) -> Result<Browser<'a>> {
        let opts = app.diff_options(&config)?;
        let mut b = Browser {
            app,
            config,
            opts,
            tab: Kind::Snapshot,
            entries: Vec::new(),
            table: TableState::default(),
            marked: None,
            screen: Screen::List,
            help: false,
            status: None,
            job: None,
        };
        b.reload(None)?;
        Ok(b)
    }

    /// Reloads the list, keeping (or moving to) the named entry.
    fn reload(&mut self, select: Option<&str>) -> Result<()> {
        let keep = select.map(str::to_string).or_else(|| self.selected_entry().map(|e| e.name.clone()));
        self.entries = self.app.store.list_in(self.tab)?;
        // Newest first: the snapshot you just took is the one you want.
        self.entries.reverse();
        let index = keep.and_then(|n| self.entries.iter().position(|e| e.name == n));
        self.table.select(index.or(if self.entries.is_empty() { None } else { Some(0) }));
        Ok(())
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.table.selected().and_then(|i| self.entries.get(i))
    }

    fn load(&self, name: &str) -> Result<Snapshot, String> {
        self.app.store.load_from(self.tab, name).map_err(|e| e.to_string())
    }

    fn open_diff(&mut self, from: Snapshot, to: Snapshot) {
        let diff = hostprint_diff::diff(&from, &to, &self.opts);
        let mut table = TableState::default();
        table.select(if diff.changes.is_empty() { None } else { Some(0) });
        let mut view = DiffView { diff, from, to, min: Significance::Low, category: None, table, detail: false };
        view.reselect();
        self.screen = Screen::Diff(Box::new(view));
    }

    fn start_capture(&mut self, pending: Pending) {
        if self.job.is_some() {
            self.status = Status::error("A capture is already running");
            return;
        }
        let name = capture::default_name();
        match capture_job(self.app, &self.config, &CaptureOptions::default(), name) {
            Ok(job) => {
                self.status = Status::info("Capturing…");
                self.job = Some((pending, job));
            }
            Err(e) => self.status = Status::error(format!("{e:#}")),
        }
    }

    fn list_key(&mut self, key: KeyEvent) -> Flow {
        let len = self.entries.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.table.select(step(self.table.selected(), len, -1)),
            KeyCode::Down | KeyCode::Char('j') => self.table.select(step(self.table.selected(), len, 1)),
            KeyCode::Home | KeyCode::Char('g') => self.table.select(step(Some(0), len, 0)),
            KeyCode::End | KeyCode::Char('G') => self.table.select(step(Some(len.saturating_sub(1)), len, 0)),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.tab = if self.tab == Kind::Snapshot { Kind::Baseline } else { Kind::Snapshot };
                self.marked = None;
                if let Err(e) = self.reload(None) {
                    self.status = Status::error(e.to_string());
                }
            }
            KeyCode::Char('r') => match self.reload(None) {
                Ok(()) => self.status = Status::info("Reloaded"),
                Err(e) => self.status = Status::error(e.to_string()),
            },
            KeyCode::Enter => {
                if let Some(name) = self.selected_entry().map(|e| e.name.clone()) {
                    match self.load(&name) {
                        Ok(snapshot) => self.screen = Screen::Snapshot(Box::new(SnapshotView::new(snapshot))),
                        Err(e) => self.status = Status::error(e),
                    }
                }
            }
            KeyCode::Char(' ') => {
                if let Some(name) = self.selected_entry().map(|e| e.name.clone()) {
                    self.marked = if self.marked.as_deref() == Some(&name) { None } else { Some(name) };
                }
            }
            KeyCode::Char('c') => self.compare(),
            KeyCode::Char('n') => {
                if let Some(name) = self.selected_entry().map(|e| e.name.clone()) {
                    match self.load(&name) {
                        Ok(snapshot) => self.start_capture(Pending::CompareWithNow(Box::new(snapshot))),
                        Err(e) => self.status = Status::error(e),
                    }
                }
            }
            KeyCode::Char('s') => self.start_capture(Pending::Save),
            _ => {}
        }
        Flow::Continue
    }

    /// Compares the marked entry (or the one below the selection, which is
    /// older) with the selected one.
    fn compare(&mut self) {
        let Some(index) = self.table.selected() else { return };
        let to_name = self.entries[index].name.clone();
        let from_name = match &self.marked {
            Some(m) if *m != to_name => m.clone(),
            Some(_) => {
                self.status = Status::error("Select a different snapshot to compare with the marked one");
                return;
            }
            None => match self.entries.get(index + 1) {
                Some(older) => older.name.clone(),
                None => {
                    self.status = Status::error("Nothing older to compare with: mark a snapshot with Space first");
                    return;
                }
            },
        };
        match (self.load(&from_name), self.load(&to_name)) {
            (Ok(from), Ok(to)) => {
                self.marked = None;
                self.open_diff(from, to);
            }
            (Err(e), _) | (_, Err(e)) => self.status = Status::error(e),
        }
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let [tabs_area, table_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        let counts = |kind: Kind| self.app.store.list_in(kind).map(|l| l.len()).unwrap_or(0);
        let titles = vec![
            format!(" Snapshots ({}) ", counts(Kind::Snapshot)),
            format!(" Baselines ({}) ", counts(Kind::Baseline)),
        ];
        let tabs = Tabs::new(titles)
            .select(if self.tab == Kind::Snapshot { 0 } else { 1 })
            .highlight_style(Style::new().fg(Color::Cyan).add_modifier(ratatui::style::Modifier::BOLD))
            .divider("");
        frame.render_widget(tabs, tabs_area);

        if self.entries.is_empty() {
            let msg = match self.tab {
                Kind::Snapshot => "No snapshots yet. Press s to capture one now.",
                Kind::Baseline => "No baselines yet. Create one with `hostprint baseline create NAME`.",
            };
            frame.render_widget(Paragraph::new(msg).style(dim()).block(Block::bordered()), table_area);
            return;
        }
        let rows: Vec<Row> = self
            .entries
            .iter()
            .map(|e| {
                let mark = if self.marked.as_deref() == Some(&e.name) { "●" } else { " " };
                match &e.info {
                    Ok(info) => Row::new(vec![
                        Cell::from(mark).style(Style::new().fg(Color::Yellow)),
                        Cell::from(e.name.clone()).style(bold()),
                        Cell::from(info.captured_at.format("%Y-%m-%d %H:%M:%S").to_string()),
                        Cell::from(info.hostname.clone().unwrap_or_default()),
                        Cell::from(bytes(e.size)),
                        Cell::from(format!("{}/{}", info.collectors_with_data, info.collectors_total)),
                    ]),
                    Err(err) => Row::new(vec![
                        Cell::from(mark),
                        Cell::from(e.name.clone()),
                        Cell::from(clip(err, 60)).style(Style::new().fg(Color::Red)),
                    ]),
                }
            })
            .collect();
        let widths = [
            Constraint::Length(1),
            Constraint::Min(16),
            Constraint::Length(19),
            Constraint::Min(12),
            Constraint::Length(10),
            Constraint::Length(10),
        ];
        let table = Table::new(rows, widths)
            .header(Row::new(["", "NAME", "CAPTURED (UTC)", "HOST", "SIZE", "COLLECTED"]).style(dim()))
            .row_highlight_style(selected())
            .block(Block::bordered());
        frame.render_stateful_widget(table, table_area, &mut self.table);
    }
}

impl View for Browser<'_> {
    fn tick(&mut self) {
        let Some(result) = self.job.as_ref().and_then(|(_, job)| job.poll()) else { return };
        let (pending, _) = self.job.take().expect("checked above");
        let snapshot = match result {
            Ok(s) => s,
            Err(e) => {
                self.status = Status::error(e);
                return;
            }
        };
        match pending {
            Pending::Save => match self.app.store.save(&snapshot, false) {
                Ok(_) => {
                    self.tab = Kind::Snapshot;
                    let _ = self.reload(Some(&snapshot.name));
                    self.status = Status::info(format!("Saved snapshot {}", snapshot.name));
                }
                Err(e) => self.status = Status::error(e.to_string()),
            },
            Pending::CompareWithNow(from) => {
                let mut now = snapshot;
                now.name = "now".into();
                self.status = None;
                self.open_diff(*from, now);
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [title, body, footer] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
        match &mut self.screen {
            Screen::List => {
                draw_title(frame, title, "Snapshots", "");
                self.draw_list(frame, body);
                let hints: &[(&str, &str)] = &[
                    ("↵", "open"),
                    ("space", "mark"),
                    ("c", "compare"),
                    ("n", "vs now"),
                    ("s", "capture"),
                    ("tab", "baselines"),
                    ("?", "help"),
                    ("q", "quit"),
                ];
                draw_footer(frame, footer, hints, &mut self.status);
            }
            Screen::Snapshot(view) => {
                let detail = format!(
                    "{} · {}",
                    view.snapshot.captured_at.format("%Y-%m-%d %H:%M:%S UTC"),
                    view.snapshot.hostname().unwrap_or("?")
                );
                draw_title(frame, title, &view.snapshot.name.clone(), &detail);
                view.draw(frame, body);
                let hints: &[(&str, &str)] = &[
                    ("←→", "section"),
                    ("↑↓", "scroll"),
                    ("/", "filter"),
                    ("esc", "back"),
                    ("?", "help"),
                    ("q", "quit"),
                ];
                draw_footer(frame, footer, hints, &mut self.status);
            }
            Screen::Diff(view) => {
                draw_title(frame, title, &format!("{} → {}", view.diff.from.name, view.diff.to.name), "");
                view.draw(frame, body);
                let hints: &[(&str, &str)] = &[
                    ("↵", "details"),
                    ("m", "min level"),
                    ("c", "category"),
                    ("b", "bundle"),
                    ("esc", "back"),
                    ("?", "help"),
                    ("q", "quit"),
                ];
                draw_footer(frame, footer, hints, &mut self.status);
            }
        }
        if self.job.is_some() {
            let w = 26.min(frame.area().width);
            let area = Rect { x: frame.area().width.saturating_sub(w), y: 0, width: w, height: 1 };
            frame.render_widget(Paragraph::new(" ● capturing… ").style(Style::new().fg(Color::Yellow)), area);
        }
        if self.help {
            draw_help(frame, HELP);
        }
    }

    fn key(&mut self, key: KeyEvent) -> Flow {
        if self.help {
            self.help = false;
            return Flow::Continue;
        }
        let editing = matches!(&self.screen, Screen::Snapshot(v) if v.editing);
        if key.code == KeyCode::Char('?') && !editing {
            self.help = true;
            return Flow::Continue;
        }
        match &mut self.screen {
            Screen::List => self.list_key(key),
            Screen::Snapshot(view) => match view.key(key) {
                ViewFlow::Back => {
                    self.screen = Screen::List;
                    Flow::Continue
                }
                ViewFlow::Quit => Flow::Quit,
                ViewFlow::Stay => Flow::Continue,
            },
            Screen::Diff(view) => {
                if key.code == KeyCode::Char('b') {
                    self.status = match bundle::write(&view.to, Some(&view.from), Some(&view.diff), None) {
                        Ok(w) => Status::info(format!("Bundle written: {}", w.path.display())),
                        Err(e) => Status::error(format!("{e:#}")),
                    };
                    return Flow::Continue;
                }
                match view.key(key) {
                    ViewFlow::Back => {
                        self.screen = Screen::List;
                        Flow::Continue
                    }
                    ViewFlow::Quit => Flow::Quit,
                    ViewFlow::Stay => Flow::Continue,
                }
            }
        }
    }
}

const HELP: &[(&str, &str)] = &[
    ("↑ ↓  j k", "move"),
    ("enter", "open a snapshot / change details"),
    ("space", "mark the 'from' side of a comparison"),
    ("c", "compare marked (or next older) → selected"),
    ("n", "compare selected with the system now"),
    ("s", "capture and save a snapshot now"),
    ("tab", "switch snapshots / baselines"),
    ("← →", "switch section (snapshot view)"),
    ("/", "filter rows (snapshot view)"),
    ("m  c", "minimum level, category (diff view)"),
    ("b", "write an incident bundle (diff view)"),
    ("esc", "back"),
    ("q", "quit"),
];

enum ViewFlow {
    Stay,
    Back,
    Quit,
}

// --- Snapshot view ----------------------------------------------------------

impl SnapshotView {
    fn new(snapshot: Snapshot) -> Self {
        SnapshotView {
            snapshot,
            section: 0,
            table: TableState::default(),
            scroll: 0,
            filter: String::new(),
            editing: false,
        }
    }

    fn key(&mut self, key: KeyEvent) -> ViewFlow {
        if self.editing {
            match key.code {
                KeyCode::Enter => self.editing = false,
                KeyCode::Esc => {
                    self.editing = false;
                    self.filter.clear();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.table.select(Some(0));
            return ViewFlow::Stay;
        }
        let len = self.rows().map(|(_, _, rows)| rows.len()).unwrap_or(0);
        match key.code {
            KeyCode::Char('q') => return ViewFlow::Quit,
            KeyCode::Esc if !self.filter.is_empty() => self.filter.clear(),
            KeyCode::Esc | KeyCode::Backspace => return ViewFlow::Back,
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => self.set_section((self.section + 1) % SECTIONS.len()),
            KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => {
                self.set_section((self.section + SECTIONS.len() - 1) % SECTIONS.len())
            }
            KeyCode::Char(c @ '1'..='9') => self.set_section((c as usize - '1' as usize).min(SECTIONS.len() - 1)),
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1, len),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1, len),
            KeyCode::PageUp => self.scroll_by(-15, len),
            KeyCode::PageDown => self.scroll_by(15, len),
            _ => {}
        }
        ViewFlow::Stay
    }

    fn set_section(&mut self, section: usize) {
        self.section = section;
        self.scroll = 0;
        self.filter.clear();
        self.table = TableState::default().with_selected(Some(0));
    }

    fn scroll_by(&mut self, delta: isize, rows: usize) {
        if self.is_text() {
            self.scroll = (self.scroll as isize + delta).max(0) as u16;
        } else {
            self.table.select(step(self.table.selected(), rows, delta));
        }
    }

    fn is_text(&self) -> bool {
        matches!(SECTIONS[self.section], "Overview" | "Logs")
    }

    /// Header, widths and (filtered) rows of a table section; `None` for
    /// text sections. An error string if the section was not collected.
    fn rows(&self) -> Option<TableData> {
        let s = &self.snapshot;
        let (header, widths, mut rows): TableData = match SECTIONS[self.section] {
            "Processes" => {
                let mut list: Vec<_> = s.processes.as_ref()?.list.iter().collect();
                list.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes).then(a.pid.cmp(&b.pid)));
                (
                    vec!["PID", "NAME", "USER", "MEMORY", "CPU%", "STATE", "COMMAND"],
                    vec![
                        Constraint::Length(7),
                        Constraint::Length(16),
                        Constraint::Length(10),
                        Constraint::Length(10),
                        Constraint::Length(6),
                        Constraint::Length(5),
                        Constraint::Min(20),
                    ],
                    list.iter()
                        .map(|p| {
                            vec![
                                p.pid.to_string(),
                                p.name.clone(),
                                p.user.clone().unwrap_or_default(),
                                bytes(p.memory_bytes),
                                p.cpu_percent.map(|c| format!("{c:.1}")).unwrap_or_default(),
                                p.state.clone(),
                                p.cmdline.clone().unwrap_or_default(),
                            ]
                        })
                        .collect(),
                )
            }
            "Ports" => (
                vec!["PROTO", "ADDRESS", "PORT", "PID", "PROCESS"],
                vec![
                    Constraint::Length(6),
                    Constraint::Length(28),
                    Constraint::Length(6),
                    Constraint::Length(8),
                    Constraint::Min(10),
                ],
                s.network
                    .as_ref()?
                    .listening
                    .iter()
                    .map(|l| {
                        vec![
                            l.protocol.clone(),
                            l.address.clone(),
                            l.port.to_string(),
                            l.pid.map(|p| p.to_string()).unwrap_or_default(),
                            l.process.clone().unwrap_or_else(|| "?".into()),
                        ]
                    })
                    .collect(),
            ),
            "Services" => {
                let mut list: Vec<_> = s.services.as_ref()?.iter().collect();
                list.sort_by_key(|svc| (svc.active_state != "failed", svc.name.clone()));
                (
                    vec!["UNIT", "STATE", "RESTARTS", "TYPE", "DESCRIPTION"],
                    vec![
                        Constraint::Min(24),
                        Constraint::Length(22),
                        Constraint::Length(8),
                        Constraint::Length(8),
                        Constraint::Min(10),
                    ],
                    list.iter()
                        .map(|svc| {
                            vec![
                                svc.name.clone(),
                                format!("{} ({})", svc.active_state, svc.sub_state),
                                svc.restarts.map(|r| r.to_string()).unwrap_or_default(),
                                svc.service_type.clone().unwrap_or_default(),
                                svc.description.clone().unwrap_or_default(),
                            ]
                        })
                        .collect(),
                )
            }
            "Containers" => (
                vec!["NAME", "STATE", "HEALTH", "RESTARTS", "MEMORY", "IMAGE", "PORTS"],
                vec![
                    Constraint::Min(16),
                    Constraint::Length(10),
                    Constraint::Length(10),
                    Constraint::Length(8),
                    Constraint::Length(10),
                    Constraint::Min(16),
                    Constraint::Min(10),
                ],
                s.docker
                    .as_ref()?
                    .containers
                    .iter()
                    .map(|c| {
                        vec![
                            c.name.clone(),
                            c.state.clone(),
                            c.health.clone().unwrap_or_else(|| "-".into()),
                            c.restart_count.to_string(),
                            c.memory_bytes.map(bytes).unwrap_or_default(),
                            c.image.clone(),
                            c.ports.join(", "),
                        ]
                    })
                    .collect(),
            ),
            "Disks" => (
                vec!["MOUNT", "USED", "SIZE", "FREE", "FS", "DEVICE", "FLAGS"],
                vec![
                    Constraint::Min(14),
                    Constraint::Length(5),
                    Constraint::Length(10),
                    Constraint::Length(10),
                    Constraint::Length(8),
                    Constraint::Min(12),
                    Constraint::Length(14),
                ],
                s.resources
                    .as_ref()?
                    .disks
                    .iter()
                    .map(|d| {
                        let mut flags = Vec::new();
                        if d.read_only {
                            flags.push("read-only");
                        }
                        if d.unresponsive {
                            flags.push("unresponsive");
                        }
                        vec![
                            d.mount_point.clone(),
                            d.usage_ratio().map(|r| format!("{:.0}%", r * 100.0)).unwrap_or_default(),
                            d.total_bytes.map(bytes).unwrap_or_default(),
                            d.available_bytes.map(bytes).unwrap_or_default(),
                            d.filesystem.clone(),
                            d.device.clone(),
                            flags.join(" "),
                        ]
                    })
                    .collect(),
            ),
            "Env" => (
                vec!["NAME", "VALUE", "SOURCE"],
                vec![Constraint::Min(20), Constraint::Min(30), Constraint::Length(20)],
                s.environment
                    .as_ref()?
                    .variables
                    .iter()
                    .map(|v| vec![v.name.clone(), v.value.clone(), v.source.clone()])
                    .collect(),
            ),
            "Collectors" => (
                vec!["COLLECTOR", "STATUS", "TIME", "DETAIL"],
                vec![Constraint::Length(12), Constraint::Length(8), Constraint::Length(8), Constraint::Min(20)],
                s.capture
                    .collectors
                    .iter()
                    .map(|c| {
                        let mut detail: Vec<String> = c.summary.iter().chain(&c.message).cloned().collect();
                        detail.extend(c.notes.iter().cloned());
                        vec![
                            c.name.clone(),
                            format!("{:?}", c.status).to_lowercase(),
                            format!("{}ms", c.duration_ms),
                            detail.join("; "),
                        ]
                    })
                    .collect(),
            ),
            _ => return None,
        };
        if !self.filter.is_empty() {
            let needle = self.filter.to_lowercase();
            rows.retain(|r| r.iter().any(|c| c.to_lowercase().contains(&needle)));
        }
        Some((header, widths, rows))
    }

    /// The collector behind a section, for "not collected" messages.
    fn collector(&self) -> &'static str {
        match SECTIONS[self.section] {
            "Overview" => "system",
            "Processes" => "processes",
            "Ports" => "network",
            "Services" => "services",
            "Containers" => "docker",
            "Disks" => "resources",
            "Env" => "environment",
            "Logs" => "logs",
            _ => "",
        }
    }

    fn text(&self) -> Option<Vec<Line<'static>>> {
        match SECTIONS[self.section] {
            "Overview" => Some(
                show::overview(&self.snapshot, &crate::style::Style::plain()).into_iter().map(Line::from).collect(),
            ),
            "Logs" => {
                let logs = self.snapshot.logs.as_ref()?;
                let needle = self.filter.to_lowercase();
                let mut lines = Vec::new();
                for src in &logs.sources {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{} ({})", src.name, src.kind), bold()),
                        Span::styled(
                            format!("  {} lines · {} errors · {} warnings", src.total, src.errors, src.warnings),
                            dim(),
                        ),
                    ]));
                    for l in src.lines.iter().filter(|l| needle.is_empty() || l.to_lowercase().contains(&needle)) {
                        let style = match crate::tui::level_of(l) {
                            Some(true) => Style::new().fg(Color::Red),
                            Some(false) => Style::new().fg(Color::Yellow),
                            None => Style::new(),
                        };
                        lines.push(Line::styled(format!("  {l}"), style));
                    }
                    lines.push(Line::raw(""));
                }
                Some(lines)
            }
            _ => None,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [tabs_area, filter_area, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(area);
        let tabs = Tabs::new(SECTIONS.iter().enumerate().map(|(i, s)| format!(" {} {s} ", i + 1)))
            .select(self.section)
            .highlight_style(Style::new().fg(Color::Cyan).add_modifier(ratatui::style::Modifier::BOLD))
            .divider("");
        frame.render_widget(tabs, tabs_area);

        let filter_line = if self.editing {
            Line::from(vec![
                Span::styled(" filter: ", Style::new().fg(Color::Cyan)),
                Span::raw(format!("{}▏", self.filter)),
            ])
        } else if !self.filter.is_empty() {
            Line::from(vec![
                Span::styled(" filter: ", dim()),
                Span::raw(self.filter.clone()),
                Span::styled("  (esc clears)", dim()),
            ])
        } else {
            Line::raw("")
        };
        frame.render_widget(Paragraph::new(filter_line), filter_area);

        if let Some(text) = self.text() {
            let block = Block::bordered().title(format!(" {} ", SECTIONS[self.section]));
            frame.render_widget(
                Paragraph::new(text).block(block).wrap(Wrap { trim: false }).scroll((self.scroll, 0)),
                body,
            );
            return;
        }
        let title = format!(" {} ", SECTIONS[self.section]);
        let Some((header, widths, rows)) = self.rows() else {
            let reason = self
                .snapshot
                .collector(self.collector())
                .and_then(|r| r.message.clone().or_else(|| (r.status == CollectorStatus::Ok).then(|| "empty".into())))
                .unwrap_or_else(|| "not collected".into());
            frame.render_widget(
                Paragraph::new(format!("Not collected: {reason}")).style(dim()).block(Block::bordered().title(title)),
                body,
            );
            return;
        };
        let count = rows.len();
        let table = Table::new(rows.into_iter().map(Row::new), widths)
            .header(Row::new(header).style(dim()))
            .row_highlight_style(selected())
            .block(Block::bordered().title(format!("{title}{count} rows ")));
        frame.render_stateful_widget(table, body, &mut self.table);
    }
}

// --- Diff view --------------------------------------------------------------

impl DiffView {
    fn visible(&self) -> Vec<&Change> {
        self.diff
            .changes
            .iter()
            .filter(|c| c.significance >= self.min && self.category.is_none_or(|cat| c.category == cat))
            .collect()
    }

    fn reselect(&mut self) {
        let len = self.visible().len();
        self.table.select(if len == 0 { None } else { Some(self.table.selected().unwrap_or(0).min(len - 1)) });
    }

    fn key(&mut self, key: KeyEvent) -> ViewFlow {
        if self.detail && matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ')) {
            self.detail = false;
            return ViewFlow::Stay;
        }
        let len = self.visible().len();
        match key.code {
            KeyCode::Char('q') => return ViewFlow::Quit,
            KeyCode::Esc | KeyCode::Backspace => return ViewFlow::Back,
            KeyCode::Up | KeyCode::Char('k') => self.table.select(step(self.table.selected(), len, -1)),
            KeyCode::Down | KeyCode::Char('j') => self.table.select(step(self.table.selected(), len, 1)),
            KeyCode::PageUp => self.table.select(step(self.table.selected(), len, -15)),
            KeyCode::PageDown => self.table.select(step(self.table.selected(), len, 15)),
            KeyCode::Enter => self.detail = len > 0,
            KeyCode::Char('m') => {
                self.min = match self.min {
                    Significance::Low => Significance::Medium,
                    Significance::Medium => Significance::High,
                    Significance::High => Significance::Info,
                    Significance::Info => Significance::Low,
                };
                self.reselect();
            }
            KeyCode::Char('c') => {
                let mut present: Vec<Category> = self.diff.changes.iter().map(|c| c.category).collect();
                present.sort();
                present.dedup();
                self.category = match self.category {
                    None => present.first().copied(),
                    Some(cat) => present.iter().position(|c| *c == cat).and_then(|i| present.get(i + 1)).copied(),
                };
                self.reselect();
            }
            _ => {}
        }
        ViewFlow::Stay
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let notes = self.diff.notes.len().min(3) as u16;
        let [head, filter_area, body] =
            Layout::vertical([Constraint::Length(2 + notes), Constraint::Length(1), Constraint::Min(0)]).areas(area);

        let s = self.diff.summary;
        let elapsed = (self.diff.to.captured_at - self.diff.from.captured_at).num_seconds();
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!(" {} ", self.diff.from.name), bold()),
                Span::styled(self.diff.from.captured_at.format("%Y-%m-%d %H:%M:%S").to_string(), dim()),
                Span::raw("  →  "),
                Span::styled(format!("{} ", self.diff.to.name), bold()),
                Span::styled(self.diff.to.captured_at.format("%Y-%m-%d %H:%M:%S").to_string(), dim()),
                Span::styled(format!("  ({} later)", duration(elapsed.max(0) as u64)), dim()),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{} high", s.high), sig_style(Significance::High)),
                Span::raw(" · "),
                Span::styled(format!("{} medium", s.medium), sig_style(Significance::Medium)),
                Span::raw(" · "),
                Span::styled(format!("{} low", s.low), sig_style(Significance::Low)),
                Span::raw(" · "),
                Span::styled(format!("{} info", s.info), sig_style(Significance::Info)),
            ]),
        ];
        for note in self.diff.notes.iter().take(3) {
            lines.push(Line::styled(format!(" ⚠ {note}"), Style::new().fg(Color::Yellow)));
        }
        frame.render_widget(Paragraph::new(lines), head);

        let visible: Vec<Change> = self.visible().into_iter().cloned().collect();
        let filter = format!(
            " showing {} and above · category: {} · {} of {}",
            self.min.label(),
            self.category.map(|c| c.label()).unwrap_or("all"),
            visible.len(),
            self.diff.changes.len()
        );
        frame.render_widget(Paragraph::new(filter).style(dim()), filter_area);

        if visible.is_empty() {
            let msg = if self.diff.changes.is_empty() {
                "No differences found."
            } else {
                "Nothing matches the filter (m: minimum level, c: category)."
            };
            frame.render_widget(Paragraph::new(msg).block(Block::bordered()), body);
            return;
        }
        let rows: Vec<Row> = visible
            .iter()
            .map(|c| {
                Row::new(vec![
                    Cell::from(c.significance.label()).style(sig_style(c.significance)),
                    Cell::from(c.category.label()).style(dim()),
                    Cell::from(c.subject.clone()).style(bold()),
                    Cell::from(c.field.clone().unwrap_or_default()),
                    Cell::from(change_value(c)),
                    Cell::from(c.delta.clone().unwrap_or_default()).style(dim()),
                ])
            })
            .collect();
        let widths = [
            Constraint::Length(6),
            Constraint::Length(13),
            Constraint::Min(18),
            Constraint::Length(18),
            Constraint::Min(30),
            Constraint::Length(20),
        ];
        let table = Table::new(rows, widths)
            .header(Row::new(["LEVEL", "CATEGORY", "SUBJECT", "FIELD", "CHANGE", "DELTA"]).style(dim()))
            .row_highlight_style(selected())
            .block(Block::bordered());
        frame.render_stateful_widget(table, body, &mut self.table);

        if self.detail {
            if let Some(c) = self.table.selected().and_then(|i| visible.get(i)) {
                draw_change_detail(frame, c);
            }
        }
    }
}

fn draw_change_detail(frame: &mut Frame, c: &Change) {
    let inner = popup(frame, frame.area(), 90, 16, c.significance.label());
    let label = |l: &str| Span::styled(format!("{l:<9}"), dim());
    let mut lines = vec![
        Line::from(vec![label("subject"), Span::styled(c.subject.clone(), bold())]),
        Line::from(vec![label("field"), Span::raw(c.field.clone().unwrap_or_default())]),
        Line::from(vec![label("category"), Span::raw(c.category.label())]),
        Line::from(vec![label("rule"), Span::styled(c.rule.clone(), Style::new().fg(Color::Cyan))]),
        Line::from(vec![label("key"), Span::styled(c.key.clone(), dim())]),
        Line::raw(""),
    ];
    if let Some(b) = &c.before {
        lines.push(Line::from(vec![label("before"), Span::raw(b.clone())]));
    }
    if let Some(a) = &c.after {
        lines.push(Line::from(vec![label("after"), Span::styled(a.clone(), bold())]));
    }
    if let Some(d) = &c.delta {
        lines.push(Line::from(vec![label("change"), Span::raw(d.clone())]));
    }
    lines.push(Line::raw(""));
    if let Some(p) = &c.policy {
        lines.push(Line::from(vec![
            label("policy"),
            Span::styled(
                format!("level set by {} (default {})", p.matched, p.default.label()),
                Style::new().fg(Color::Yellow),
            ),
        ]));
    }
    lines.push(Line::styled("Evidence, not a cause. esc closes.", dim()));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style as TextStyle;
    use crate::tui::fixtures::{broken, snapshot};
    use crate::tui::testing::{char, key, render};
    use hostprint_storage::Store;

    fn app(tag: &str) -> App {
        let root = std::env::temp_dir().join(format!("hostprint-tui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        App { store: Store::new(root), style: TextStyle::plain(), err_style: TextStyle::plain(), policy_file: None }
    }

    #[test]
    fn lists_newest_first_and_compares() {
        let app = app("compare");
        app.store.save(&snapshot("healthy", 0), false).unwrap();
        app.store.save(&broken("broken", 600), false).unwrap();
        let mut b = Browser::new(&app, Config::default()).unwrap();
        let screen = render(&mut b, 100, 12);
        assert!(screen.contains("Snapshots (2)"), "{screen}");
        let broken_row = screen.lines().position(|l| l.contains("broken")).unwrap();
        let healthy_row = screen.lines().position(|l| l.contains("healthy")).unwrap();
        assert!(broken_row < healthy_row, "newest first:\n{screen}");

        // `c` with nothing marked compares the next older snapshot with the selected one.
        assert_eq!(b.key(char('c')), Flow::Continue);
        let screen = render(&mut b, 120, 16);
        assert!(screen.contains("healthy → broken"), "{screen}");
        assert!(screen.contains("restart count"), "{screen}");
        assert!(screen.contains("0 → 17"), "{screen}");
        assert!(screen.lines().any(|l| l.contains("HIGH") && l.contains("nginx.service")), "{screen}");

        // Filters: only HIGH, then only containers.
        b.key(char('m'));
        b.key(char('m'));
        let screen = render(&mut b, 120, 16);
        assert!(screen.contains("showing HIGH and above"), "{screen}");
        b.key(char('c'));
        let screen = render(&mut b, 120, 16);
        assert!(screen.contains("category: SERVICES") || screen.contains("category: CONTAINERS"), "{screen}");

        // Details, then back to the list.
        b.key(key(KeyCode::Enter));
        let screen = render(&mut b, 120, 24);
        assert!(screen.contains("rule"), "{screen}");
        b.key(key(KeyCode::Esc));
        b.key(key(KeyCode::Esc));
        assert!(matches!(b.screen, Screen::List));
        let _ = std::fs::remove_dir_all(app.store.root());
    }

    #[test]
    fn marking_sets_the_from_side() {
        let app = app("mark");
        app.store.save(&snapshot("a", 0), false).unwrap();
        app.store.save(&snapshot("b", 60), false).unwrap();
        app.store.save(&broken("c", 120), false).unwrap();
        let mut b = Browser::new(&app, Config::default()).unwrap();
        // Rows: c, b, a. Mark a, select c, compare: a → c.
        b.key(key(KeyCode::End));
        b.key(char(' '));
        b.key(key(KeyCode::Home));
        b.key(char('c'));
        let Screen::Diff(view) = &b.screen else { panic!("expected the diff view") };
        assert_eq!((view.diff.from.name.as_str(), view.diff.to.name.as_str()), ("a", "c"));
        let _ = std::fs::remove_dir_all(app.store.root());
    }

    #[test]
    fn snapshot_view_sections_and_filter() {
        let app = app("view");
        app.store.save(&snapshot("s", 0), false).unwrap();
        let mut b = Browser::new(&app, Config::default()).unwrap();
        b.key(key(KeyCode::Enter));
        let screen = render(&mut b, 120, 30);
        assert!(screen.contains("SNAPSHOT s"), "{screen}");

        b.key(char('5')); // Containers
        let screen = render(&mut b, 120, 20);
        assert!(screen.contains("api") && screen.contains("redis"), "{screen}");
        b.key(char('/'));
        for c in "red".chars() {
            b.key(char(c));
        }
        b.key(key(KeyCode::Enter));
        let screen = render(&mut b, 120, 20);
        assert!(screen.contains("1 rows"), "{screen}");
        assert!(!screen.lines().any(|l| l.contains(" api ")), "{screen}");

        b.key(char('3')); // Ports: not collected in the fixture
        let screen = render(&mut b, 120, 20);
        assert!(screen.contains("Not collected"), "{screen}");

        b.key(key(KeyCode::Esc));
        assert!(matches!(b.screen, Screen::List));
        let _ = std::fs::remove_dir_all(app.store.root());
    }

    #[test]
    fn bundles_from_the_diff_view() {
        let app = app("bundle");
        app.store.save(&snapshot("healthy", 0), false).unwrap();
        app.store.save(&broken("broken", 600), false).unwrap();
        let mut b = Browser::new(&app, Config::default()).unwrap();
        b.key(char('c'));
        let dir = std::env::current_dir().unwrap();
        b.key(char('b'));
        let written = dir.join("broken-20260921-142320.tar.gz");
        assert!(written.exists(), "bundle not at {}", written.display());
        let screen = render(&mut b, 120, 16);
        assert!(screen.contains("Bundle written"), "{screen}");
        std::fs::remove_file(written).unwrap();
        let _ = std::fs::remove_dir_all(app.store.root());
    }

    #[test]
    fn empty_store_explains_what_to_do() {
        let app = app("empty");
        let mut b = Browser::new(&app, Config::default()).unwrap();
        let screen = render(&mut b, 100, 10);
        assert!(screen.contains("Press s to capture one now"), "{screen}");
        b.key(key(KeyCode::Tab));
        let screen = render(&mut b, 100, 10);
        assert!(screen.contains("No baselines yet"), "{screen}");
        assert_eq!(b.key(char('q')), Flow::Quit);
    }
}
