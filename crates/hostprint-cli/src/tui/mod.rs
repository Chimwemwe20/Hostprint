//! Interactive terminal UI: `hostprint tui` (browse and compare snapshots) and
//! `hostprint watch` (a live dashboard).
//!
//! Each screen is a [`View`]: plain state plus `key` and `draw`. Keeping state
//! and drawing apart lets tests drive a view with key events and check what
//! it renders on ratatui's in-memory backend. Captures run on a worker thread
//! ([`Job`]) so a slow collector never freezes the interface.

mod browser;
#[cfg(test)]
mod fixtures;
mod watch;

pub use browser::run as browse;
pub use watch::run as watch;

use crate::{App, CaptureOptions};
use anyhow::{bail, Result};
use hostprint_core::Config;
use hostprint_diff::{Change, ChangeKind, Significance};
use hostprint_model::Snapshot;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::{cursor, execute};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::{IsTerminal, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long a status message stays in the footer.
const STATUS_TTL: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    Continue,
    Quit,
}

pub(crate) trait View {
    /// Runs before every frame: collect finished background work, start
    /// scheduled work.
    fn tick(&mut self) {}
    fn draw(&mut self, frame: &mut Frame);
    fn key(&mut self, key: KeyEvent) -> Flow;
}

/// Runs a view until it quits. Ctrl-C always quits.
pub(crate) fn run_view(view: &mut impl View) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("the terminal UI needs an interactive terminal");
    }
    let mut screen = Screen::enter()?;
    loop {
        view.tick();
        screen.terminal.draw(|frame| view.draw(frame))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                break;
            }
            if view.key(key) == Flow::Quit {
                break;
            }
        }
    }
    Ok(())
}

/// The alternate screen in raw mode, restored on drop and on panic.
struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Screen {
    fn enter() -> Result<Screen> {
        terminal::enable_raw_mode()?;
        if let Err(e) = execute!(std::io::stdout(), EnterAlternateScreen) {
            restore();
            return Err(e.into());
        }
        // Collector panics on worker threads are caught and reported in the
        // snapshot; printing them would scribble over the screen. A panic on
        // the main thread restores the terminal before reporting.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() == Some("main") {
                restore();
                previous(info);
            }
        }));
        Ok(Screen { terminal: Terminal::new(CrosstermBackend::new(std::io::stdout()))? })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
}

/// Work running on a background thread.
pub(crate) struct Job<T> {
    rx: mpsc::Receiver<T>,
    pub started: Instant,
}

impl<T: Send + 'static> Job<T> {
    pub fn spawn(work: impl FnOnce() -> T + Send + 'static) -> Job<T> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        Job { rx, started: Instant::now() }
    }

    /// The result once the work has finished; an error if the worker died.
    pub fn poll(&self) -> Option<Result<T, String>> {
        match self.rx.try_recv() {
            Ok(v) => Some(Ok(v)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("the capture stopped unexpectedly".into())),
        }
    }
}

/// Starts a capture of the live system on a worker thread.
pub(crate) fn capture_job(app: &App, config: &Config, options: &CaptureOptions, name: String) -> Result<Job<Snapshot>> {
    let (ctx, collectors) = app.prepare_capture(config, options)?;
    Ok(Job::spawn(move || hostprint_core::capture(&name, &ctx, &collectors)))
}

/// A footer message that expires.
pub(crate) struct Status {
    text: String,
    error: bool,
    at: Instant,
}

impl Status {
    pub fn info(text: impl Into<String>) -> Option<Status> {
        Some(Status { text: text.into(), error: false, at: Instant::now() })
    }

    pub fn error(text: impl Into<String>) -> Option<Status> {
        Some(Status { text: text.into(), error: true, at: Instant::now() })
    }
}

// --- Drawing helpers --------------------------------------------------------

pub(crate) fn sig_style(sig: Significance) -> Style {
    match sig {
        Significance::High => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        Significance::Medium => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        Significance::Low => Style::new().fg(Color::Cyan),
        Significance::Info => Style::new().fg(Color::DarkGray),
    }
}

