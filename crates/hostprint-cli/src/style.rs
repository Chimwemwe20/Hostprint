//! Terminal styling. Colour is used only on terminals, never when NO_COLOR is
//! set, and never in JSON output.

use std::io::IsTerminal;
use std::path::Path;

#[derive(Clone, Copy)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy)]
pub struct Style {
    color: bool,
}

impl Style {
    pub fn detect(disabled: bool, stream: Stream) -> Style {
        let terminal = match stream {
            Stream::Stdout => std::io::stdout().is_terminal(),
            Stream::Stderr => std::io::stderr().is_terminal(),
        };
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        Style { color: terminal && !disabled && !no_color }
    }

    /// No colour, for files and reports.
    pub fn plain() -> Style {
        Style { color: false }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn red_bold(&self, s: &str) -> String {
        self.paint("1;31", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn yellow_bold(&self, s: &str) -> String {
        self.paint("1;33", s)
    }
    pub fn cyan(&self, s: &str) -> String {
        self.paint("36", s)
    }
    pub fn blue_bold(&self, s: &str) -> String {
        self.paint("1;34", s)
    }
}

/// Pads `s` to `width` display columns (counting chars, which is right for
/// the ASCII-heavy content Hostprint prints).
pub fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// "1 error", "2 errors".
pub fn plural(n: impl Into<u64>, word: &str) -> String {
    let n = n.into();
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Shortens `s` to at most `max` chars, marking the cut with `…`.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Displays a path with the home directory abbreviated to `~`.
pub fn tilde(path: &Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => format!("~{}", &shown[home.len()..]),
        _ => shown,
    }
}
