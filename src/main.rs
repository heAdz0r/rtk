mod cargo_cmd;
mod cc_economics;
mod ccusage;
mod config;
mod container;
mod curl_cmd;
mod deps;
mod diff_cmd;
mod discover;
mod display_helpers;
mod env_cmd;
mod filter;
mod find_cmd;
mod format_cmd;
mod gain;
mod gh_cmd;
mod git;
mod go_cmd;
mod golangci_cmd;
mod grep_cmd;
mod grepai;
mod hook_audit_cmd; // upstream sync: hook rewrite audit metrics // grepai external semantic search integration
mod init;
mod json_cmd;
mod learn;
mod lint_cmd;
mod local_llm;
mod log_cmd;
mod ls;
mod next_cmd;
mod npm_cmd;
mod parser;
mod pip_cmd;
mod playwright_cmd;
mod pnpm_cmd;
mod prettier_cmd;
mod prisma_cmd;
mod pytest_cmd;
mod read;
mod read_cache; // PR-2: extracted read cache logic
mod read_changed; // PR-5: git-aware diff reading
mod read_digest; // PR-2: extracted tabular digest logic
mod read_render; // PR-2: extracted render helpers
mod read_source; // PR-2: extracted source I/O and line-range logic
mod read_symbols; // PR-3: symbol model and extraction traits
mod read_types; // PR-2: shared read types (ReadMode, ReadRequest)
mod rgai_cmd; // semantic search command (grepai-style intent matching)
mod ruff_cmd;
mod runner;
mod ssh_cmd;
mod summary;
mod symbols_regex; // PR-3: regex-based symbol extractor
mod tee; // upstream sync: tee raw output to file for LLM re-read
mod tracking;
mod tree;
mod tsc_cmd;
mod utils;
mod vitest_cmd;
mod wget_cmd;
mod write_cmd;
mod write_core;
mod write_semantics;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "rtk",
    version = "0.20.1-fork.4",
    about = "Rust Token Killer - Minimize LLM token consumption",
    long_about = "A high-performance CLI proxy designed to filter and summarize system outputs before they reach your LLM context."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Verbosity level (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Ultra-compact mode: ASCII icons, inline format (Level 2 optimizations)
    #[arg(short = 'u', long, global = true)]
    ultra_compact: bool,

    /// Set SKIP_ENV_VALIDATION=1 for child processes (Next.js, tsc, lint, prisma)
    #[arg(long = "skip-env", global = true)]
    skip_env: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// List directory contents with token-optimized output (proxy to native ls)
    Ls {
        /// Arguments passed to ls (supports all native ls flags like -l, -a, -h, -R)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Directory tree with token-optimized output (proxy to native tree)
    Tree {
        /// Arguments passed to tree (supports all native tree flags like -L, -d, -a)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Read file with intelligent filtering (CSV/TSV auto-digest in filtered modes)
    Read {
        /// File to read
        file: PathBuf,
        /// Filter: none (exact), minimal, aggressive
        #[arg(short, long, default_value = "minimal")]
        level: filter::FilterLevel,
        /// Start line (1-based, inclusive)
        #[arg(long)]
        from: Option<usize>,
        /// End line (1-based, inclusive)
        #[arg(long)]
        to: Option<usize>,
        /// Max lines
        #[arg(short, long)]
        max_lines: Option<usize>,
        /// Show line numbers
        #[arg(short = 'n', long)]
        line_numbers: bool,

        // ── Read mode flags (mutually exclusive) ──
        /// Show structural outline with line spans
        #[arg(long, group = "read_mode")]
        outline: bool,
        /// Show machine-readable JSON symbol index
        #[arg(long, group = "read_mode")]
        symbols: bool,
        /// Show only changed hunks from git working tree
        #[arg(long, group = "read_mode")]
        changed: bool,
        /// Show changed hunks relative to a revision (e.g., HEAD~3, main)
        #[arg(long, group = "read_mode")]
        since: Option<String>,
        /// Context lines for --changed/--since (default: 3)
        #[arg(long, default_value = "3", requires = "read_mode")]
        diff_context: usize,
        /// Deduplicate repetitive blocks (opt-in, disabled by default)
        #[arg(long)]
        dedup: bool,
    },

    /// Generate 2-line technical summary (heuristic-based)
    Smart {
        /// File to analyze
        file: PathBuf,
        /// Model: heuristic
        #[arg(short, long, default_value = "heuristic")]
        model: String,
        /// Force model download
        #[arg(long)]
        force_download: bool,
    },

    /// Git commands with compact output
    Git {
        #[command(subcommand)]
        command: GitCommands,
    },

    /// Safe file writes (atomic + idempotent) with dry-run support
    Write {
        /// Output mode: quiet (silent on success), concise (default), json (machine-readable)
        #[arg(long, value_enum, default_value = "concise")]
        output: write_cmd::OutputMode,
        #[command(subcommand)]
        command: WriteCommands,
    },

    /// GitHub CLI (gh) commands with token-optimized output
    Gh {
        /// Subcommand: pr, issue, run, repo
        subcommand: String,
        /// Additional arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// pnpm commands with ultra-compact output
    Pnpm {
        #[command(subcommand)]
        command: PnpmCommands,
    },

    /// Run command and show only errors/warnings
    Err {
        /// Command to run
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Run tests and show only failures
    Test {
        /// Test command (e.g. cargo test)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Show JSON structure without values
    Json {
        /// JSON file
        file: PathBuf,
        /// Max depth
        #[arg(short, long, default_value = "5")]
        depth: usize,
    },

    /// Summarize project dependencies
    Deps {
        /// Project path
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Show environment variables (filtered, sensitive masked)
    Env {
        /// Filter by name (e.g. PATH, AWS)
        #[arg(short, long)]
        filter: Option<String>,
        /// Show all (include sensitive)
        #[arg(long)]
        show_all: bool,
    },

    /// Find files with compact tree output
    Find {
        /// Pattern to search (glob)
        pattern: String,
        /// Path to search in
        #[arg(default_value = ".")]
        path: String,
        /// Maximum results to show
        #[arg(short, long, default_value = "50")]
        max: usize,
        /// Filter by type: f (file), d (directory)
        #[arg(short = 't', long, default_value = "f")]
        file_type: String,
    },

    /// Ultra-condensed diff (only changed lines)
    Diff {
        /// First file or - for stdin (unified diff)
        file1: PathBuf,
        /// Second file (optional if stdin)
        file2: Option<PathBuf>,
    },

    /// Filter and deduplicate log output
    Log {
        /// Log file (omit for stdin)
        file: Option<PathBuf>,
    },

    /// Docker commands with compact output
    Docker {
        #[command(subcommand)]
        command: DockerCommands,
    },

    /// Kubectl commands with compact output
    Kubectl {
        #[command(subcommand)]
        command: KubectlCommands,
    },

    /// Run command and show heuristic summary
    Summary {
        /// Command to run and summarize
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Compact grep - strips whitespace, truncates, groups by file
    Grep {
        /// Pattern to search
        pattern: String,
        /// Path to search in
        #[arg(default_value = ".")]
        path: String,
        /// Max line length
        #[arg(short = 'l', long, default_value = "80")]
        max_len: usize,
        /// Max results to show
        #[arg(short, long, default_value = "50")]
        max: usize,
        /// Show only match context (not full line)
        #[arg(short, long)]
        context_only: bool,
        /// Filter by file type (e.g., ts, py, rust)
        #[arg(short = 't', long)]
        file_type: Option<String>,
        /// Show line numbers (always on, accepted for grep/rg compatibility)
        #[arg(short = 'n', long)]
        line_numbers: bool,
        /// Extra ripgrep arguments (e.g., -i, -A 3, -w, --glob)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Rust-native semantic search (grepai-style intent matching)
    Rgai {
        /// Natural-language query
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Path to search in
        #[arg(short, long, default_value = ".")]
        path: String,
        /// Max files to show
        #[arg(short, long, default_value = "8")]
        max: usize,
        /// Context lines around each match
        #[arg(short = 'c', long, default_value = "1")]
        context: usize,
        /// Filter by file type (e.g., ts, py, rust)
        #[arg(short = 't', long)]
        file_type: Option<String>,
        /// Skip files larger than N KB
        #[arg(long, default_value = "512")]
        max_file_kb: usize,
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Compact output (fewer lines per hit)
        #[arg(long)]
        compact: bool,
        /// Force built-in keyword search (skip grepai delegation)
        #[arg(long)]
        builtin: bool,
    },

    /// Initialize rtk instructions in CLAUDE.md
    Init {
        /// Add to global ~/.claude/CLAUDE.md instead of local
        #[arg(short, long)]
        global: bool,

        /// Show current configuration
        #[arg(long)]
        show: bool,

        /// Inject full instructions into CLAUDE.md (legacy mode)
        #[arg(long = "claude-md", group = "mode")]
        claude_md: bool,

        /// Hook only, no RTK.md
        #[arg(long = "hook-only", group = "mode")]
        hook_only: bool,

        /// Auto-patch settings.json without prompting
        #[arg(long = "auto-patch", group = "patch")]
        auto_patch: bool,

        /// Skip settings.json patching (print manual instructions)
        #[arg(long = "no-patch", group = "patch")]
        no_patch: bool,

        /// Remove all RTK artifacts (hook, RTK.md, CLAUDE.md reference, settings.json entry)
        #[arg(long)]
        uninstall: bool,
    },

    /// Download with compact output (strips progress bars)
    Wget {
        /// URL to download
        url: String,
        /// Output to stdout instead of file
        #[arg(short = 'O', long)]
        stdout: bool,
        /// Additional wget arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Show token savings summary and history
    Gain {
        /// Filter statistics to current project (current working directory) // added
        #[arg(short, long)]
        project: bool,
        /// Show ASCII graph of daily savings
        #[arg(short, long)]
        graph: bool,
        /// Show recent command history
        #[arg(short = 'H', long)]
        history: bool,
        /// Show monthly quota savings estimate
        #[arg(short, long)]
        quota: bool,
        /// Subscription tier for quota calculation: pro, 5x, 20x
        #[arg(short, long, default_value = "20x", requires = "quota")]
        tier: String,
        /// Show detailed daily breakdown (all days)
        #[arg(short, long)]
        daily: bool,
        /// Show weekly breakdown
        #[arg(short, long)]
        weekly: bool,
        /// Show monthly breakdown
        #[arg(short, long)]
        monthly: bool,
        /// Show all time breakdowns (daily + weekly + monthly)
        #[arg(short, long)]
        all: bool,
        /// Output format: text, json, csv
        #[arg(short, long, default_value = "text")]
        format: String,
    },

    /// Claude Code economics: spending (ccusage) vs savings (rtk) analysis
    CcEconomics {
        /// Show detailed daily breakdown
        #[arg(short, long)]
        daily: bool,
        /// Show weekly breakdown
        #[arg(short, long)]
        weekly: bool,
        /// Show monthly breakdown
        #[arg(short, long)]
        monthly: bool,
        /// Show all time breakdowns (daily + weekly + monthly)
        #[arg(short, long)]
        all: bool,
        /// Output format: text, json, csv
        #[arg(short, long, default_value = "text")]
        format: String,
    },

    /// Show or create configuration file
    Config {
        /// Create default config file
        #[arg(long)]
        create: bool,
    },

    /// Vitest commands with compact output
    Vitest {
        #[command(subcommand)]
        command: VitestCommands,
    },

    /// Prisma commands with compact output (no ASCII art)
    Prisma {
        #[command(subcommand)]
        command: PrismaCommands,
    },

    /// TypeScript compiler with grouped error output
    Tsc {
        /// TypeScript compiler arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Next.js build with compact output
    Next {
        /// Next.js build arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// ESLint with grouped rule violations
    Lint {
        /// Linter arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Prettier format checker with compact output
    Prettier {
        /// Prettier arguments (e.g., --check, --write)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Universal format checker (prettier, black, ruff format)
    Format {
        /// Formatter arguments (auto-detects formatter from project files)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Playwright E2E tests with compact output
    Playwright {
        /// Playwright arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Cargo commands with compact output
    Cargo {
        #[command(subcommand)]
        command: CargoCommands,
    },

    /// npm run with filtered output (strip boilerplate)
    Npm {
        /// npm run arguments (script name + options)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// npx with intelligent routing (tsc, eslint, prisma -> specialized filters)
    Npx {
        /// npx arguments (command + options)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Curl with auto-JSON detection and schema output
    Curl {
        /// Curl arguments (URL + options)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// SSH with smart output filtering (psql/json/html/generic)
    Ssh {
        /// SSH arguments (host + remote command + flags)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Discover missed RTK savings from Claude Code history
    Discover {
        /// Filter by project path (substring match)
        #[arg(short, long)]
        project: Option<String>,
        /// Max commands per section
        #[arg(short, long, default_value = "15")]
        limit: usize,
        /// Scan all projects (default: current project only)
        #[arg(short, long)]
        all: bool,
        /// Limit to sessions from last N days
        #[arg(short, long, default_value = "30")]
        since: u64,
        /// Output format: text, json
        #[arg(short, long, default_value = "text")]
        format: String,
    },

    /// Learn CLI corrections from Claude Code error history
    Learn {
        /// Filter by project path (substring match)
        #[arg(short, long)]
        project: Option<String>,
        /// Scan all projects (default: current project only)
        #[arg(short, long)]
        all: bool,
        /// Limit to sessions from last N days
        #[arg(short, long, default_value = "30")]
        since: u64,
        /// Output format: text, json
        #[arg(short, long, default_value = "text")]
        format: String,
        /// Generate .claude/rules/cli-corrections.md file
        #[arg(short, long)]
        write_rules: bool,
        /// Minimum confidence threshold (0.0-1.0)
        #[arg(long, default_value = "0.6")]
        min_confidence: f64,
        /// Minimum occurrences to include in report
        #[arg(long, default_value = "1")]
        min_occurrences: usize,
    },

    /// Execute command without filtering but track usage
    Proxy {
        /// Command and arguments to execute
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },

    /// Ruff linter/formatter with compact output
    Ruff {
        /// Ruff arguments (e.g., check, format --check)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Pytest test runner with compact output
    Pytest {
        /// Pytest arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Pip package manager with compact output (auto-detects uv)
    Pip {
        /// Pip arguments (e.g., list, outdated, install)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Go commands with compact output
    Go {
        #[command(subcommand)]
        command: GoCommands,
    },

    /// golangci-lint with compact output
    #[command(name = "golangci-lint")]
    GolangciLint {
        /// golangci-lint arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Show hook rewrite audit metrics (requires RTK_HOOK_AUDIT=1) // upstream sync
    #[command(name = "hook-audit")]
    HookAudit {
        /// Show entries from last N days (0 = all time)
        #[arg(short, long, default_value = "7")]
        since: u64,
    },
}

#[derive(Subcommand)]
enum GitCommands {
    /// Condensed diff output
    Diff {
        /// Git arguments (supports all git diff flags like --stat, --cached, etc)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// One-line commit history
    Log {
        /// Git arguments (supports all git log flags like --oneline, --graph, --all)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compact status (supports all git status flags)
    Status {
        /// Git arguments (supports all git status flags like --porcelain, --short, -s)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compact show (commit summary + stat + compacted diff)
    Show {
        /// Git arguments (supports all git show flags)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Add files → "ok ✓"
    Add {
        /// Files and flags to add (supports all git add flags like -A, -p, --all, etc)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Commit → "ok ✓ \<hash\>"
    Commit {
        /// Commit message
        #[arg(short, long)]
        message: String,
    },
    /// Push → "ok ✓ \<branch\>"
    Push {
        /// Git push arguments (supports -u, remote, branch, etc.)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Pull → "ok ✓ \<stats\>"
    Pull {
        /// Git pull arguments (supports --rebase, remote, branch, etc.)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compact branch listing (current/local/remote)
    Branch {
        /// Git branch arguments (supports -d, -D, -m, etc.)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Fetch → "ok fetched (N new refs)"
    Fetch {
        /// Git fetch arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Stash management (list, show, pop, apply, drop)
    Stash {
        /// Subcommand: list, show, pop, apply, drop, push
        subcommand: Option<String>,
        /// Additional arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Compact worktree listing
    Worktree {
        /// Git worktree arguments (add, remove, prune, or empty for list)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Passthrough: runs any unsupported git subcommand directly
    #[command(external_subcommand)]
    Other(Vec<OsString>),
}

#[derive(Subcommand)]
enum WriteCommands {
    /// Replace exact text in a file (first match by default)
    Replace {
        /// Target file
        file: PathBuf,
        /// Text to find (exact match)
        #[arg(long)]
        from: String,
        /// Replacement text
        #[arg(long)]
        to: String,
        /// Replace all matches
        #[arg(long)]
        all: bool,
        /// Preview without writing
        #[arg(long)]
        dry_run: bool,
        /// Use fast durability mode (skip fsync)
        #[arg(long)]
        fast: bool,
    },
    /// Apply exact old->new hunk replacement
    Patch {
        /// Target file
        file: PathBuf,
        /// Old block to replace (exact match)
        #[arg(long)]
        old: String,
        /// New block
        #[arg(long = "new")]
        new_text: String,
        /// Replace all matching hunks
        #[arg(long)]
        all: bool,
        /// Preview without writing
        #[arg(long)]
        dry_run: bool,
        /// Use fast durability mode (skip fsync)
        #[arg(long)]
        fast: bool,
    },
    /// Set structured config key in JSON/TOML file
    Set {
        /// Target file
        file: PathBuf,
        /// Dotted key path (e.g. hooks.PreToolUse.0.matcher)
        #[arg(long)]
        key: String,
        /// Value payload
        #[arg(long)]
        value: String,
        /// Value parser
        #[arg(long, value_enum, default_value = "auto")]
        value_type: write_cmd::ConfigValueType,
        /// Config format
        #[arg(long, value_enum, default_value = "auto")]
        format: write_cmd::ConfigFormat,
        /// Preview without writing
        #[arg(long)]
        dry_run: bool,
        /// Use fast durability mode (skip fsync)
        #[arg(long)]
        fast: bool,
    },
    /// Execute a batch of write operations from a JSON plan (single process, grouped I/O)
    Batch {
        /// JSON plan: array of ops [{op:"replace",file:"...",from:"...",to:"..."}, ...]
        #[arg(long)]
        plan: String,
        /// Preview without writing
        #[arg(long)]
        dry_run: bool,
        /// Use fast durability mode (skip fsync)
        #[arg(long)]
        fast: bool,
    },
}

#[derive(Subcommand)]
enum PnpmCommands {
    /// List installed packages (ultra-dense)
    List {
        /// Depth level (default: 0)
        #[arg(short, long, default_value = "0")]
        depth: usize,
        /// Additional pnpm arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show outdated packages (condensed: "pkg: old → new")
    Outdated {
        /// Additional pnpm arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Install packages (filter progress bars)
    Install {
        /// Packages to install
        packages: Vec<String>,
        /// Additional pnpm arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build (delegates to next build filter)
    Build {
        /// Additional build arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Typecheck (delegates to tsc filter)
    Typecheck {
        /// Additional typecheck arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Passthrough: runs any unsupported pnpm subcommand directly
    #[command(external_subcommand)]
    Other(Vec<OsString>),
}

#[derive(Subcommand)]
enum DockerCommands {
    /// List running containers
    Ps,
    /// List images
    Images,
    /// Show container logs (deduplicated)
    Logs { container: String },
    /// Passthrough: runs any unsupported docker subcommand directly
    #[command(external_subcommand)]
    Other(Vec<OsString>),
}

#[derive(Subcommand)]
enum KubectlCommands {
    /// List pods
    Pods {
        #[arg(short, long)]
        namespace: Option<String>,
        /// All namespaces
        #[arg(short = 'A', long)]
        all: bool,
    },
    /// List services
    Services {
        #[arg(short, long)]
        namespace: Option<String>,
        /// All namespaces
        #[arg(short = 'A', long)]
        all: bool,
    },
    /// Show pod logs (deduplicated)
    Logs {
        pod: String,
        #[arg(short, long)]
        container: Option<String>,
    },
    /// Passthrough: runs any unsupported kubectl subcommand directly
    #[command(external_subcommand)]
    Other(Vec<OsString>),
}

#[derive(Subcommand)]
enum VitestCommands {
    /// Run tests with filtered output (90% token reduction)
    Run {
        /// Additional vitest arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PrismaCommands {
    /// Generate Prisma Client (strip ASCII art)
    Generate {
        /// Additional prisma arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage migrations
    Migrate {
        #[command(subcommand)]
        command: PrismaMigrateCommands,
    },
    /// Push schema to database
    DbPush {
        /// Additional prisma arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PrismaMigrateCommands {
    /// Create and apply migration
    Dev {
        /// Migration name
        #[arg(short, long)]
        name: Option<String>,
        /// Additional arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Check migration status
    Status {
        /// Additional arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Deploy migrations to production
    Deploy {
        /// Additional arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum CargoCommands {
    /// Build with compact output (strip Compiling lines, keep errors)
    Build {
        /// Additional cargo build arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Test with failures-only output
    Test {
        /// Additional cargo test arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Clippy with warnings grouped by lint rule
    Clippy {
        /// Additional cargo clippy arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Check with compact output (strip Checking lines, keep errors)
    Check {
        /// Additional cargo check arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Install with compact output (strip dep compilation, keep installed/errors)
    Install {
        /// Additional cargo install arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Nextest with failures-only output
    Nextest {
        /// Additional cargo nextest arguments (e.g., run, list, --lib)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Passthrough: runs any unsupported cargo subcommand directly
    #[command(external_subcommand)]
    Other(Vec<OsString>),
}

#[derive(Subcommand)]
enum GoCommands {
    /// Run tests with compact output (90% token reduction via JSON streaming)
    Test {
        /// Additional go test arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build with compact output (errors only)
    Build {
        /// Additional go build arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Vet with compact output
    Vet {
        /// Additional go vet arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Passthrough: runs any unsupported go subcommand directly
    #[command(external_subcommand)]
    Other(Vec<OsString>),
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Ls { args } => {
            ls::run(&args, cli.verbose)?;
        }

        Commands::Tree { args } => {
            tree::run(&args, cli.verbose)?;
        }

        Commands::Read {
            file,
            level,
            from,
            to,
            max_lines,
            line_numbers,
            outline,
            symbols,
            changed,
            since,
            diff_context,
            dedup,
        } => {
            // Determine ReadMode from flags
            let mode = if outline {
                read::ReadMode::Outline
            } else if symbols {
                read::ReadMode::Symbols
            } else if changed {
                read::ReadMode::Changed
            } else if let Some(rev) = since {
                read::ReadMode::Since(rev)
            } else {
                read::ReadMode::Full
            };

            // Reject incompatible flags in non-full modes.
            if !matches!(mode, read::ReadMode::Full) {
                let mut incompatible = Vec::new();
                if from.is_some() {
                    incompatible.push("--from");
                }
                if to.is_some() {
                    incompatible.push("--to");
                }
                if max_lines.is_some() {
                    incompatible.push("--max-lines");
                }
                if line_numbers {
                    incompatible.push("-n/--line-numbers");
                }
                if dedup {
                    incompatible.push("--dedup");
                }
                if level != filter::FilterLevel::Minimal {
                    incompatible.push("--level");
                }
                if matches!(mode, read::ReadMode::Outline | read::ReadMode::Symbols)
                    && diff_context != 3
                {
                    incompatible.push("--diff-context");
                }
                if !incompatible.is_empty() {
                    let mode_flag = match &mode {
                        read::ReadMode::Outline => "--outline",
                        read::ReadMode::Symbols => "--symbols",
                        read::ReadMode::Changed => "--changed",
                        read::ReadMode::Since(_) => "--since",
                        read::ReadMode::Full => unreachable!(),
                    };
                    anyhow::bail!(
                        "{} incompatible with {}",
                        incompatible.join(", "),
                        mode_flag
                    );
                }
            }

            // Dispatch by mode
            match mode {
                read::ReadMode::Outline | read::ReadMode::Symbols => {
                    if file == Path::new("-") {
                        anyhow::bail!("--outline and --symbols require a file path, not stdin");
                    }
                    read::run_symbols(&file, &mode, cli.verbose)?;
                }
                read::ReadMode::Changed => {
                    if file == Path::new("-") {
                        anyhow::bail!("--changed requires a file path, not stdin");
                    }
                    read::run_changed(&file, None, diff_context, cli.verbose)?;
                }
                read::ReadMode::Since(ref rev) => {
                    if file == Path::new("-") {
                        anyhow::bail!("--since requires a file path, not stdin");
                    }
                    read::run_changed(&file, Some(rev), diff_context, cli.verbose)?;
                }
                read::ReadMode::Full => {
                    if file == Path::new("-") {
                        read::run_stdin(level, from, to, max_lines, line_numbers, cli.verbose)?;
                    } else {
                        read::run(
                            &file,
                            level,
                            from,
                            to,
                            max_lines,
                            line_numbers,
                            dedup,
                            cli.verbose,
                        )?;
                    }
                }
            }
        }

        Commands::Smart {
            file,
            model,
            force_download,
        } => {
            local_llm::run(&file, &model, force_download, cli.verbose)?;
        }

        Commands::Git { command } => match command {
            GitCommands::Diff { args } => {
                git::run(git::GitCommand::Diff, &args, None, cli.verbose)?;
            }
            GitCommands::Log { args } => {
                git::run(git::GitCommand::Log, &args, None, cli.verbose)?;
            }
            GitCommands::Status { args } => {
                git::run(git::GitCommand::Status, &args, None, cli.verbose)?;
            }
            GitCommands::Show { args } => {
                git::run(git::GitCommand::Show, &args, None, cli.verbose)?;
            }
            GitCommands::Add { args } => {
                git::run(git::GitCommand::Add, &args, None, cli.verbose)?;
            }
            GitCommands::Commit { message } => {
                git::run(git::GitCommand::Commit { message }, &[], None, cli.verbose)?;
            }
            GitCommands::Push { args } => {
                git::run(git::GitCommand::Push, &args, None, cli.verbose)?;
            }
            GitCommands::Pull { args } => {
                git::run(git::GitCommand::Pull, &args, None, cli.verbose)?;
            }
            GitCommands::Branch { args } => {
                git::run(git::GitCommand::Branch, &args, None, cli.verbose)?;
            }
            GitCommands::Fetch { args } => {
                git::run(git::GitCommand::Fetch, &args, None, cli.verbose)?;
            }
            GitCommands::Stash { subcommand, args } => {
                git::run(
                    git::GitCommand::Stash { subcommand },
                    &args,
                    None,
                    cli.verbose,
                )?;
            }
            GitCommands::Worktree { args } => {
                git::run(git::GitCommand::Worktree, &args, None, cli.verbose)?;
            }
            GitCommands::Other(args) => {
                git::run_passthrough(&args, cli.verbose)?;
            }
        },

        Commands::Write { output, command } => match command {
            WriteCommands::Replace {
                file,
                from,
                to,
                all,
                dry_run,
                fast,
            } => {
                write_cmd::run_replace(&file, &from, &to, all, dry_run, fast, cli.verbose, output)?;
            }
            WriteCommands::Patch {
                file,
                old,
                new_text,
                all,
                dry_run,
                fast,
            } => {
                write_cmd::run_patch(&file, &old, &new_text, all, dry_run, fast, cli.verbose, output)?;
            }
            WriteCommands::Set {
                file,
                key,
                value,
                value_type,
                format,
                dry_run,
                fast,
            } => {
                write_cmd::run_set(
                    &file,
                    &key,
                    &value,
                    value_type,
                    format,
                    dry_run,
                    fast,
                    cli.verbose,
                    output,
                )?;
            }
            WriteCommands::Batch {
                plan,
                dry_run,
                fast,
            } => {
                write_cmd::run_batch(&plan, dry_run, fast, cli.verbose, output)?;
            }
        },

        Commands::Gh { subcommand, args } => {
            gh_cmd::run(&subcommand, &args, cli.verbose, cli.ultra_compact)?;
        }

        Commands::Pnpm { command } => match command {
            PnpmCommands::List { depth, args } => {
                pnpm_cmd::run(pnpm_cmd::PnpmCommand::List { depth }, &args, cli.verbose)?;
            }
            PnpmCommands::Outdated { args } => {
                pnpm_cmd::run(pnpm_cmd::PnpmCommand::Outdated, &args, cli.verbose)?;
            }
            PnpmCommands::Install { packages, args } => {
                pnpm_cmd::run(
                    pnpm_cmd::PnpmCommand::Install { packages },
                    &args,
                    cli.verbose,
                )?;
            }
            PnpmCommands::Build { args } => {
                next_cmd::run(&args, cli.verbose)?;
            }
            PnpmCommands::Typecheck { args } => {
                tsc_cmd::run(&args, cli.verbose)?;
            }
            PnpmCommands::Other(args) => {
                pnpm_cmd::run_passthrough(&args, cli.verbose)?;
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
            if file == Path::new("-") {
                json_cmd::run_stdin(depth, cli.verbose)?;
            } else {
                json_cmd::run(&file, depth, cli.verbose)?;
            }
        }

        Commands::Deps { path } => {
            deps::run(&path, cli.verbose)?;
        }

        Commands::Env { filter, show_all } => {
            env_cmd::run(filter.as_deref(), show_all, cli.verbose)?;
        }

        Commands::Find {
            pattern,
            path,
            max,
            file_type,
        } => {
            find_cmd::run(&pattern, &path, max, &file_type, cli.verbose)?;
        }

        Commands::Diff { file1, file2 } => {
            if let Some(f2) = file2 {
                diff_cmd::run(&file1, &f2, cli.verbose)?;
            } else {
                diff_cmd::run_stdin(cli.verbose)?;
            }
        }

        Commands::Log { file } => {
            if let Some(f) = file {
                log_cmd::run_file(&f, cli.verbose)?;
            } else {
                log_cmd::run_stdin(cli.verbose)?;
            }
        }

        Commands::Docker { command } => match command {
            DockerCommands::Ps => {
                container::run(container::ContainerCmd::DockerPs, &[], cli.verbose)?;
            }
            DockerCommands::Images => {
                container::run(container::ContainerCmd::DockerImages, &[], cli.verbose)?;
            }
            DockerCommands::Logs { container: c } => {
                container::run(container::ContainerCmd::DockerLogs, &[c], cli.verbose)?;
            }
            DockerCommands::Other(args) => {
                container::run_docker_passthrough(&args, cli.verbose)?;
            }
        },

        Commands::Kubectl { command } => match command {
            KubectlCommands::Pods { namespace, all } => {
                let mut args: Vec<String> = Vec::new();
                if all {
                    args.push("-A".to_string());
                } else if let Some(n) = namespace {
                    args.push("-n".to_string());
                    args.push(n);
                }
                container::run(container::ContainerCmd::KubectlPods, &args, cli.verbose)?;
            }
            KubectlCommands::Services { namespace, all } => {
                let mut args: Vec<String> = Vec::new();
                if all {
                    args.push("-A".to_string());
                } else if let Some(n) = namespace {
                    args.push("-n".to_string());
                    args.push(n);
                }
                container::run(container::ContainerCmd::KubectlServices, &args, cli.verbose)?;
            }
            KubectlCommands::Logs { pod, container: c } => {
                let mut args = vec![pod];
                if let Some(cont) = c {
                    args.push("-c".to_string());
                    args.push(cont);
                }
                container::run(container::ContainerCmd::KubectlLogs, &args, cli.verbose)?;
            }
            KubectlCommands::Other(args) => {
                container::run_kubectl_passthrough(&args, cli.verbose)?;
            }
        },

        Commands::Summary { command } => {
            let cmd = command.join(" ");
            summary::run(&cmd, cli.verbose)?;
        }

        Commands::Grep {
            pattern,
            path,
            max_len,
            max,
            context_only,
            file_type,
            line_numbers: _, // no-op: line numbers always enabled in grep_cmd::run
            extra_args,
        } => {
            grep_cmd::run(
                &pattern,
                &path,
                max_len,
                max,
                context_only,
                file_type.as_deref(),
                &extra_args,
                cli.verbose,
            )?;
        }

        Commands::Rgai {
            query,
            path,
            max,
            context,
            file_type,
            max_file_kb,
            json,
            compact,
            builtin, // --builtin flag: skip grepai delegation
        } => {
            // Backward-compat: rtk rgai "query words" ./src -> path="./src"
            let (query, path) = normalize_rgai_args(query, path);
            rgai_cmd::run(
                &query,
                &path,
                max,
                context,
                file_type.as_deref(),
                max_file_kb,
                json,
                compact,
                builtin, // pass --builtin flag
                cli.verbose,
            )?;
        }

        Commands::Init {
            global,
            show,
            claude_md,
            hook_only,
            auto_patch,
            no_patch,
            uninstall,
        } => {
            if show {
                init::show_config()?;
            } else if uninstall {
                init::uninstall(global, cli.verbose)?;
            } else {
                let patch_mode = if auto_patch {
                    init::PatchMode::Auto
                } else if no_patch {
                    init::PatchMode::Skip
                } else {
                    init::PatchMode::Ask
                };
                init::run(global, claude_md, hook_only, patch_mode, cli.verbose)?;
            }
        }

        Commands::Wget { url, stdout, args } => {
            if stdout {
                wget_cmd::run_stdout(&url, &args, cli.verbose)?;
            } else {
                wget_cmd::run(&url, &args, cli.verbose)?;
            }
        }

        Commands::Gain {
            project, // added
            graph,
            history,
            quota,
            tier,
            daily,
            weekly,
            monthly,
            all,
            format,
        } => {
            gain::run(
                project, // added: pass project flag
                graph,
                history,
                quota,
                &tier,
                daily,
                weekly,
                monthly,
                all,
                &format,
                cli.verbose,
            )?;
        }

        Commands::CcEconomics {
            daily,
            weekly,
            monthly,
            all,
            format,
        } => {
            cc_economics::run(daily, weekly, monthly, all, &format, cli.verbose)?;
        }

        Commands::Config { create } => {
            if create {
                let path = config::Config::create_default()?;
                println!("Created: {}", path.display());
            } else {
                config::show_config()?;
            }
        }

        Commands::Vitest { command } => match command {
            VitestCommands::Run { args } => {
                vitest_cmd::run(vitest_cmd::VitestCommand::Run, &args, cli.verbose)?;
            }
        },

        Commands::Prisma { command } => match command {
            PrismaCommands::Generate { args } => {
                prisma_cmd::run(prisma_cmd::PrismaCommand::Generate, &args, cli.verbose)?;
            }
            PrismaCommands::Migrate { command } => match command {
                PrismaMigrateCommands::Dev { name, args } => {
                    prisma_cmd::run(
                        prisma_cmd::PrismaCommand::Migrate {
                            subcommand: prisma_cmd::MigrateSubcommand::Dev { name },
                        },
                        &args,
                        cli.verbose,
                    )?;
                }
                PrismaMigrateCommands::Status { args } => {
                    prisma_cmd::run(
                        prisma_cmd::PrismaCommand::Migrate {
                            subcommand: prisma_cmd::MigrateSubcommand::Status,
                        },
                        &args,
                        cli.verbose,
                    )?;
                }
                PrismaMigrateCommands::Deploy { args } => {
                    prisma_cmd::run(
                        prisma_cmd::PrismaCommand::Migrate {
                            subcommand: prisma_cmd::MigrateSubcommand::Deploy,
                        },
                        &args,
                        cli.verbose,
                    )?;
                }
            },
            PrismaCommands::DbPush { args } => {
                prisma_cmd::run(prisma_cmd::PrismaCommand::DbPush, &args, cli.verbose)?;
            }
        },

        Commands::Tsc { args } => {
            tsc_cmd::run(&args, cli.verbose)?;
        }

        Commands::Next { args } => {
            next_cmd::run(&args, cli.verbose)?;
        }

        Commands::Lint { args } => {
            lint_cmd::run(&args, cli.verbose)?;
        }

        Commands::Prettier { args } => {
            prettier_cmd::run(&args, cli.verbose)?;
        }

        Commands::Format { args } => {
            format_cmd::run(&args, cli.verbose)?;
        }

        Commands::Playwright { args } => {
            playwright_cmd::run(&args, cli.verbose)?;
        }

        Commands::Cargo { command } => match command {
            CargoCommands::Build { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Build, &args, cli.verbose)?;
            }
            CargoCommands::Test { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Test, &args, cli.verbose)?;
            }
            CargoCommands::Clippy { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Clippy, &args, cli.verbose)?;
            }
            CargoCommands::Check { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Check, &args, cli.verbose)?;
            }
            CargoCommands::Install { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Install, &args, cli.verbose)?;
            }
            CargoCommands::Nextest { args } => {
                cargo_cmd::run(cargo_cmd::CargoCommand::Nextest, &args, cli.verbose)?;
            }
            CargoCommands::Other(args) => {
                cargo_cmd::run_passthrough(&args, cli.verbose)?;
            }
        },

        Commands::Npm { args } => {
            npm_cmd::run(&args, cli.verbose, cli.skip_env)?;
        }

        Commands::Curl { args } => {
            curl_cmd::run(&args, cli.verbose)?;
        }

        Commands::Ssh { args } => {
            ssh_cmd::run(&args, cli.verbose)?;
        }

        Commands::Discover {
            project,
            limit,
            all,
            since,
            format,
        } => {
            discover::run(project.as_deref(), all, since, limit, &format, cli.verbose)?;
        }

        Commands::Learn {
            project,
            all,
            since,
            format,
            write_rules,
            min_confidence,
            min_occurrences,
        } => {
            learn::run(
                project,
                all,
                since,
                format,
                write_rules,
                min_confidence,
                min_occurrences,
            )?;
        }

        Commands::Npx { args } => {
            if args.is_empty() {
                anyhow::bail!("npx requires a command argument");
            }

            // Intelligent routing: delegate to specialized filters
            match args[0].as_str() {
                "tsc" | "typescript" => {
                    tsc_cmd::run(&args[1..], cli.verbose)?;
                }
                "eslint" => {
                    lint_cmd::run(&args[1..], cli.verbose)?;
                }
                "prisma" => {
                    // Route to prisma_cmd based on subcommand
                    if args.len() > 1 {
                        let prisma_args: Vec<String> = args[2..].to_vec();
                        match args[1].as_str() {
                            "generate" => {
                                prisma_cmd::run(
                                    prisma_cmd::PrismaCommand::Generate,
                                    &prisma_args,
                                    cli.verbose,
                                )?;
                            }
                            "db" if args.len() > 2 && args[2] == "push" => {
                                prisma_cmd::run(
                                    prisma_cmd::PrismaCommand::DbPush,
                                    &args[3..],
                                    cli.verbose,
                                )?;
                            }
                            _ => {
                                // Passthrough other prisma subcommands
                                let timer = tracking::TimedExecution::start();
                                let mut cmd = std::process::Command::new("npx");
                                for arg in &args {
                                    cmd.arg(arg);
                                }
                                let status = cmd.status().context("Failed to run npx prisma")?;
                                let args_str = args.join(" ");
                                timer.track_passthrough(
                                    &format!("npx {}", args_str),
                                    &format!("rtk npx {} (passthrough)", args_str),
                                );
                                if !status.success() {
                                    std::process::exit(status.code().unwrap_or(1));
                                }
                            }
                        }
                    } else {
                        let timer = tracking::TimedExecution::start();
                        let status = std::process::Command::new("npx")
                            .arg("prisma")
                            .status()
                            .context("Failed to run npx prisma")?;
                        timer.track_passthrough("npx prisma", "rtk npx prisma (passthrough)");
                        if !status.success() {
                            std::process::exit(status.code().unwrap_or(1));
                        }
                    }
                }
                "next" => {
                    next_cmd::run(&args[1..], cli.verbose)?;
                }
                "prettier" => {
                    prettier_cmd::run(&args[1..], cli.verbose)?;
                }
                "playwright" => {
                    playwright_cmd::run(&args[1..], cli.verbose)?;
                }
                _ => {
                    // Generic passthrough with npm boilerplate filter
                    npm_cmd::run(&args, cli.verbose, cli.skip_env)?;
                }
            }
        }

        Commands::Ruff { args } => {
            ruff_cmd::run(&args, cli.verbose)?;
        }

        Commands::Pytest { args } => {
            pytest_cmd::run(&args, cli.verbose)?;
        }

        Commands::Pip { args } => {
            pip_cmd::run(&args, cli.verbose)?;
        }

        Commands::Go { command } => match command {
            GoCommands::Test { args } => {
                go_cmd::run_test(&args, cli.verbose)?;
            }
            GoCommands::Build { args } => {
                go_cmd::run_build(&args, cli.verbose)?;
            }
            GoCommands::Vet { args } => {
                go_cmd::run_vet(&args, cli.verbose)?;
            }
            GoCommands::Other(args) => {
                go_cmd::run_other(&args, cli.verbose)?;
            }
        },

        Commands::GolangciLint { args } => {
            golangci_cmd::run(&args, cli.verbose)?;
        }

        Commands::HookAudit { since } => {
            // upstream sync: hook audit command
            hook_audit_cmd::run(since, cli.verbose)?;
        }

        Commands::Proxy { args } => {
            use std::process::Command;

            if args.is_empty() {
                anyhow::bail!(
                    "proxy requires a command to execute\nUsage: rtk proxy <command> [args...]"
                );
            }

            let timer = tracking::TimedExecution::start();

            let cmd_name = args[0].to_string_lossy();
            let cmd_args: Vec<String> = args[1..]
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();

            if cli.verbose > 0 {
                eprintln!("Proxy mode: {} {}", cmd_name, cmd_args.join(" "));
            }

            let output = Command::new(cmd_name.as_ref())
                .args(&cmd_args)
                .output()
                .context(format!("Failed to execute command: {}", cmd_name))?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let full_output = format!("{}{}", stdout, stderr);

            // Print output
            print!("{}", stdout);
            eprint!("{}", stderr);

            // Track usage (input = output since no filtering)
            timer.track(
                &format!("{} {}", cmd_name, cmd_args.join(" ")),
                &format!("rtk proxy {} {}", cmd_name, cmd_args.join(" ")),
                &full_output,
                &full_output,
            );

            // Exit with same code as child process
            if !output.status.success() {
                std::process::exit(output.status.code().unwrap_or(1));
            }
        }
    }

    Ok(())
}

/// Normalize rgai positional args: detect trailing path token in query words.
fn normalize_rgai_args(mut query_parts: Vec<String>, mut path: String) -> (String, String) {
    if path == "." && query_parts.len() > 1 {
        if let Some(last) = query_parts.last().cloned() {
            if looks_like_path_token(&last) {
                path = last;
                query_parts.pop();
            }
        }
    }
    let query = query_parts.join(" ");
    (query, path)
}

fn looks_like_path_token(token: &str) -> bool {
    // FIX: removed bare contains('/') — too greedy, treats "client/server" as a path.
    // Now only matches tokens that look like actual filesystem paths.
    token == "."
        || token == ".."
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with('/')
        || token.starts_with("~/")
}

#[cfg(test)]
mod rgai_arg_tests {
    use super::*;

    #[test]
    fn normalize_rgai_keeps_multiword_query() {
        let (query, path) = normalize_rgai_args(
            vec!["token".to_string(), "refresh".to_string()],
            ".".to_string(),
        );
        assert_eq!(query, "token refresh");
        assert_eq!(path, ".");
    }

    #[test]
    fn normalize_rgai_supports_old_positional_path() {
        let (query, path) = normalize_rgai_args(
            vec!["auth".to_string(), "flow".to_string(), "./src".to_string()],
            ".".to_string(),
        );
        assert_eq!(query, "auth flow");
        assert_eq!(path, "./src");
    }

    #[test]
    fn normalize_rgai_does_not_treat_plain_word_as_path() {
        let (query, path) = normalize_rgai_args(
            vec!["domain".to_string(), "model".to_string()],
            ".".to_string(),
        );
        assert_eq!(query, "domain model");
        assert_eq!(path, ".");
    }

    // FIX: slash-containing words like "client/server" must NOT be treated as paths
    #[test]
    fn normalize_rgai_does_not_treat_slash_word_as_path() {
        let (query, path) = normalize_rgai_args(
            vec!["client/server".to_string(), "architecture".to_string()],
            ".".to_string(),
        );
        assert_eq!(query, "client/server architecture");
        assert_eq!(path, ".");
    }

    #[test]
    fn looks_like_path_recognizes_real_paths() {
        assert!(looks_like_path_token("./src"));
        assert!(looks_like_path_token("../lib"));
        assert!(looks_like_path_token("/usr/local"));
        assert!(looks_like_path_token("~/projects"));
        assert!(looks_like_path_token("."));
        assert!(looks_like_path_token(".."));
    }

    #[test]
    fn looks_like_path_rejects_non_paths() {
        assert!(!looks_like_path_token("client/server"));
        assert!(!looks_like_path_token("input/output"));
        assert!(!looks_like_path_token("read/write"));
    }
}