pub(crate) fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

pub(crate) fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub(crate) fn selected() -> Style {
    Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
}

/// "before → after", "+ after" or "− before".
pub(crate) fn change_value(c: &Change) -> String {
    let v = |s: &Option<String>| s.clone().unwrap_or_default();
    match c.kind {
        ChangeKind::Changed => format!("{} → {}", v(&c.before), v(&c.after)),
        ChangeKind::Added => format!("+ {}", v(&c.after)),
        ChangeKind::Removed => format!("− {}", v(&c.before)),
    }
}

/// The one-line title bar.
pub(crate) fn draw_title(frame: &mut Frame, area: Rect, title: &str, detail: &str) {
    let line = Line::from(vec![
        Span::styled(" HOSTPRINT ", Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {title} "), bold()),
        Span::styled(detail.to_string(), dim()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// The footer: a recent status message, or key hints.
pub(crate) fn draw_footer(frame: &mut Frame, area: Rect, hints: &[(&str, &str)], status: &mut Option<Status>) {
    if status.as_ref().is_some_and(|s| s.at.elapsed() > STATUS_TTL) {
        *status = None;
    }
    let line = match status {
        Some(s) => {
            let color = if s.error { Color::Red } else { Color::Green };
            Line::from(Span::styled(format!(" {}", s.text), Style::new().fg(color)))
        }
        None => Line::from(
            hints
                .iter()
                .flat_map(|(key, what)| {
                    [
                        Span::styled(format!(" {key} "), Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        Span::styled(format!("{what} "), dim()),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// A centered box of the given size (clamped to `area`), cleared for a popup.
pub(crate) fn popup(frame: &mut Frame, area: Rect, width: u16, height: u16, title: &str) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))]).flex(Flex::Center).areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width.min(area.width))]).flex(Flex::Center).areas(row);
    frame.render_widget(Clear, rect);
    let block = Block::bordered().title(format!(" {title} ")).border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

/// A help popup listing keys.
pub(crate) fn draw_help(frame: &mut Frame, keys: &[(&str, &str)]) {
    let height = keys.len() as u16 + 4;
    let inner = popup(frame, frame.area(), 56, height, "Keys");
    let lines: Vec<Line> = keys
        .iter()
        .map(|(k, what)| {
            Line::from(vec![
                Span::styled(format!("{k:>12}  "), Style::new().fg(Color::Cyan)),
                Span::raw(what.to_string()),
            ])
        })
        .chain([Line::raw(""), Line::styled("any key closes this", dim())])
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Some(true) for an error-looking log line, Some(false) for a warning.
pub(crate) fn level_of(line: &str) -> Option<bool> {
    let lower = line.to_ascii_lowercase();
    const ERRORS: [&str; 8] = [" err ", " crit ", " alert ", " emerg ", "error", "fatal", "panic", "exception"];
    if ERRORS.iter().any(|w| lower.contains(w)) {
        Some(true)
    } else if lower.contains("warn") {
        Some(false)
    } else {
        None
    }
}

/// Moves a selection by `delta` within `len` items.
pub(crate) fn step(selected: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = selected.unwrap_or(0) as isize;
    Some((current + delta).clamp(0, len as isize - 1) as usize)
}

#[cfg(test)]
pub(crate) mod testing {
    use super::View;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    pub fn char(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    /// Renders a view into a `width`×`height` buffer and returns its text.
    pub fn render(view: &mut impl View, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| view.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_selection_within_bounds() {
        assert_eq!(step(None, 0, 1), None);
        assert_eq!(step(None, 3, 1), Some(1));
        assert_eq!(step(Some(2), 3, 1), Some(2));
        assert_eq!(step(Some(0), 3, -5), Some(0));
        assert_eq!(step(Some(1), 3, 10), Some(2));
    }

    #[test]
    fn jobs_report_results() {
        let job = Job::spawn(|| 42);
        let started = Instant::now();
        loop {
            if let Some(result) = job.poll() {
                assert_eq!(result, Ok(42));
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
