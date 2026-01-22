mod deps;
mod env_cmd;
mod filter;
mod git;
mod json_cmd;
mod local_llm;
mod ls;
mod read;
mod runner;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rtk",
    version,
    about = "Rust Token Killer - Minimize LLM token consumption",
    long_about = "A high-performance CLI proxy designed to filter and summarize system outputs before they reach your LLM context."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Verbosity level (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Commands {
    /// List directory contents in ultra-dense, token-optimized format
    Ls {
        /// Path to list (defaults to current directory)
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Maximum depth to traverse
        #[arg(short, long, default_value = "10")]
        depth: usize,

        /// Show hidden files (except .git, node_modules, etc.)
        #[arg(short = 'a', long)]
        all: bool,

        /// Output format: tree, flat, json
        #[arg(short, long, default_value = "tree")]
        format: ls::OutputFormat,
    },

    /// Read file with intelligent filtering
    Read {
        /// File to read
        file: PathBuf,

        /// Filter level: none, minimal, aggressive
        #[arg(short, long, default_value = "minimal")]
        level: filter::FilterLevel,

        /// Maximum lines to output (smart truncation)
        #[arg(short, long)]
        max_lines: Option<usize>,

        /// Show line numbers
        #[arg(short = 'n', long)]
        line_numbers: bool,
    },

    /// Generate 2-line technical summary (heuristic-based, no LLM)
    Smart {
        /// File to summarize
        file: PathBuf,

        /// Model (ignored, kept for compatibility)
        #[arg(short, long, default_value = "heuristic")]
        model: String,

        /// Force re-download (ignored)
        #[arg(long)]
        force_download: bool,
    },

    /// Git commands with compact output
    Git {
        #[command(subcommand)]
        command: GitCommands,
    },

    /// Run command and show only errors/warnings
    Err {
        /// Command to run
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Run tests and show only failures
    Test {
        /// Test command to run (e.g., "cargo test", "pytest", "npm test")
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Show JSON structure without values
    Json {
        /// JSON file to analyze
        file: PathBuf,

        /// Maximum depth to show
        #[arg(short, long, default_value = "5")]
        depth: usize,
    },

    /// Summarize project dependencies
    Deps {
        /// Project directory (defaults to current)
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Show environment variables (filtered, sensitive masked)
    Env {
        /// Filter by name (case-insensitive)
        #[arg(short, long)]
        filter: Option<String>,

        /// Show all values (including sensitive)
        #[arg(long)]
        show_all: bool,
    },
}

#[derive(Subcommand)]
enum GitCommands {
    /// Compact diff output
    Diff {
        /// Additional git diff arguments
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,

        /// Maximum lines to show
        #[arg(short, long)]
        max_lines: Option<usize>,
    },

    /// Compact log output
    Log {
        /// Additional git log arguments
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,

        /// Number of commits to show
        #[arg(short = 'n', long, default_value = "10")]
        count: usize,
    },

    /// Compact status output
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Ls {
            path,
            depth,
            all,
            format,
        } => {
            ls::run(&path, depth, all, format, cli.verbose)?;
        }

        Commands::Read {
            file,
            level,
            max_lines,
            line_numbers,
        } => {
            read::run(&file, level, max_lines, line_numbers, cli.verbose)?;
        }

        Commands::Smart {
            file,
            model,
            force_download,
        } => {
            local_llm::run(&file, &model, force_download, cli.verbose)?;
        }

        Commands::Git { command } => match command {
            GitCommands::Diff { args, max_lines } => {
                git::run(git::GitCommand::Diff, &args, max_lines, cli.verbose)?;
            }
            GitCommands::Log { args, count } => {
                git::run(git::GitCommand::Log, &args, Some(count), cli.verbose)?;
            }
            GitCommands::Status => {
                git::run(git::GitCommand::Status, &[], None, cli.verbose)?;
            }
        },

        Commands::Err { command } => {
            let cmd = command.join(" ");
            runner::run_err(&cmd, cli.verbose)?;
        }

        Commands::Test { command } => {
            let cmd = command.join(" ");
            runner::run_test(&cmd, cli.verbose)?;
        }

        Commands::Json { file, depth } => {
            json_cmd::run(&file, depth, cli.verbose)?;
        }

        Commands::Deps { path } => {
            deps::run(&path, cli.verbose)?;
        }

        Commands::Env { filter, show_all } => {
            env_cmd::run(filter.as_deref(), show_all, cli.verbose)?;
        }
    }

    Ok(())
}
