mod baseline;
mod bundle;
mod capture;
mod diff;
mod doctor;
mod html;
mod list;
mod report;
mod show;
mod style;
#[cfg(feature = "tui")]
mod tui;

use anyhow::{Context as _, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use hostprint_collectors::logs::LogOptions;
use hostprint_collectors::redact::Redactor;
use hostprint_collectors::{CaptureContext, Collector};
use hostprint_core::config::parse_duration;
use hostprint_core::Config;
use hostprint_diff::Significance;
use hostprint_storage::Store;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use style::Style;

#[derive(Parser)]
#[command(
    name = "hostprint",
    version,
    about = "Fingerprint your system. See what changed.",
    after_help = "Example:\n  hostprint capture --name healthy\n  # ...later, when something is wrong\n  hostprint capture --name broken\n  hostprint diff healthy broken"
)]
struct Cli {
    /// Storage directory [default: $HOSTPRINT_HOME or ~/.hostprint]
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,

    /// Disable colored output (also honours NO_COLOR)
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Capture the current system state as a snapshot
    Capture(CaptureArgs),
    /// List stored snapshots
    List {
        /// Print as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show a stored snapshot
    Show(ShowArgs),
    /// Compare two snapshots, or a snapshot with the system as it is now
    Diff(DiffArgs),
    /// Package a snapshot (and optionally its diff) for an issue or ticket
    Bundle(BundleArgs),
    /// Write a standalone HTML (or Markdown) report of a snapshot or a comparison
    Report(ReportArgs),
    /// Manage known-good baselines for `hostprint check`
    #[command(subcommand)]
    Baseline(BaselineCommand),
    /// Compare the system as it is now with a baseline
    Check(CheckArgs),
    /// Write a snapshot to a file for sharing
    Export(ExportArgs),
    /// Delete a stored snapshot
    Delete {
        /// Snapshot name
        name: String,
    },
    /// Check what Hostprint can observe on this machine
    Doctor,
    /// Browse, compare and bundle snapshots in an interactive terminal UI
    #[cfg(feature = "tui")]
    Tui,
    /// Live dashboard: capture on an interval and show what changes
    #[cfg(feature = "tui")]
    Watch(WatchArgs),
}

#[cfg(feature = "tui")]
#[derive(Args)]
pub struct WatchArgs {
    /// Time between captures, e.g. 10s or 1m
    #[arg(long, default_value = "10s", value_name = "DURATION")]
    pub interval: String,
    /// Compare with this baseline instead of the first capture
    #[arg(long, value_name = "NAME")]
    pub baseline: Option<String>,
    #[command(flatten)]
    pub options: CaptureOptions,
}

/// Options shared by every command that captures the live system.
#[derive(Args, Clone, Default)]
pub struct CaptureOptions {
    /// Git repository to record [default: current directory]
    #[arg(long, value_name = "DIR")]
    pub repo: Option<PathBuf>,
    /// Also record variables from this dotenv file (repeatable)
    #[arg(long = "env-file", value_name = "FILE")]
    pub env_files: Vec<PathBuf>,
    /// Also fingerprint this file (repeatable)
    #[arg(long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,
    /// Collect logs from this far back, e.g. 30m or 2h
    #[arg(long, value_name = "DURATION")]
    pub logs_since: Option<String>,
    /// Run only these collectors (comma-separated)
    #[arg(long, value_delimiter = ',', value_name = "NAMES")]
    pub only: Vec<String>,
    /// Skip these collectors (comma-separated)
    #[arg(long, value_delimiter = ',', value_name = "NAMES")]
    pub skip: Vec<String>,
}

#[derive(Args)]
pub struct CaptureArgs {
    /// Snapshot name [default: snap-YYYYMMDD-HHMMSS]
    #[arg(short, long)]
    pub name: Option<String>,
    /// Replace an existing snapshot with the same name
    #[arg(short, long)]
    pub force: bool,
    /// Print the snapshot as JSON on stdout (progress goes to stderr)
    #[arg(long)]
    pub json: bool,
    /// Do not store the snapshot
    #[arg(long)]
    pub no_save: bool,
    /// Only print the snapshot name
    #[arg(short, long)]
    pub quiet: bool,
    #[command(flatten)]
    pub options: CaptureOptions,
}

#[derive(Args)]
pub struct ShowArgs {
    /// Snapshot name or path to a snapshot file
    pub snapshot: String,
    /// Print the full snapshot as JSON
    #[arg(long)]
    pub json: bool,
    /// List one section in full
    #[arg(long, value_enum)]
    pub section: Option<show::SectionArg>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Text,
    Json,
    Markdown,
}

/// How a diff is printed and when it fails.
#[derive(Args)]
pub struct DiffOutput {
    /// Output format
    #[arg(long, value_enum, default_value = "text")]
    pub format: Format,
    /// Shorthand for --format json
    #[arg(long)]
    pub json: bool,
    /// Show every change, including INFO
    #[arg(short, long)]
    pub all: bool,
    /// Lowest significance to show
    #[arg(long, value_enum, value_name = "LEVEL", default_value = "low")]
    pub min: Level,
}

impl DiffOutput {
    pub fn format(&self) -> Format {
        if self.json {
            Format::Json
        } else {
            self.format
        }
    }

