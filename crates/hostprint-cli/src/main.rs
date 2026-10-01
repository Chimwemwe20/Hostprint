mod capture;
mod diff;
mod doctor;
mod list;
mod show;
mod style;

use anyhow::{Context as _, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use hostprint_collectors::redact::Redactor;
use hostprint_collectors::CaptureContext;
use hostprint_core::Config;
use hostprint_diff::Significance;
use hostprint_storage::Store;
use std::path::PathBuf;
use std::process::ExitCode;
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
    /// Delete a stored snapshot
    Delete {
        /// Snapshot name
        name: String,
    },
    /// Check what Hostprint can observe on this machine
    Doctor,
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
    /// Git repository to record [default: current directory]
    #[arg(long, value_name = "DIR")]
    pub repo: Option<PathBuf>,
    /// Also record variables from this dotenv file (repeatable)
    #[arg(long = "env-file", value_name = "FILE")]
    pub env_files: Vec<PathBuf>,
    /// Also fingerprint this file (repeatable)
    #[arg(long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,
    /// Only print the snapshot name
    #[arg(short, long)]
    pub quiet: bool,
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

#[derive(Args)]
pub struct DiffArgs {
    /// Baseline snapshot (name or file)
    pub from: String,
    /// Snapshot to compare with [default: capture the current state, without saving]
    pub to: Option<String>,
    /// Print the diff as JSON
    #[arg(long)]
    pub json: bool,
    /// Show every change, including INFO
    #[arg(short, long)]
    pub all: bool,
    /// Lowest significance to show
    #[arg(long, value_enum, value_name = "LEVEL", default_value = "low")]
    pub min: Level,
    /// Exit with status 1 if any change is at least this significant
    #[arg(long, value_enum, value_name = "LEVEL")]
    pub fail_on: Option<Level>,
    /// Git repository to record for a live comparison [default: current directory]
    #[arg(long, value_name = "DIR")]
    pub repo: Option<PathBuf>,
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

    /// A capture context configured from `config.toml`.
    pub fn capture_context(&self, config: &Config, repo: Option<PathBuf>) -> Result<CaptureContext> {
        let key = self.store.fingerprint_key().context("loading the secret fingerprint key")?;
        let redactor = Redactor::new(&key)
            .enabled(config.hostprint.redact_secrets)
            .with_words(config.redact.patterns.iter().cloned())
            .with_allowed(config.redact.allow.iter().cloned());
        let mut ctx = CaptureContext::new(redactor);
        if let Some(repo) = repo {
            ctx.repo_dir = repo;
        }
        ctx.capture_process_env = config.env.capture_process;
        ctx.env_files = config.env.files.clone();
        ctx.file_paths = config.files.paths.clone();
        Ok(ctx)
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
        Command::List { json } => list::run(&app, json),
        Command::Show(args) => show::run(&app, args),
        Command::Diff(args) => diff::run(&app, args),
        Command::Delete { name } => {
            let path = app.store.delete(&name)?;
            println!("Deleted {} ({})", name, style::tilde(&path));
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor => doctor::run(&app),
    }
}