    pub fn min(&self) -> Significance {
        if self.all {
            Significance::Info
        } else {
            self.min.into()
        }
    }
}

#[derive(Args)]
pub struct DiffArgs {
    /// Baseline snapshot (name or file)
    pub from: String,
    /// Snapshot to compare with [default: capture the current state, without saving]
    pub to: Option<String>,
    #[command(flatten)]
    pub output: DiffOutput,
    /// Exit with status 1 if any change is at least this significant
    #[arg(long, value_enum, value_name = "LEVEL")]
    pub fail_on: Option<Level>,
    #[command(flatten)]
    pub options: CaptureOptions,
}

#[derive(Args)]
pub struct BundleArgs {
    /// Snapshot to bundle (name or file) [default: capture the current state]
    pub snapshot: Option<String>,
    /// Include a diff against this snapshot (name or file)
    #[arg(long, value_name = "SNAPSHOT")]
    pub against: Option<String>,
    /// Output file [default: <name>-<timestamp>.tar.gz in the current directory]
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    #[command(flatten)]
    pub options: CaptureOptions,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    Html,
    Markdown,
}

#[derive(Args)]
pub struct ReportArgs {
    /// Snapshot to report on (name or file); with TO, the "from" side
    pub snapshot: String,
    /// Report the changes from SNAPSHOT to this snapshot (name or file)
    pub to: Option<String>,
    /// Output format
    #[arg(long, value_enum, default_value = "html")]
    pub format: ReportFormat,
    /// Shorthand for --format html
    #[arg(long)]
    pub html: bool,
    /// Output file [default: <name>.html in the current directory; "-" for stdout]
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Replace an existing file
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Subcommand)]
pub enum BaselineCommand {
    /// Record a known-good state, from a new capture or an existing snapshot
    Create {
        /// Baseline name, e.g. production
        name: String,
        /// Use this stored snapshot (name or file) instead of capturing now
        #[arg(long, value_name = "SNAPSHOT")]
        from: Option<String>,
        /// Replace an existing baseline with the same name
        #[arg(short, long)]
        force: bool,
        #[command(flatten)]
        options: CaptureOptions,
    },
    /// List baselines
    List {
        /// Print as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show a baseline
    Show {
        name: String,
        /// Print as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a baseline
    Delete { name: String },
}

#[derive(Args)]
pub struct CheckArgs {
    /// Baseline to compare with
    pub baseline: String,
    #[command(flatten)]
    pub output: DiffOutput,
    /// Exit with status 1 if any change is at least this significant
    #[arg(long, value_enum, value_name = "LEVEL", default_value = "medium")]
    pub fail_on: Level,
    #[command(flatten)]
    pub options: CaptureOptions,
}

#[derive(Args)]
pub struct ExportArgs {
    /// Snapshot to export (name)
    pub snapshot: String,
    /// Output file [default: <name>.hostprint in the current directory]
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Replace an existing file
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Level {
    Info,
    Low,
    Medium,
    High,
}

impl From<Level> for Significance {
    fn from(l: Level) -> Significance {
        match l {
            Level::Info => Significance::Info,
            Level::Low => Significance::Low,
            Level::Medium => Significance::Medium,
            Level::High => Significance::High,
        }
    }
}

/// Shared setup for every command.
pub struct App {
    pub store: Store,
    pub style: Style,
    pub err_style: Style,
}

impl App {
    pub fn config(&self) -> Result<Config> {
        Ok(Config::load(&self.store.config_path())?)
    }

    /// The context and collectors for a capture, from `config.toml` and the
    /// command-line options.
    pub fn prepare_capture(
        &self,
        config: &Config,
        options: &CaptureOptions,
    ) -> Result<(CaptureContext, Vec<Arc<dyn Collector>>)> {
        let key = self.store.fingerprint_key().context("loading the secret fingerprint key")?;
        let redactor = Redactor::new(&key)
            .enabled(config.hostprint.redact_secrets)
            .with_words(config.redact.patterns.iter().cloned())
            .with_allowed(config.redact.allow.iter().cloned());
        let mut ctx = CaptureContext::new(redactor);
        if let Some(repo) = &options.repo {
            ctx.repo_dir = repo.clone();
        }
        ctx.capture_process_env = config.env.capture_process;
        ctx.env_files = config.env.files.iter().chain(&options.env_files).cloned().collect();
        ctx.file_paths = config.files.paths.iter().chain(&options.files).cloned().collect();

        let since = match (&options.logs_since, config.hostprint.collect_logs) {
            (Some(since), _) => Some(parse_duration(since).map_err(anyhow::Error::msg)?),
            (None, true) => Some(
                parse_duration(&config.logs.since)
                    .map_err(|e| anyhow::anyhow!("[logs] since in {}: {e}", self.store.config_path().display()))?,
            ),
            (None, false) => None,
        };
        ctx.logs = since.map(|since| LogOptions {
            since: chrono::Utc::now() - chrono::Duration::from_std(since).unwrap_or(chrono::Duration::zero()),
            journal: config.logs.journal,
            docker: config.logs.docker,
            files: config.logs.files.clone(),
            max_lines: config.logs.lines.max(1),
        });

        let skip: Vec<String> = config.collectors.disable.iter().chain(&options.skip).cloned().collect();
        let collectors = hostprint_collectors::select(hostprint_collectors::default_collectors(), &options.only, &skip)
            .map_err(anyhow::Error::msg)?;
        Ok((ctx, collectors))
    }

    pub fn diff_options(config: &Config) -> hostprint_diff::DiffOptions {
        hostprint_diff::DiffOptions {
            ignore_processes: config.ignore.processes.clone(),
            ignore_ports: config.ignore.ports.clone(),
            ignore_env: config.ignore.env.clone(),
            ignore_containers: config.ignore.containers.clone(),
            ignore_services: config.ignore.services.clone(),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let style = Style::detect(cli.no_color, style::Stream::Stdout);
    let err_style = Style::detect(cli.no_color, style::Stream::Stderr);
    match run(cli, style, err_style) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{} {err:#}", err_style.red_bold("error:"));
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli, style: Style, err_style: Style) -> Result<ExitCode> {
    let root = match cli.home {
        Some(home) => home,
        None => Store::default_root()?,
    };
    let app = App { store: Store::new(root), style, err_style };
    match cli.command {
        Command::Capture(args) => capture::run(&app, args),
        Command::List { json } => list::run(&app, hostprint_storage::Kind::Snapshot, json),
        Command::Show(args) => show::run(&app, args),
        Command::Diff(args) => diff::run(&app, args),
        Command::Bundle(args) => bundle::run(&app, args),
        Command::Report(args) => report::run(&app, args),
        Command::Baseline(cmd) => baseline::run(&app, cmd),
        Command::Check(args) => baseline::check(&app, args),
        Command::Export(args) => {
            let snapshot = app.store.load(&args.snapshot)?;
            let path = args
                .output
                .unwrap_or_else(|| PathBuf::from(format!("{}.{}", snapshot.name, hostprint_storage::EXPORT_EXTENSION)));
            app.store.export(&snapshot, &path, args.force)?;
            println!("Exported {} to {}", snapshot.name, path.display());
            eprintln!(
                "{}",
                app.err_style
                    .dim("Secrets are redacted, but review hostnames, addresses and command lines before sharing.")
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Delete { name } => {
            let path = app.store.delete(&name)?;
            println!("Deleted {} ({})", name, style::tilde(&path));
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor => doctor::run(&app),
        #[cfg(feature = "tui")]
        Command::Tui => tui::browse(&app),
        #[cfg(feature = "tui")]
        Command::Watch(args) => tui::watch(&app, args),
    }
}
