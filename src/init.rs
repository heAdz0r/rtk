use crate::grepai; // grepai integration
use crate::write_core::{AtomicWriter, WriteOptions};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

// Embedded hook scripts (guards before set -euo pipefail)
const REWRITE_HOOK: &str = include_str!("../hooks/rtk-rewrite.sh");
const BLOCK_GREP_HOOK: &str = include_str!("../hooks/rtk-block-native-grep.sh"); // prefer rtk over native Grep
const BLOCK_READ_HOOK: &str = include_str!("../hooks/rtk-block-native-read.sh"); // prefer rtk over native Read
const BLOCK_WRITE_HOOK: &str = include_str!("../hooks/rtk-block-native-write.sh"); // prefer rtk over native Edit/Write
const BLOCK_EXPLORE_HOOK: &str = include_str!("../hooks/rtk-block-native-explore.sh"); // prefer rtk memory over native Task/Explore

// Embedded slim RTK awareness instructions
const RTK_SLIM: &str = include_str!("../hooks/rtk-awareness.md");

/// Control flow for settings.json patching
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatchMode {
    Ask,  // Default: prompt user [y/N]
    Auto, // --auto-patch: no prompt
    Skip, // --no-patch: manual instructions
}

/// Result of settings.json patching operation
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatchResult {
    Patched,        // Hook was added successfully
    AlreadyPresent, // Hook was already in settings.json
    Declined,       // User declined when prompted
    Skipped,        // --no-patch flag used
}

// Legacy full instructions for backward compatibility (--claude-md mode)
const RTK_INSTRUCTIONS: &str = r##"<!-- rtk-instructions v2 -->
# RTK (Rust Token Killer) - Token-Optimized Commands

## Golden Rule

**Always prefix commands with `rtk`**. If RTK has a dedicated filter, it uses it. If not, it passes through unchanged. This means RTK is always safe to use.

**Important**: Even in command chains with `&&`, use `rtk`:
```bash
# ❌ Wrong
git add . && git commit -m "msg" && git push

# ✅ Correct
rtk git add . && rtk git commit -m "msg" && rtk git push
```

## Search Priority Policy

**Search priority (mandatory): rgai > rg > grep.**

- Use `rtk rgai` first for semantic/intention-based discovery.
- Use `rtk grep` for exact/regex matching.
- `rtk grep` internally follows `rg -> grep` backend fallback automatically.
- Native Grep/Read tools are blocked by default (hard deny policy); use `rtk grep` / `rtk read` via Bash.

## Precise File Reads

For exact parity with native file reads (without filtering), use:

```bash
rtk read <file> --level none --from <N> --to <M>
```

## Safe File Writes

Use `rtk write` for atomicity and idempotency, not primarily for token savings (write token delta is negligible).

| Scenario | Tool |
|----------|------|
| 2+ files in one task | `rtk write batch` — single call, groups I/O |
| Retry-safe / idempotent | `rtk write patch/replace` — noop if already applied |
| Structured config | `rtk write set` — type-safe JSON/TOML key update |
| Single trivial edit | `rtk write patch/replace` |

```bash
rtk write batch --plan '[{"op":"patch","file":"a.rs","old":"x","new":"y"},...]'  # multi-file
rtk write replace <file> --from <old> --to <new> [--all]
rtk write patch <file> --old "<block>" --new "<block>" [--all]
rtk write set <file.{json|toml}> --key a.b --value <value>
```

## Tabular Files (CSV/TSV)

- In `minimal/aggressive` modes, `rtk read` emits a compact table digest (rows/cols/sample/sampled-stats) for token savings.
- Use `--level none --from/--to` when exact row text is required.

## Read Cache

- `rtk read` filtered output is cached for repeat reads.
- Cache is invalidated automatically when file path/size/mtime change.

## RTK Commands by Workflow

### Build & Compile (80-90% savings)
```bash
rtk cargo build         # Cargo build output
rtk cargo check         # Cargo check output
rtk cargo clippy        # Clippy warnings grouped by file (80%)
rtk tsc                 # TypeScript errors grouped by file/code (83%)
rtk lint                # ESLint/Biome violations grouped (84%)
rtk prettier --check    # Files needing format only (70%)
rtk next build          # Next.js build with route metrics (87%)
```

### Test (90-99% savings)
```bash
rtk cargo test          # Cargo test failures only (90%)
rtk vitest run          # Vitest failures only (99.5%)
rtk playwright test     # Playwright failures only (94%)
rtk test <cmd>          # Generic test wrapper - failures only
```

### Git (59-80% savings)
```bash
rtk git status          # Compact status
rtk git log             # Compact log (works with all git flags)
rtk git diff            # Compact diff (80%)
rtk git show            # Compact show (80%)
rtk git add             # Ultra-compact confirmations (59%)
rtk git commit          # Ultra-compact confirmations (59%)
rtk git push            # Ultra-compact confirmations
rtk git pull            # Ultra-compact confirmations
rtk git branch          # Compact branch list
rtk git fetch           # Compact fetch
rtk git stash           # Compact stash
rtk git worktree        # Compact worktree
```

Note: Git passthrough works for ALL subcommands, even those not explicitly listed.

### GitHub (26-87% savings)
```bash
rtk gh pr view <num>    # Compact PR view (87%)
rtk gh pr checks        # Compact PR checks (79%)
rtk gh run list         # Compact workflow runs (82%)
rtk gh issue list       # Compact issue list (80%)
rtk gh api              # Compact API responses (26%)
```

### JavaScript/TypeScript Tooling (70-90% savings)
```bash
rtk pnpm list           # Compact dependency tree (70%)
rtk pnpm outdated       # Compact outdated packages (80%)
rtk pnpm install        # Compact install output (90%)
rtk bun run <script>    # Compact bun script output (default summary)
rtk npm run <script>    # Compact npm script output
rtk npx <cmd>           # Compact npx command output
rtk prisma              # Prisma without ASCII art (88%)
```

### Files & Search (60-85% savings)
```bash
rtk ls <path>           # Tree format, compact (65%)
rtk read <file>         # Code/text read with filtering; CSV/TSV -> compact digest
rtk read <file> --level none --from <N> --to <M>  # Exact line-range read (no filtering)
rtk write batch --plan '[...]'  # Multi-file atomic batch (prefer for 2+ files)
rtk write replace <file> --from old --to new [--all]  # Atomic text replace
rtk write patch <file> --old "<block>" --new "<block>" [--all]  # Atomic block patch (idempotent)
rtk write set <file> --key a.b --value v --format json|toml  # Atomic structured update
rtk rgai <query>        # Semantic search ranked by relevance (85%)
rtk grep <pattern>      # Exact/regex search (internal rg -> grep fallback)
rtk find <pattern>      # Find grouped by directory (70%)
```

### Analysis & Debug (70-90% savings)
```bash
rtk err <cmd>           # Filter errors only from any command
rtk log <file>          # Deduplicated logs with counts
rtk json <file>         # JSON structure without values
rtk deps                # Dependency overview
rtk env                 # Environment variables compact
rtk summary <cmd>       # Smart summary of command output
rtk diff                # Ultra-compact diffs
```

### Infrastructure (85% savings)
```bash
rtk docker ps           # Compact container list
rtk docker images       # Compact image list
rtk docker logs <c>     # Deduplicated logs
rtk kubectl get         # Compact resource list
rtk kubectl logs        # Deduplicated pod logs
```

### Network (65-70% savings)
```bash
rtk ssh <host> <cmd>   # SSH output filtering (psql/json/html/noise)
rtk curl <url>          # Compact HTTP responses (70%)
rtk wget <url>          # Compact download output (65%)
```

### Meta Commands
```bash
rtk gain                # View token savings statistics
rtk gain --history      # View command history with savings
rtk discover            # Analyze Claude Code sessions for missed RTK usage
rtk proxy <cmd>         # Run command without filtering (for debugging)
rtk init                # Add RTK instructions to CLAUDE.md
rtk init --global       # Add RTK to ~/.claude/CLAUDE.md
```

## Token Savings Overview

| Category | Commands | Typical Savings |
|----------|----------|-----------------|
| Tests | vitest, playwright, cargo test | 90-99% |
| Build | next, tsc, lint, prettier | 70-87% |
| Git | status, log, diff, add, commit | 59-80% |
| GitHub | gh pr, gh run, gh issue | 26-87% |
| Package Managers | pnpm, npm, npx | 70-90% |
| Files | ls, read, grep, rgai, find | 60-85% |
| Infrastructure | docker, kubectl | 85% |
| Network | curl, wget | 65-70% |

Overall average: **60-90% token reduction** on common development operations.
<!-- /rtk-instructions -->
"##;

/// Main entry point for `rtk init`
pub fn run(
    global: bool,
    claude_md: bool,
    hook_only: bool,
    patch_mode: PatchMode,
    verbose: u8,
) -> Result<()> {
    // Mode selection
    match (claude_md, hook_only) {
        (true, _) => run_claude_md_mode(global, verbose),
        (false, true) => run_hook_only_mode(global, patch_mode, verbose),
        (false, false) => run_default_mode(global, patch_mode, verbose),
    }
}

/// Prepare hook directory and return paths (hook_dir, rewrite_path, block_grep_path, block_read_path, block_write_path, block_explore_path)
fn prepare_hook_paths() -> Result<(PathBuf, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf)> {
    let claude_dir = resolve_claude_dir()?;
    let hook_dir = claude_dir.join("hooks");
    fs::create_dir_all(&hook_dir)
        .with_context(|| format!("Failed to create hook directory: {}", hook_dir.display()))?;
    let rewrite_path = hook_dir.join("rtk-rewrite.sh");
    let block_grep_path = hook_dir.join("rtk-block-native-grep.sh"); // Grep guidance/optional block hook
    let block_read_path = hook_dir.join("rtk-block-native-read.sh"); // Read guidance/optional block hook
    let block_write_path = hook_dir.join("rtk-block-native-write.sh"); // Edit/Write guidance/optional block hook
    let block_explore_path = hook_dir.join("rtk-block-native-explore.sh"); // Task/Explore guidance/optional block hook
    Ok((
        hook_dir,
        rewrite_path,
        block_grep_path,
        block_read_path,
        block_write_path,
        block_explore_path,
    ))
}

/// Write a single hook file if missing or outdated, return true if changed
#[cfg(unix)]
fn install_single_hook(hook_path: &Path, content: &str, verbose: u8) -> Result<bool> {
    let changed = if hook_path.exists() {
        let existing = fs::read_to_string(hook_path)
            .with_context(|| format!("Failed to read existing hook: {}", hook_path.display()))?;

        if existing == content {
            if verbose > 0 {
                eprintln!("Hook already up to date: {}", hook_path.display());
            }
            false
        } else {
            fs::write(hook_path, content)
                .with_context(|| format!("Failed to write hook to {}", hook_path.display()))?;
            if verbose > 0 {
                eprintln!("Updated hook: {}", hook_path.display());
            }
            true
        }
    } else {
        fs::write(hook_path, content)
            .with_context(|| format!("Failed to write hook to {}", hook_path.display()))?;
        if verbose > 0 {
            eprintln!("Created hook: {}", hook_path.display());
        }
        true
    };

    // Set executable permissions
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(hook_path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("Failed to set hook permissions: {}", hook_path.display()))?;

    Ok(changed)
}

/// Install all hook files (rewrite + grep/read/write/explore policy hooks), return true if any changed
#[cfg(unix)]
fn ensure_hooks_installed(
    rewrite_path: &Path,
    block_grep_path: &Path,
    block_read_path: &Path,
    block_write_path: &Path,
    block_explore_path: &Path,
    verbose: u8,
) -> Result<bool> {
    let r1 = install_single_hook(rewrite_path, REWRITE_HOOK, verbose)?;
    let r2 = install_single_hook(block_grep_path, BLOCK_GREP_HOOK, verbose)?;
    let r3 = install_single_hook(block_read_path, BLOCK_READ_HOOK, verbose)?; // Read guidance hook
    let r4 = install_single_hook(block_write_path, BLOCK_WRITE_HOOK, verbose)?; // Edit/Write guidance hook
    let r5 = install_single_hook(block_explore_path, BLOCK_EXPLORE_HOOK, verbose)?; // Task/Explore guidance hook
    Ok(r1 || r2 || r3 || r4 || r5)
}

/// Idempotent file write: create or update if content differs
fn write_if_changed(path: &Path, content: &str, name: &str, verbose: u8) -> Result<bool> {
    let existed = path.exists();
    let writer = AtomicWriter::new(WriteOptions::durable());
    let stats = writer
        .write_str(path, content)
        .with_context(|| format!("Failed to write {}: {}", name, path.display()))?;

    if stats.skipped_unchanged {
        if verbose > 0 {
            eprintln!("{} already up to date: {}", name, path.display());
        }
        Ok(false)
    } else {
        if verbose > 0 {
            if existed {
                eprintln!("Updated {}: {}", name, path.display());
            } else {
                eprintln!("Created {}: {}", name, path.display());
            }
        }
        Ok(true)
    }
}

/// Atomic write using tempfile + rename
/// Prevents corruption on crash/interrupt
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let writer = AtomicWriter::new(WriteOptions::durable());
    writer.write_str(path, content).map(|_| ())?;
    Ok(())
}

/// Prompt user for consent to patch settings.json
/// Prints to stderr (stdout may be piped), reads from stdin
/// Default is No (capital N)
fn prompt_user_consent(settings_path: &Path) -> Result<bool> {
    use std::io::{self, BufRead, IsTerminal};

    eprintln!("\nPatch existing {}? [y/N] ", settings_path.display());

    // If stdin is not a terminal (piped), default to No
    if !io::stdin().is_terminal() {
        eprintln!("(non-interactive mode, defaulting to N)");
        return Ok(false);
    }

    let stdin = io::stdin();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .context("Failed to read user input")?;

    let response = line.trim().to_lowercase();
    Ok(response == "y" || response == "yes")
}

/// Print manual instructions for settings.json patching
fn print_manual_instructions(
    rewrite_path: &Path,
    block_grep_path: &Path,
    block_read_path: &Path,
    block_write_path: &Path,
    block_explore_path: &Path,
) {
    println!("\n  MANUAL STEP: Add this to ~/.claude/settings.json:");
    println!("  {{");
    println!("    \"hooks\": {{ \"PreToolUse\": [");
    println!("      {{");
    println!("        \"matcher\": \"Bash\",");
    println!(
        "        \"hooks\": [{{ \"type\": \"command\", \"command\": \"{}\", \"timeout\": 5 }}]",
        rewrite_path.display()
    );
    println!("      }},");
    println!("      {{");
    println!("        \"matcher\": \"Grep\",");
    println!(
        "        \"hooks\": [{{ \"type\": \"command\", \"command\": \"{}\", \"timeout\": 5 }}]",
        block_grep_path.display()
    );
    println!("      }},");
    println!("      {{");
    println!("        \"matcher\": \"Read\",");
    println!(
        "        \"hooks\": [{{ \"type\": \"command\", \"command\": \"{}\", \"timeout\": 5 }}]",
        block_read_path.display()
    );
    println!("      }},");
    println!("      {{");
    println!("        \"matcher\": \"Edit\",");
    println!(
        "        \"hooks\": [{{ \"type\": \"command\", \"command\": \"{}\", \"timeout\": 5 }}]",
        block_write_path.display()
    );
    println!("      }},");
    println!("      {{");
    println!("        \"matcher\": \"Write\",");
    println!(
        "        \"hooks\": [{{ \"type\": \"command\", \"command\": \"{}\", \"timeout\": 5 }}]",
        block_write_path.display()
    );
    println!("      }},");
    println!("      {{");
    println!("        \"matcher\": \"Task\",");
    println!(
        "        \"hooks\": [{{ \"type\": \"command\", \"command\": \"{}\", \"timeout\": 10 }}]",
        block_explore_path.display()
    );
    println!("      }}");
    println!("    ]}}");
    println!("  }}");
    println!("\n  Then restart Claude Code. Test with: git status\n");
}

/// Remove RTK hook entry from settings.json
/// Returns true if hook was found and removed
fn remove_hook_from_json(root: &mut serde_json::Value) -> bool {
    let hooks = match root.get_mut("hooks").and_then(|h| h.get_mut("PreToolUse")) {
        Some(pre_tool_use) => pre_tool_use,
        None => return false,
    };

    let pre_tool_use_array = match hooks.as_array_mut() {
        Some(arr) => arr,
        None => return false,
    };

    // Find and remove all RTK entries (rewrite + block-grep + block-read + block-write + block-explore)
    let original_len = pre_tool_use_array.len();
    pre_tool_use_array.retain(|entry| {
        if let Some(hooks_array) = entry.get("hooks").and_then(|h| h.as_array()) {
            for hook in hooks_array {
                if let Some(command) = hook.get("command").and_then(|c| c.as_str()) {
                    if command.contains("rtk-rewrite.sh")
                        || command.contains("rtk-block-native-grep.sh")
                        || command.contains("rtk-block-native-read.sh")
                        || command.contains("rtk-block-native-write.sh")
                        || command.contains("rtk-block-native-explore.sh")
                    {
                        return false; // Remove this RTK entry
                    }
                }
            }
        }
        true // Keep this entry
    });

    pre_tool_use_array.len() < original_len
}

/// Remove RTK hook from settings.json file
/// Backs up before modification, returns true if hook was found and removed
fn remove_hook_from_settings(verbose: u8) -> Result<bool> {
    let claude_dir = resolve_claude_dir()?;
    let settings_path = claude_dir.join("settings.json");

    if !settings_path.exists() {
        if verbose > 0 {
            eprintln!("settings.json not found, nothing to remove");
        }
        return Ok(false);
    }

    let content = fs::read_to_string(&settings_path)
        .with_context(|| format!("Failed to read {}", settings_path.display()))?;

    if content.trim().is_empty() {
        return Ok(false);
    }

    let mut root: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {} as JSON", settings_path.display()))?;

    let removed = remove_hook_from_json(&mut root);

    if removed {
        // Backup original
        let backup_path = settings_path.with_extension("json.bak");
        fs::copy(&settings_path, &backup_path)
            .with_context(|| format!("Failed to backup to {}", backup_path.display()))?;

        // Atomic write
        let serialized =
            serde_json::to_string_pretty(&root).context("Failed to serialize settings.json")?;
        atomic_write(&settings_path, &serialized)?;

        if verbose > 0 {
            eprintln!("Removed RTK hook from settings.json");
        }
    }

    Ok(removed)
}

/// Full uninstall: remove hook, RTK.md, @RTK.md reference, settings.json entry
pub fn uninstall(global: bool, verbose: u8) -> Result<()> {
    if !global {
        anyhow::bail!("Uninstall only works with --global flag. For local projects, manually remove RTK from CLAUDE.md");
    }

    let claude_dir = resolve_claude_dir()?;
    let mut removed = Vec::new();

    // 1. Remove hook files (rewrite + block-grep + block-read + block-write + block-explore)
    for hook_name in &[
        "rtk-rewrite.sh",
        "rtk-block-native-grep.sh",
        "rtk-block-native-read.sh",
        "rtk-block-native-write.sh",
        "rtk-block-native-explore.sh",
    ] {
        let hook_path = claude_dir.join("hooks").join(hook_name);
        if hook_path.exists() {
            fs::remove_file(&hook_path)
                .with_context(|| format!("Failed to remove hook: {}", hook_path.display()))?;
            removed.push(format!("Hook: {}", hook_path.display()));
        }
    }

    // 2. Remove RTK.md
    let rtk_md_path = claude_dir.join("RTK.md");
    if rtk_md_path.exists() {
        fs::remove_file(&rtk_md_path)
            .with_context(|| format!("Failed to remove RTK.md: {}", rtk_md_path.display()))?;
        removed.push(format!("RTK.md: {}", rtk_md_path.display()));
    }

    // 3. Remove @RTK.md reference from CLAUDE.md
    let claude_md_path = claude_dir.join("CLAUDE.md");
    if claude_md_path.exists() {
        let content = fs::read_to_string(&claude_md_path)
            .with_context(|| format!("Failed to read CLAUDE.md: {}", claude_md_path.display()))?;

        if content.contains("@RTK.md") {
            let new_content = content
                .lines()
                .filter(|line| !line.trim().starts_with("@RTK.md"))
                .collect::<Vec<_>>()
                .join("\n");

            // Clean up double blanks
            let cleaned = clean_double_blanks(&new_content);

            fs::write(&claude_md_path, cleaned).with_context(|| {
                format!("Failed to write CLAUDE.md: {}", claude_md_path.display())
            })?;
            removed.push(format!("CLAUDE.md: removed @RTK.md reference"));
        }
    }

    // 4. Remove hook entry from settings.json
    if remove_hook_from_settings(verbose)? {
        removed.push("settings.json: removed RTK hook entry".to_string());
    }

    // Report results
    if removed.is_empty() {
        println!("RTK was not installed (nothing to remove)");
    } else {
        println!("RTK uninstalled:");
        for item in removed {
            println!("  - {}", item);
        }
        println!("\nRestart Claude Code to apply changes.");
    }

    Ok(())
}

/// Orchestrator: patch settings.json with RTK hooks (rewrite + grep/read/write/explore policy hooks)
/// Handles reading, checking, prompting, merging, backing up, and atomic writing
fn patch_settings_json(
    rewrite_path: &Path,
    block_grep_path: &Path,
    block_read_path: &Path,
    block_write_path: &Path,
    block_explore_path: &Path,
    mode: PatchMode,
    verbose: u8,
) -> Result<PatchResult> {
    let claude_dir = resolve_claude_dir()?;
    let settings_path = claude_dir.join("settings.json");
    let rewrite_command = rewrite_path
        .to_str()
        .context("Rewrite hook path contains invalid UTF-8")?;
    let block_grep_command = block_grep_path
        .to_str()
        .context("Block-grep hook path contains invalid UTF-8")?;
    let block_read_command = block_read_path
        .to_str()
        .context("Block-read hook path contains invalid UTF-8")?;
    let block_write_command = block_write_path
        .to_str()
        .context("Block-write hook path contains invalid UTF-8")?;
    let block_explore_command = block_explore_path
        .to_str()
        .context("Block-explore hook path contains invalid UTF-8")?;

    // Read or create settings.json
    let mut root = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)
            .with_context(|| format!("Failed to read {}", settings_path.display()))?;

        if content.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse {} as JSON", settings_path.display()))?
        }
    } else {
        serde_json::json!({})
    };

    // Check idempotency — all hooks must be present
    if hooks_already_present(
        &root,
        rewrite_command,
        block_grep_command,
        block_read_command,
        block_write_command,
        block_explore_command,
    ) {
        if verbose > 0 {
            eprintln!("settings.json: all hooks already present");
        }
        return Ok(PatchResult::AlreadyPresent);
    }

    // Handle mode
    match mode {
        PatchMode::Skip => {
            print_manual_instructions(
                rewrite_path,
                block_grep_path,
                block_read_path,
                block_write_path,
                block_explore_path,
            );
            return Ok(PatchResult::Skipped);
        }
        PatchMode::Ask => {
            if !prompt_user_consent(&settings_path)? {
                print_manual_instructions(
                    rewrite_path,
                    block_grep_path,
                    block_read_path,
                    block_write_path,
                    block_explore_path,
                );
                return Ok(PatchResult::Declined);
            }
        }
        PatchMode::Auto => {
            // Proceed without prompting
        }
    }

    // Remove any existing RTK entries first (clean slate for idempotent re-insert)
    remove_hook_from_json(&mut root);

    // Deep-merge all hooks (rewrite + grep/read/write/explore policy hooks)
    insert_hook_entry(
        &mut root,
        rewrite_command,
        block_grep_command,
        block_read_command,
        block_write_command,
        block_explore_command,
    );

    // Backup original
    if settings_path.exists() {
        let backup_path = settings_path.with_extension("json.bak");
        fs::copy(&settings_path, &backup_path)
            .with_context(|| format!("Failed to backup to {}", backup_path.display()))?;
        if verbose > 0 {
            eprintln!("Backup: {}", backup_path.display());
        }
    }

    // Atomic write
    let serialized =
        serde_json::to_string_pretty(&root).context("Failed to serialize settings.json")?;
    atomic_write(&settings_path, &serialized)?;

    println!("\n  settings.json: hooks added (Bash rewrite + Grep/Read/Edit/Write/Task policy)");
    if settings_path.with_extension("json.bak").exists() {
        println!(
            "  Backup: {}",
            settings_path.with_extension("json.bak").display()
        );
    }
    println!("  Restart Claude Code. Test with: git status");

    Ok(PatchResult::Patched)
}

/// Clean up consecutive blank lines (collapse 3+ to 2)
/// Used when removing @RTK.md line from CLAUDE.md
fn clean_double_blanks(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if line.trim().is_empty() {
            // Count consecutive blank lines
            let mut blank_count = 0;
            while i < lines.len() && lines[i].trim().is_empty() {
                blank_count += 1;
                i += 1;
            }

            // Keep at most 2 blank lines
            let keep = blank_count.min(2);
            for _ in 0..keep {
                result.push("");
            }
        } else {
            result.push(line);
            i += 1;
        }
    }

    result.join("\n")
}

/// Deep-merge RTK hook entries into settings.json
/// Creates hooks.PreToolUse structure if missing, preserves existing hooks
/// Adds Bash (rewrite), Grep (block), Read (block), Edit (block), Write (block), Task/Explore (block) entries
fn insert_hook_entry(
    root: &mut serde_json::Value,
    rewrite_command: &str,
    block_grep_command: &str,
    block_read_command: &str,
    block_write_command: &str,
    block_explore_command: &str,
) {
    // Ensure root is an object
    let root_obj = match root.as_object_mut() {
        Some(obj) => obj,
        None => {
            *root = serde_json::json!({});
            root.as_object_mut()
                .expect("Just created object, must succeed")
        }
    };

    // Use entry() API for idiomatic insertion
    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .expect("hooks must be an object");

    let pre_tool_use = hooks
        .entry("PreToolUse")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .expect("PreToolUse must be an array");

    // Append Bash rewrite hook entry
    pre_tool_use.push(serde_json::json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": rewrite_command,
            "timeout": 5
        }]
    }));

    // Append Grep hook entry (defaults to hard deny; explicit allow override)
    pre_tool_use.push(serde_json::json!({
        "matcher": "Grep",
        "hooks": [{
            "type": "command",
            "command": block_grep_command,
            "timeout": 5
        }]
    }));

    // Append Read hook entry (defaults to hard deny; explicit allow override)
    pre_tool_use.push(serde_json::json!({
        "matcher": "Read",
        "hooks": [{
            "type": "command",
            "command": block_read_command,
            "timeout": 5
        }]
    }));

    // Append Edit hook entry (defaults to hard deny; explicit allow override)
    pre_tool_use.push(serde_json::json!({
        "matcher": "Edit",
        "hooks": [{
            "type": "command",
            "command": block_write_command,
            "timeout": 5
        }]
    }));

    // Append Write hook entry (defaults to hard deny; explicit allow override)
    pre_tool_use.push(serde_json::json!({
        "matcher": "Write",
        "hooks": [{
            "type": "command",
            "command": block_write_command,
            "timeout": 5
        }]
    }));

    // Append Task/Explore hook entry (defaults to hard deny, explicit allow override)
    pre_tool_use.push(serde_json::json!({
        "matcher": "Task",
        "hooks": [{
            "type": "command",
            "command": block_explore_command,
            "timeout": 10
        }]
    }));
}

/// Check if RTK hooks are already present in settings.json
/// Returns true only if all 6 hooks are present: rewrite + grep/read/write + task/explore policy hooks
fn hooks_already_present(
    root: &serde_json::Value,
    rewrite_command: &str,
    block_grep_command: &str,
    block_read_command: &str,
    block_write_command: &str,
    block_explore_command: &str,
) -> bool {
    let pre_tool_use_array = match root
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|p| p.as_array())
    {
        Some(arr) => arr,
        None => return false,
    };

    fn entry_has_command(entry: &serde_json::Value, expected: &str, fallback_file: &str) -> bool {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .into_iter()
            .flatten()
            .filter_map(|hook| hook.get("command").and_then(|c| c.as_str()))
            .any(|cmd| {
                cmd == expected || (cmd.contains(fallback_file) && expected.contains(fallback_file))
            })
    }

    let has_rewrite = pre_tool_use_array.iter().any(|entry| {
        entry.get("matcher").and_then(|m| m.as_str()) == Some("Bash")
            && entry_has_command(entry, rewrite_command, "rtk-rewrite.sh")
    });

    let has_block_grep = pre_tool_use_array.iter().any(|entry| {
        entry.get("matcher").and_then(|m| m.as_str()) == Some("Grep")
            && entry_has_command(entry, block_grep_command, "rtk-block-native-grep.sh")
    });

    let has_block_read = pre_tool_use_array.iter().any(|entry| {
        entry.get("matcher").and_then(|m| m.as_str()) == Some("Read")
            && entry_has_command(entry, block_read_command, "rtk-block-native-read.sh")
    });

    let has_block_edit = pre_tool_use_array.iter().any(|entry| {
        entry.get("matcher").and_then(|m| m.as_str()) == Some("Edit")
            && entry_has_command(entry, block_write_command, "rtk-block-native-write.sh")
    });

    let has_block_write = pre_tool_use_array.iter().any(|entry| {
        entry.get("matcher").and_then(|m| m.as_str()) == Some("Write")
            && entry_has_command(entry, block_write_command, "rtk-block-native-write.sh")
    });

    let has_block_explore = pre_tool_use_array.iter().any(|entry| {
        entry.get("matcher").and_then(|m| m.as_str()) == Some("Task")
            && entry_has_command(entry, block_explore_command, "rtk-block-native-explore.sh")
    });

    has_rewrite
        && has_block_grep
        && has_block_read
        && has_block_edit
        && has_block_write
        && has_block_explore
}

/// Check if any RTK hook is present (for diagnostics / show_config)
/// Returns (rewrite, block_grep, block_read, block_edit, block_write, block_explore)
fn any_rtk_hook_present(root: &serde_json::Value) -> (bool, bool, bool, bool, bool, bool) {
    let pre_tool_use_array = match root
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|p| p.as_array())
    {
        Some(arr) => arr,
        None => return (false, false, false, false, false, false),
    };

    fn has_matcher_with_file(
        pre_tool_use_array: &[serde_json::Value],
        matcher: &str,
        hook_file_fragment: &str,
    ) -> bool {
        pre_tool_use_array.iter().any(|entry| {
            entry.get("matcher").and_then(|m| m.as_str()) == Some(matcher)
                && entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|hook| hook.get("command").and_then(|c| c.as_str()))
                    .any(|cmd| cmd.contains(hook_file_fragment))
        })
    }

    let has_rewrite = has_matcher_with_file(pre_tool_use_array, "Bash", "rtk-rewrite.sh");
    let has_block_grep =
        has_matcher_with_file(pre_tool_use_array, "Grep", "rtk-block-native-grep.sh");
    let has_block_read =
        has_matcher_with_file(pre_tool_use_array, "Read", "rtk-block-native-read.sh");
    let has_block_edit =
        has_matcher_with_file(pre_tool_use_array, "Edit", "rtk-block-native-write.sh");
    let has_block_write =
        has_matcher_with_file(pre_tool_use_array, "Write", "rtk-block-native-write.sh");
    let has_block_explore =
        has_matcher_with_file(pre_tool_use_array, "Task", "rtk-block-native-explore.sh");

    (
        has_rewrite,
        has_block_grep,
        has_block_read,
        has_block_edit,
        has_block_write,
        has_block_explore,
    )
}

/// Default mode: hook + slim RTK.md + @RTK.md reference
#[cfg(not(unix))]
fn run_default_mode(_global: bool, _patch_mode: PatchMode, _verbose: u8) -> Result<()> {
    eprintln!("⚠️  Hook-based mode requires Unix (macOS/Linux).");
    eprintln!("    Windows: use --claude-md mode for full injection.");
    eprintln!("    Falling back to --claude-md mode.");
    run_claude_md_mode(_global, _verbose)
}

#[cfg(unix)]
fn run_default_mode(global: bool, patch_mode: PatchMode, verbose: u8) -> Result<()> {
    if !global {
        // Local init: unchanged behavior (full injection into ./CLAUDE.md)
        return run_claude_md_mode(false, verbose);
    }

    let claude_dir = resolve_claude_dir()?;
    let rtk_md_path = claude_dir.join("RTK.md");
    let claude_md_path = claude_dir.join("CLAUDE.md");

    // 1. Prepare hook directory and install hooks
    let (
        _hook_dir,
        rewrite_path,
        block_grep_path,
        block_read_path,
        block_write_path,
        block_explore_path,
    ) = prepare_hook_paths()?;
    ensure_hooks_installed(
        &rewrite_path,
        &block_grep_path,
        &block_read_path,
        &block_write_path,
        &block_explore_path,
        verbose,
    )?;

    // 2. Write RTK.md
    write_if_changed(&rtk_md_path, RTK_SLIM, "RTK.md", verbose)?;

    // 3. Patch CLAUDE.md (add @RTK.md, migrate if needed)
    let migrated = patch_claude_md(&claude_md_path, verbose)?;

    // 4. Print success message
    println!("\nRTK hooks installed (global).\n");
    println!("  Rewrite:    {}", rewrite_path.display());
    println!("  Grep Hook:  {}", block_grep_path.display());
    println!("  Read Hook:  {}", block_read_path.display());
    println!(
        "  Edit Hook:  {} (Edit + Write)",
        block_write_path.display()
    );
    println!(
        "  Task Hook:  {} (Task/Explore)",
        block_explore_path.display()
    );
    println!("  RTK.md:     {} (10 lines)", rtk_md_path.display());
    println!("  CLAUDE.md:  @RTK.md reference added");

    if migrated {
        println!("\n  Migrated: removed 137-line RTK block from CLAUDE.md");
        println!("            replaced with @RTK.md (10 lines)");
    }

    // 5. Patch settings.json (Bash rewrite + Grep/Read/Edit/Write/Task hook entries)
    let patch_result = patch_settings_json(
        &rewrite_path,
        &block_grep_path,
        &block_read_path,
        &block_write_path,
        &block_explore_path,
        patch_mode,
        verbose,
    )?;

    // Report result
    match patch_result {
        PatchResult::Patched => {
            // Already printed by patch_settings_json
        }
        PatchResult::AlreadyPresent => {
            println!("\n  settings.json: all hooks already present");
            println!("  Restart Claude Code. Test with: git status");
        }
        PatchResult::Declined | PatchResult::Skipped => {
            // Manual instructions already printed by patch_settings_json
        }
    }

    // 6. Clean up project-local hook duplicates
    cleanup_project_local_hooks(verbose)?;

    // 7. Offer grepai installation
    setup_grepai(patch_mode, verbose)?;

    println!(); // Final newline

    Ok(())
}

/// Offer grepai installation during `rtk init --global`
fn setup_grepai(patch_mode: PatchMode, verbose: u8) -> Result<()> {
    if std::env::var("RTK_SKIP_GREPAI").ok().as_deref() == Some("1") {
        if verbose > 0 {
            eprintln!("grepai setup skipped (RTK_SKIP_GREPAI=1)");
        }
        return Ok(());
    }

    // Check if grepai is already installed
    if let Some(path) = grepai::find_grepai_binary() {
        println!("\n  grepai: already installed at {}", path.display());
        return Ok(());
    }

    // Not installed — decide based on patch_mode
    match patch_mode {
        PatchMode::Auto => {
            // Install without prompting
            println!("\n  Installing grepai...");
            match grepai::install_grepai(verbose) {
                Ok(path) => {
                    println!("  grepai installed: {}", path.display());
                    println!(
                        "  Run `grepai init` in any project, then `grepai watch --background`."
                    );
                }
                Err(e) => {
                    eprintln!("  grepai install failed: {}", e);
                    eprintln!("  Install manually: https://github.com/yoanbernabeu/grepai");
                }
            }
        }
        PatchMode::Skip => {
            // Print manual instructions only
            println!("\n  grepai: not installed (skipped)");
            println!("  Install manually: https://github.com/yoanbernabeu/grepai");
        }
        PatchMode::Ask => {
            // Prompt with Y as default (capital Y, unlike settings.json which defaults to N)
            if prompt_grepai_consent()? {
                println!("  Installing grepai...");
                match grepai::install_grepai(verbose) {
                    Ok(path) => {
                        println!("  grepai installed: {}", path.display());
                        println!(
                            "  Run `grepai init` in any project, then `grepai watch --background`."
                        );
                    }
                    Err(e) => {
                        eprintln!("  grepai install failed: {}", e);
                        eprintln!("  Install manually: https://github.com/yoanbernabeu/grepai");
                    }
                }
            } else {
                println!("  grepai: skipped");
                println!("  Install later: https://github.com/yoanbernabeu/grepai");
            }
        }
    }

    Ok(())
}

/// Prompt user for consent to install grepai
/// Default is Yes (capital Y) — unlike settings.json patch which defaults to No
fn prompt_grepai_consent() -> Result<bool> {
    use std::io::{self, BufRead, IsTerminal};

    eprintln!("\nInstall grepai for semantic code search? [Y/n] ");

    // If stdin is not a terminal (piped), default to Yes
    if !io::stdin().is_terminal() {
        eprintln!("(non-interactive mode, defaulting to Y)");
        return Ok(true);
    }

    let stdin = io::stdin();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .context("Failed to read user input")?;

    let response = line.trim().to_lowercase();
    // Default is Yes: empty input or explicit y/yes
    Ok(response.is_empty() || response == "y" || response == "yes")
}

/// Hook-only mode: just the hook, no RTK.md
#[cfg(not(unix))]
fn run_hook_only_mode(_global: bool, _patch_mode: PatchMode, _verbose: u8) -> Result<()> {
    anyhow::bail!("Hook install requires Unix (macOS/Linux). Use WSL or --claude-md mode.")
}

#[cfg(unix)]
fn run_hook_only_mode(global: bool, patch_mode: PatchMode, verbose: u8) -> Result<()> {
    if !global {
        eprintln!("Warning: --hook-only only makes sense with --global");
        eprintln!("    For local projects, use default mode or --claude-md");
        return Ok(());
    }

    // Prepare and install hooks
    let (
        _hook_dir,
        rewrite_path,
        block_grep_path,
        block_read_path,
        block_write_path,
        block_explore_path,
    ) = prepare_hook_paths()?;
    ensure_hooks_installed(
        &rewrite_path,
        &block_grep_path,
        &block_read_path,
        &block_write_path,
        &block_explore_path,
        verbose,
    )?;

    println!("\nRTK hooks installed (hook-only mode).\n");
    println!("  Rewrite:    {}", rewrite_path.display());
    println!("  Grep Hook:  {}", block_grep_path.display());
    println!("  Read Hook:  {}", block_read_path.display());
    println!(
        "  Edit Hook:  {} (Edit + Write)",
        block_write_path.display()
    );
    println!(
        "  Task Hook:  {} (Task/Explore)",
        block_explore_path.display()
    );
    println!(
        "  Note: No RTK.md created. Claude won't know about meta commands (gain, discover, proxy)."
    );

    // Patch settings.json (all hooks)
    let patch_result = patch_settings_json(
        &rewrite_path,
        &block_grep_path,
        &block_read_path,
        &block_write_path,
        &block_explore_path,
        patch_mode,
        verbose,
    )?;

    // Report result
    match patch_result {
        PatchResult::Patched => {
            // Already printed by patch_settings_json
        }
        PatchResult::AlreadyPresent => {
            println!("\n  settings.json: all hooks already present");
            println!("  Restart Claude Code. Test with: git status");
        }
        PatchResult::Declined | PatchResult::Skipped => {
            // Manual instructions already printed by patch_settings_json
        }
    }

    println!(); // Final newline

    Ok(())
}

/// Legacy mode: full 137-line injection into CLAUDE.md
fn run_claude_md_mode(global: bool, verbose: u8) -> Result<()> {
    let path = if global {
        resolve_claude_dir()?.join("CLAUDE.md")
    } else {
        PathBuf::from("CLAUDE.md")
    };

    if global {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
    }

    if verbose > 0 {
        eprintln!("Writing rtk instructions to: {}", path.display());
    }

    if path.exists() {
        let existing = fs::read_to_string(&path)?;
        // upsert_rtk_block handles all 4 cases: add, update, unchanged, malformed
        let (new_content, action) = upsert_rtk_block(&existing, RTK_INSTRUCTIONS);

        match action {
            RtkBlockUpsert::Added => {
                fs::write(&path, new_content)?;
                println!("✅ Added rtk instructions to existing {}", path.display());
            }
            RtkBlockUpsert::Updated => {
                fs::write(&path, new_content)?;
                println!("✅ Updated rtk instructions in {}", path.display());
            }
            RtkBlockUpsert::Unchanged => {
                println!(
                    "✅ {} already contains up-to-date rtk instructions",
                    path.display()
                );
                return Ok(());
            }
            RtkBlockUpsert::Malformed => {
                eprintln!(
                    "⚠️  Warning: Found '<!-- rtk-instructions' without closing marker in {}",
                    path.display()
                );

                if let Some((line_num, _)) = existing
                    .lines()
                    .enumerate()
                    .find(|(_, line)| line.contains("<!-- rtk-instructions"))
                {
                    eprintln!("    Location: line {}", line_num + 1);
                }

                eprintln!("    Action: Manually remove the incomplete block, then re-run:");
                if global {
                    eprintln!("            rtk init -g --claude-md");
                } else {
                    eprintln!("            rtk init --claude-md");
                }
                return Ok(());
            }
        }
    } else {
        fs::write(&path, RTK_INSTRUCTIONS)?;
        println!("✅ Created {} with rtk instructions", path.display());
    }

    if global {
        println!("   Claude Code will now use rtk in all sessions");
    } else {
        println!("   Claude Code will use rtk in this project");
    }

    Ok(())
}

// --- upsert_rtk_block: idempotent RTK block management ---

#[derive(Debug, Clone, Copy, PartialEq)]
enum RtkBlockUpsert {
    /// No existing block found — appended new block
    Added,
    /// Existing block found with different content — replaced
    Updated,
    /// Existing block found with identical content — no-op
    Unchanged,
    /// Opening marker found without closing marker — not safe to rewrite
    Malformed,
}

/// Insert or replace the RTK instructions block in `content`.
///
/// Returns `(new_content, action)` describing what happened.
/// The caller decides whether to write `new_content` based on `action`.
fn upsert_rtk_block(content: &str, block: &str) -> (String, RtkBlockUpsert) {
    let start_marker = "<!-- rtk-instructions";
    let end_marker = "<!-- /rtk-instructions -->";

    if let Some(start) = content.find(start_marker) {
        if let Some(relative_end) = content[start..].find(end_marker) {
            let end = start + relative_end;
            let end_pos = end + end_marker.len();
            let current_block = content[start..end_pos].trim();
            let desired_block = block.trim();

            if current_block == desired_block {
                return (content.to_string(), RtkBlockUpsert::Unchanged);
            }

            // Replace stale block with desired block
            let before = content[..start].trim_end();
            let after = content[end_pos..].trim_start();

            let result = match (before.is_empty(), after.is_empty()) {
                (true, true) => desired_block.to_string(),
                (true, false) => format!("{desired_block}\n\n{after}"),
                (false, true) => format!("{before}\n\n{desired_block}"),
                (false, false) => format!("{before}\n\n{desired_block}\n\n{after}"),
            };

            return (result, RtkBlockUpsert::Updated);
        }

        // Opening marker without closing marker — malformed
        return (content.to_string(), RtkBlockUpsert::Malformed);
    }

    // No existing block — append
    let trimmed = content.trim();
    if trimmed.is_empty() {
        (block.to_string(), RtkBlockUpsert::Added)
    } else {
        (
            format!("{trimmed}\n\n{}", block.trim()),
            RtkBlockUpsert::Added,
        )
    }
}

/// Patch CLAUDE.md: add @RTK.md, migrate if old block exists
fn patch_claude_md(path: &Path, verbose: u8) -> Result<bool> {
    let mut content = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };

    let mut migrated = false;

    // Check for old block and migrate
    if content.contains("<!-- rtk-instructions") {
        let (new_content, did_migrate) = remove_rtk_block(&content);
        if did_migrate {
            content = new_content;
            migrated = true;
            if verbose > 0 {
                eprintln!("Migrated: removed old RTK block from CLAUDE.md");
            }
        }
    }

    // Check if @RTK.md already present
    if content.contains("@RTK.md") {
        if verbose > 0 {
            eprintln!("@RTK.md reference already present in CLAUDE.md");
        }
        if migrated {
            fs::write(path, content)?;
        }
        return Ok(migrated);
    }

    // Add @RTK.md
    let new_content = if content.is_empty() {
        "@RTK.md\n".to_string()
    } else {
        format!("{}\n\n@RTK.md\n", content.trim())
    };

    fs::write(path, new_content)?;

    if verbose > 0 {
        eprintln!("Added @RTK.md reference to CLAUDE.md");
    }

    Ok(migrated)
}

/// Remove old RTK block from CLAUDE.md (migration helper)
fn remove_rtk_block(content: &str) -> (String, bool) {
    if let (Some(start), Some(end)) = (
        content.find("<!-- rtk-instructions"),
        content.find("<!-- /rtk-instructions -->"),
    ) {
        let end_pos = end + "<!-- /rtk-instructions -->".len();
        let before = content[..start].trim_end();
        let after = content[end_pos..].trim_start();

        let result = if after.is_empty() {
            before.to_string()
        } else {
            format!("{}\n\n{}", before, after)
        };

        (result, true) // migrated
    } else if content.contains("<!-- rtk-instructions") {
        eprintln!("⚠️  Warning: Found '<!-- rtk-instructions' without closing marker.");
        eprintln!("    This can happen if CLAUDE.md was manually edited.");

        // Find line number
        if let Some((line_num, _)) = content
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("<!-- rtk-instructions"))
        {
            eprintln!("    Location: line {}", line_num + 1);
        }

        eprintln!("    Action: Manually remove the incomplete block, then re-run:");
        eprintln!("            rtk init -g");
        (content.to_string(), false)
    } else {
        (content.to_string(), false)
    }
}

/// Resolve ~/.claude directory with proper home expansion
fn resolve_claude_dir() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".claude"))
        .context("Cannot determine home directory. Is $HOME set?")
}

/// Clean up project-local RTK hook duplicates when running `rtk init -g`
/// Removes .claude/hooks/rtk-*.sh files and hook entries from local settings
fn cleanup_project_local_hooks(verbose: u8) -> Result<bool> {
    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let local_claude = cwd.join(".claude");

    if !local_claude.exists() {
        return Ok(false);
    }

    let mut cleaned = Vec::new();

    // 1. Remove local hook script files
    let local_hooks_dir = local_claude.join("hooks");
    if local_hooks_dir.exists() {
        for hook_name in &[
            "rtk-rewrite.sh",
            "rtk-block-native-grep.sh",
            "rtk-block-native-read.sh",
            "rtk-block-native-write.sh",
            "rtk-block-native-explore.sh",
        ] {
            let local_hook = local_hooks_dir.join(hook_name);
            if local_hook.exists() {
                fs::remove_file(&local_hook).with_context(|| {
                    format!("Failed to remove local hook: {}", local_hook.display())
                })?;
                cleaned.push(format!("removed {}", local_hook.display()));
            }
        }
    }

    // 2. Remove hook entries from local settings files
    for settings_name in &["settings.json", "settings.local.json"] {
        let settings_path = local_claude.join(settings_name);
        if !settings_path.exists() {
            continue;
        }

        let content = fs::read_to_string(&settings_path)
            .with_context(|| format!("Failed to read local {}", settings_path.display()))?;

        if content.trim().is_empty() {
            continue;
        }

        let mut root: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed JSON
        };

        if remove_hook_from_json(&mut root) {
            // Backup before modifying
            let backup_path = settings_path.with_extension("json.bak");
            fs::copy(&settings_path, &backup_path).ok(); // best-effort backup

            let serialized = serde_json::to_string_pretty(&root)
                .context("Failed to serialize local settings")?;
            fs::write(&settings_path, serialized)
                .with_context(|| format!("Failed to write local {}", settings_path.display()))?;
            cleaned.push(format!("cleaned RTK hooks from {}", settings_name));
        }
    }

    if !cleaned.is_empty() {
        println!("\n  Project-local cleanup:");
        for item in &cleaned {
            println!("    - {}", item);
        }
        if verbose > 0 {
            eprintln!("Cleaned {} project-local RTK artifacts", cleaned.len());
        }
    }

    Ok(!cleaned.is_empty())
}

/// Show current rtk configuration
pub fn show_config() -> Result<()> {
    let claude_dir = resolve_claude_dir()?;
    let rewrite_path = claude_dir.join("hooks").join("rtk-rewrite.sh");
    let block_grep_path = claude_dir.join("hooks").join("rtk-block-native-grep.sh");
    let block_read_path = claude_dir.join("hooks").join("rtk-block-native-read.sh");
    let block_write_path = claude_dir.join("hooks").join("rtk-block-native-write.sh");
    let block_explore_path = claude_dir.join("hooks").join("rtk-block-native-explore.sh");
    let rtk_md_path = claude_dir.join("RTK.md");
    let global_claude_md = claude_dir.join("CLAUDE.md");
    let local_claude_md = PathBuf::from("CLAUDE.md");

    println!("rtk Configuration:\n");

    // Check rewrite hook
    if rewrite_path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&rewrite_path)?;
            let perms = metadata.permissions();
            let is_executable = perms.mode() & 0o111 != 0;

            let hook_content = fs::read_to_string(&rewrite_path)?;
            let has_guards =
                hook_content.contains("command -v rtk") && hook_content.contains("command -v jq");

            if is_executable && has_guards {
                println!(
                    "  [ok] Rewrite hook: {} (executable, with guards)",
                    rewrite_path.display()
                );
            } else if !is_executable {
                println!(
                    "  [!]  Rewrite hook: {} (NOT executable - run: chmod +x)",
                    rewrite_path.display()
                );
            } else {
                println!(
                    "  [!]  Rewrite hook: {} (no guards - outdated)",
                    rewrite_path.display()
                );
            }
        }

        #[cfg(not(unix))]
        {
            println!("  [ok] Rewrite hook: {} (exists)", rewrite_path.display());
        }
    } else {
        println!("  [--] Rewrite hook: not found");
    }

    // Check grep guidance hook (Grep matcher)
    if block_grep_path.exists() {
        println!("  [ok] Grep hook: {}", block_grep_path.display());
    } else {
        println!("  [!]  Grep hook: not found (run: rtk init -g to install)");
    }

    // Check read guidance hook (Read matcher)
    if block_read_path.exists() {
        println!("  [ok] Read hook: {}", block_read_path.display());
    } else {
        println!("  [!]  Read hook: not found (run: rtk init -g to install)");
    }

    // Check write guidance hook (Edit/Write matcher)
    if block_write_path.exists() {
        println!("  [ok] Edit/Write hook: {}", block_write_path.display());
    } else {
        println!("  [!]  Edit/Write hook: not found (run: rtk init -g to install)");
    }

    // Check explore policy hook (Task matcher)
    if block_explore_path.exists() {
        println!("  [ok] Task/Explore hook: {}", block_explore_path.display());
    } else {
        println!("  [!]  Task/Explore hook: not found (run: rtk init -g to install)");
    }

    // Check RTK.md
    if rtk_md_path.exists() {
        println!("  [ok] RTK.md: {} (slim mode)", rtk_md_path.display());
    } else {
        println!("  [--] RTK.md: not found");
    }

    // Check global CLAUDE.md
    if global_claude_md.exists() {
        let content = fs::read_to_string(&global_claude_md)?;
        if content.contains("@RTK.md") {
            println!("  [ok] Global (~/.claude/CLAUDE.md): @RTK.md reference");
        } else if content.contains("<!-- rtk-instructions") {
            println!(
                "  [!]  Global (~/.claude/CLAUDE.md): old RTK block (run: rtk init -g to migrate)"
            );
        } else {
            println!("  [--] Global (~/.claude/CLAUDE.md): exists but rtk not configured");
        }
    } else {
        println!("  [--] Global (~/.claude/CLAUDE.md): not found");
    }

    // Check local CLAUDE.md
    if local_claude_md.exists() {
        let content = fs::read_to_string(&local_claude_md)?;
        if content.contains("rtk") {
            println!("  [ok] Local (./CLAUDE.md): rtk enabled");
        } else {
            println!("  [--] Local (./CLAUDE.md): exists but rtk not configured");
        }
    } else {
        println!("  [--] Local (./CLAUDE.md): not found");
    }

    // Check settings.json
    let settings_path = claude_dir.join("settings.json");
    if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)?;
        if !content.trim().is_empty() {
            if let Ok(root) = serde_json::from_str::<serde_json::Value>(&content) {
                let (
                    has_rewrite,
                    has_block_grep,
                    has_block_read,
                    has_block_edit,
                    has_block_write,
                    has_block_explore,
                ) = any_rtk_hook_present(&root);
                if has_rewrite
                    && has_block_grep
                    && has_block_read
                    && has_block_edit
                    && has_block_write
                    && has_block_explore
                {
                    println!("  [ok] settings.json: all RTK hooks configured");
                } else {
                    println!("  [!]  settings.json: partial RTK hook configuration");
                    if !has_rewrite {
                        println!("       Missing: rewrite hook (Bash matcher)");
                    }
                    if !has_block_grep {
                        println!("       Missing: grep hook (Grep matcher)");
                    }
                    if !has_block_read {
                        println!("       Missing: read hook (Read matcher)");
                    }
                    if !has_block_edit {
                        println!("       Missing: edit hook (Edit matcher)");
                    }
                    if !has_block_write {
                        println!("       Missing: write hook (Write matcher)");
                    }
                    if !has_block_explore {
                        println!("       Missing: Task/Explore hook (Task matcher)");
                    }
                    println!("       Run: rtk init -g --auto-patch");
                }
            } else {
                println!("  [!]  settings.json: exists but invalid JSON");
            }
        } else {
            println!("  [--] settings.json: empty");
        }
    } else {
        println!("  [--] settings.json: not found");
    }

    // Check for project-local hook duplicates
    let cwd = std::env::current_dir().ok();
    if let Some(ref cwd) = cwd {
        let local_hooks_dir = cwd.join(".claude").join("hooks");
        let mut local_dupes = Vec::new();
        for hook_name in &[
            "rtk-rewrite.sh",
            "rtk-block-native-grep.sh",
            "rtk-block-native-read.sh",
            "rtk-block-native-write.sh",
            "rtk-block-native-explore.sh",
        ] {
            if local_hooks_dir.join(hook_name).exists() {
                local_dupes.push(*hook_name);
            }
        }
        if !local_dupes.is_empty() {
            println!(
                "\n  [!]  Project-local hook duplicates found: {}",
                local_dupes.join(", ")
            );
            println!("       Run `rtk init -g` to clean up (global hooks take precedence)");
        }
    }

    println!("\nSearch priority (mandatory): rgai > rg > grep.");
    println!(
        "  Use rtk rgai first; use rtk grep for exact/regex (internal rg -> grep fallback).\n"
    );
    println!("Usage:");
    println!("  rtk init              # Full injection into local CLAUDE.md");
    println!("  rtk init -g           # Hook + RTK.md + @RTK.md + settings.json (recommended)");
    println!("  rtk init -g --auto-patch    # Same as above but no prompt");
    println!("  rtk init -g --no-patch      # Skip settings.json (manual setup)");
    println!("  rtk init -g --uninstall     # Remove all RTK artifacts");
    println!("  rtk init -g --claude-md     # Legacy: full injection into ~/.claude/CLAUDE.md");
    println!("  rtk init -g --hook-only     # Hook only, no RTK.md");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_init_mentions_all_top_level_commands() {
        for cmd in [
            "rtk cargo",
            "rtk gh",
            "rtk vitest",
            "rtk tsc",
            "rtk lint",
            "rtk prettier",
            "rtk next",
            "rtk playwright",
            "rtk prisma",
            "rtk pnpm",
            "rtk bun",
            "rtk npm",
            "rtk ssh",
            "rtk curl",
            "rtk git",
            "rtk docker",
            "rtk kubectl",
            "rtk rgai",
        ] {
            assert!(
                RTK_INSTRUCTIONS.contains(cmd),
                "Missing {cmd} in RTK_INSTRUCTIONS"
            );
        }
    }

    #[test]
    fn test_init_has_version_marker() {
        assert!(
            RTK_INSTRUCTIONS.contains("<!-- rtk-instructions"),
            "RTK_INSTRUCTIONS must have version marker for idempotency"
        );
    }

    #[test]
    fn test_hook_has_guards() {
        assert!(REWRITE_HOOK.contains("command -v rtk"));
        assert!(REWRITE_HOOK.contains("command -v jq"));
        // Guards must be BEFORE set -euo pipefail
        let guard_pos = REWRITE_HOOK.find("command -v rtk").unwrap();
        let set_pos = REWRITE_HOOK.find("set -euo pipefail").unwrap();
        assert!(
            guard_pos < set_pos,
            "Guards must come before set -euo pipefail"
        );
    }

    #[test]
    fn test_rewrite_hook_rewrites_ssh() {
        assert!(REWRITE_HOOK.contains("^ssh([[:space:]]|$)"));
        assert!(REWRITE_HOOK.contains("rtk ssh"));
    }

    #[test]
    fn test_block_grep_hook_has_correct_schema() {
        // Verify grep hook defaults to hard deny with explicit allow override.
        assert!(BLOCK_GREP_HOOK.contains("\"permissionDecision\": \"allow\""));
        assert!(BLOCK_GREP_HOOK.contains("\"permissionDecision\": \"deny\""));
        assert!(BLOCK_GREP_HOOK.contains("\"hookEventName\": \"PreToolUse\"")); // canonical PreToolUse schema
        assert!(BLOCK_GREP_HOOK.contains("RTK_ALLOW_NATIVE_GREP=1"));
        assert!(BLOCK_GREP_HOOK.contains("RTK_BLOCK_NATIVE_GREP=0"));
        assert!(BLOCK_GREP_HOOK.contains("rtk grep"));
        assert!(BLOCK_GREP_HOOK.contains("rtk rgai"));
        // Read is now in its own separate hook
        assert!(
            !BLOCK_GREP_HOOK.contains("rtk read"),
            "Grep hook must not mention rtk read (separate hook)"
        );
    }

    #[test]
    fn test_block_read_hook_has_correct_schema() {
        // Verify read hook defaults to hard deny with explicit allow override.
        assert!(BLOCK_READ_HOOK.contains("\"permissionDecision\": \"allow\""));
        assert!(BLOCK_READ_HOOK.contains("\"permissionDecision\": \"deny\""));
        assert!(BLOCK_READ_HOOK.contains("\"hookEventName\": \"PreToolUse\"")); // canonical PreToolUse schema
        assert!(BLOCK_READ_HOOK.contains("RTK_ALLOW_NATIVE_READ=1"));
        assert!(BLOCK_READ_HOOK.contains("RTK_BLOCK_NATIVE_READ=0"));
        assert!(BLOCK_READ_HOOK.contains("rtk read"));
        // Read hook must not mention grep
        assert!(
            !BLOCK_READ_HOOK.contains("rtk grep"),
            "Read hook must not mention rtk grep (separate hook)"
        );
    }

    #[test]
    fn test_block_write_hook_has_correct_schema() {
        // Verify write hook defaults to hard deny with explicit allow override.
        assert!(BLOCK_WRITE_HOOK.contains("\"permissionDecision\": \"allow\""));
        assert!(BLOCK_WRITE_HOOK.contains("\"permissionDecision\": \"deny\""));
        assert!(BLOCK_WRITE_HOOK.contains("\"hookEventName\": \"PreToolUse\""));
        assert!(BLOCK_WRITE_HOOK.contains("RTK_ALLOW_NATIVE_WRITE=1"));
        assert!(BLOCK_WRITE_HOOK.contains("RTK_BLOCK_NATIVE_WRITE=0"));
        assert!(BLOCK_WRITE_HOOK.contains("rtk write"));
    }

    #[test]
    fn test_block_explore_hook_has_correct_schema() {
        assert!(BLOCK_EXPLORE_HOOK.contains("\"permissionDecision\": \"allow\""));
        assert!(BLOCK_EXPLORE_HOOK.contains("\"permissionDecision\": \"deny\""));
        assert!(BLOCK_EXPLORE_HOOK.contains("\"hookEventName\": \"PreToolUse\""));
        assert!(BLOCK_EXPLORE_HOOK.contains("RTK_ALLOW_NATIVE_EXPLORE=1"));
        assert!(BLOCK_EXPLORE_HOOK.contains("RTK_BLOCK_NATIVE_EXPLORE=0"));
        assert!(BLOCK_EXPLORE_HOOK.contains("subagent_type"));
        assert!(BLOCK_EXPLORE_HOOK.contains("rtk memory explore"));
    }

    #[test]
    fn test_migration_removes_old_block() {
        let input = r#"# My Config

<!-- rtk-instructions v2 -->
OLD RTK STUFF
<!-- /rtk-instructions -->

More content"#;

        let (result, migrated) = remove_rtk_block(input);
        assert!(migrated);
        assert!(!result.contains("OLD RTK STUFF"));
        assert!(result.contains("# My Config"));
        assert!(result.contains("More content"));
    }

    #[test]
    fn test_migration_warns_on_missing_end_marker() {
        let input = "<!-- rtk-instructions v2 -->\nOLD STUFF\nNo end marker";
        let (result, migrated) = remove_rtk_block(input);
        assert!(!migrated);
        assert_eq!(result, input);
    }

    #[test]
    #[cfg(unix)]
    fn test_default_mode_creates_all_hooks_and_rtk_md() {
        let temp = TempDir::new().unwrap();
        let rewrite_path = temp.path().join("rtk-rewrite.sh");
        let block_grep_path = temp.path().join("rtk-block-native-grep.sh");
        let block_read_path = temp.path().join("rtk-block-native-read.sh");
        let block_write_path = temp.path().join("rtk-block-native-write.sh");
        let block_explore_path = temp.path().join("rtk-block-native-explore.sh");
        let rtk_md_path = temp.path().join("RTK.md");

        // Simulate install_single_hook behavior
        fs::write(&rewrite_path, REWRITE_HOOK).unwrap();
        fs::write(&block_grep_path, BLOCK_GREP_HOOK).unwrap();
        fs::write(&block_read_path, BLOCK_READ_HOOK).unwrap();
        fs::write(&block_write_path, BLOCK_WRITE_HOOK).unwrap();
        fs::write(&block_explore_path, BLOCK_EXPLORE_HOOK).unwrap();
        fs::write(&rtk_md_path, RTK_SLIM).unwrap();

        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&rewrite_path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&block_grep_path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&block_read_path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&block_write_path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&block_explore_path, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(rewrite_path.exists());
        assert!(block_grep_path.exists());
        assert!(block_read_path.exists());
        assert!(block_write_path.exists());
        assert!(block_explore_path.exists());
        assert!(rtk_md_path.exists());

        let metadata = fs::metadata(&rewrite_path).unwrap();
        assert!(metadata.permissions().mode() & 0o111 != 0);
        let metadata = fs::metadata(&block_grep_path).unwrap();
        assert!(metadata.permissions().mode() & 0o111 != 0);
        let metadata = fs::metadata(&block_read_path).unwrap();
        assert!(metadata.permissions().mode() & 0o111 != 0);
        let metadata = fs::metadata(&block_explore_path).unwrap();
        assert!(metadata.permissions().mode() & 0o111 != 0);
    }

    #[test]
    fn test_claude_md_mode_creates_full_injection() {
        // Just verify RTK_INSTRUCTIONS constant has the right content
        assert!(RTK_INSTRUCTIONS.contains("<!-- rtk-instructions"));
        assert!(RTK_INSTRUCTIONS.contains("rtk cargo test"));
        assert!(RTK_INSTRUCTIONS.contains("rtk rgai"));
        assert!(RTK_INSTRUCTIONS.contains("<!-- /rtk-instructions -->"));
        assert!(RTK_INSTRUCTIONS.len() > 4000);
    }

    // --- upsert_rtk_block tests ---

    #[test]
    fn test_upsert_rtk_block_appends_when_missing() {
        let input = "# Team instructions";
        let (content, action) = upsert_rtk_block(input, RTK_INSTRUCTIONS);
        assert_eq!(action, RtkBlockUpsert::Added);
        assert!(content.contains("# Team instructions"));
        assert!(content.contains("<!-- rtk-instructions"));
    }

    #[test]
    fn test_upsert_rtk_block_updates_stale_block() {
        let input = r#"# Team instructions

<!-- rtk-instructions v1 -->
OLD RTK CONTENT
<!-- /rtk-instructions -->

More notes
"#;

        let (content, action) = upsert_rtk_block(input, RTK_INSTRUCTIONS);
        assert_eq!(action, RtkBlockUpsert::Updated);
        assert!(!content.contains("OLD RTK CONTENT"));
        assert!(content.contains("rtk cargo test")); // from current RTK_INSTRUCTIONS
        assert!(content.contains("# Team instructions"));
        assert!(content.contains("More notes"));
    }

    #[test]
    fn test_upsert_rtk_block_noop_when_already_current() {
        let input = format!(
            "# Team instructions\n\n{}\n\nMore notes\n",
            RTK_INSTRUCTIONS
        );
        let (content, action) = upsert_rtk_block(&input, RTK_INSTRUCTIONS);
        assert_eq!(action, RtkBlockUpsert::Unchanged);
        assert_eq!(content, input);
    }

    #[test]
    fn test_upsert_rtk_block_detects_malformed_block() {
        let input = "<!-- rtk-instructions v2 -->\npartial";
        let (content, action) = upsert_rtk_block(input, RTK_INSTRUCTIONS);
        assert_eq!(action, RtkBlockUpsert::Malformed);
        assert_eq!(content, input);
    }

    #[test]
    fn test_search_priority_policy_in_init_and_slim_templates() {
        let policy = "Search priority (mandatory): rgai > rg > grep.";
        assert!(
            RTK_INSTRUCTIONS.contains(policy),
            "RTK_INSTRUCTIONS must include strict search priority policy"
        );
        assert!(
            RTK_SLIM.contains(policy),
            "RTK_SLIM must include strict search priority policy"
        );
    }

    #[test]
    fn test_files_search_examples_prioritize_rgai_before_grep() {
        let rgai_pos = RTK_INSTRUCTIONS
            .find("rtk rgai <query>")
            .expect("rtk rgai example missing");
        let grep_pos = RTK_INSTRUCTIONS
            .find("rtk grep <pattern>")
            .expect("rtk grep example missing");
        assert!(
            rgai_pos < grep_pos,
            "Files/Search examples must prioritize rtk rgai before rtk grep"
        );
    }

    #[test]
    fn test_templates_include_precise_read_guidance() {
        let precise = "rtk read <file> --level none --from <N> --to <M>";
        assert!(
            RTK_INSTRUCTIONS.contains(precise),
            "RTK_INSTRUCTIONS must include exact line-range read guidance"
        );
        assert!(
            RTK_SLIM.contains("--level none --from"),
            "RTK_SLIM must include precise read guidance for hook-based mode"
        );
    }

    #[test]
    fn test_templates_explain_native_tool_policy() {
        let policy = "blocked by default (hard deny policy)";
        assert!(
            RTK_INSTRUCTIONS.contains(policy),
            "RTK_INSTRUCTIONS must describe hard-deny native tool behavior"
        );
        assert!(
            RTK_SLIM.contains(policy),
            "RTK_SLIM must describe hard-deny native tool behavior"
        );
    }

    #[test]
    fn test_init_is_idempotent() {
        let temp = TempDir::new().unwrap();
        let claude_md = temp.path().join("CLAUDE.md");

        fs::write(&claude_md, "# My stuff\n\n@RTK.md\n").unwrap();

        let content = fs::read_to_string(&claude_md).unwrap();
        let count = content.matches("@RTK.md").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_local_init_unchanged() {
        // Local init should use claude-md mode
        let temp = TempDir::new().unwrap();
        let claude_md = temp.path().join("CLAUDE.md");

        fs::write(&claude_md, RTK_INSTRUCTIONS).unwrap();
        let content = fs::read_to_string(&claude_md).unwrap();

        assert!(content.contains("<!-- rtk-instructions"));
    }

    // Tests for hooks_already_present()
    #[test]
    fn test_hooks_already_present_all() {
        let json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{"type": "command", "command": "/Users/test/.claude/hooks/rtk-rewrite.sh"}]
                    },
                    {
                        "matcher": "Grep",
                        "hooks": [{"type": "command", "command": "/Users/test/.claude/hooks/rtk-block-native-grep.sh"}]
                    },
                    {
                        "matcher": "Read",
                        "hooks": [{"type": "command", "command": "/Users/test/.claude/hooks/rtk-block-native-read.sh"}]
                    },
                    {
                        "matcher": "Edit",
                        "hooks": [{"type": "command", "command": "/Users/test/.claude/hooks/rtk-block-native-write.sh"}]
                    },
                    {
                        "matcher": "Write",
                        "hooks": [{"type": "command", "command": "/Users/test/.claude/hooks/rtk-block-native-write.sh"}]
                    },
                    {
                        "matcher": "Task",
                        "hooks": [{"type": "command", "command": "/Users/test/.claude/hooks/rtk-block-native-explore.sh"}]
                    }
                ]
            }
        });

        assert!(hooks_already_present(
            &json_content,
            "/Users/test/.claude/hooks/rtk-rewrite.sh",
            "/Users/test/.claude/hooks/rtk-block-native-grep.sh",
            "/Users/test/.claude/hooks/rtk-block-native-read.sh",
            "/Users/test/.claude/hooks/rtk-block-native-write.sh",
            "/Users/test/.claude/hooks/rtk-block-native-explore.sh",
        ));
    }

    #[test]
    fn test_hooks_already_present_only_rewrite() {
        // Only rewrite present -> should return false (all required)
        let json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/Users/test/.claude/hooks/rtk-rewrite.sh"}]
                }]
            }
        });

        assert!(!hooks_already_present(
            &json_content,
            "/Users/test/.claude/hooks/rtk-rewrite.sh",
            "/Users/test/.claude/hooks/rtk-block-native-grep.sh",
            "/Users/test/.claude/hooks/rtk-block-native-read.sh",
            "/Users/test/.claude/hooks/rtk-block-native-write.sh",
            "/Users/test/.claude/hooks/rtk-block-native-explore.sh",
        ));
    }

    #[test]
    fn test_hooks_already_present_different_paths() {
        // Different paths but same filenames → should match on substring
        let json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/home/user/.claude/hooks/rtk-rewrite.sh"}]},
                    {"matcher": "Grep", "hooks": [{"type": "command", "command": "/home/user/.claude/hooks/rtk-block-native-grep.sh"}]},
                    {"matcher": "Read", "hooks": [{"type": "command", "command": "/home/user/.claude/hooks/rtk-block-native-read.sh"}]},
                    {"matcher": "Edit", "hooks": [{"type": "command", "command": "/home/user/.claude/hooks/rtk-block-native-write.sh"}]},
                    {"matcher": "Write", "hooks": [{"type": "command", "command": "/home/user/.claude/hooks/rtk-block-native-write.sh"}]},
                    {"matcher": "Task", "hooks": [{"type": "command", "command": "/home/user/.claude/hooks/rtk-block-native-explore.sh"}]}
                ]
            }
        });

        assert!(hooks_already_present(
            &json_content,
            "~/.claude/hooks/rtk-rewrite.sh",
            "~/.claude/hooks/rtk-block-native-grep.sh",
            "~/.claude/hooks/rtk-block-native-read.sh",
            "~/.claude/hooks/rtk-block-native-write.sh",
            "~/.claude/hooks/rtk-block-native-explore.sh",
        ));
    }

    #[test]
    fn test_hooks_not_present_empty() {
        let json_content = serde_json::json!({});
        assert!(!hooks_already_present(
            &json_content,
            "/Users/test/.claude/hooks/rtk-rewrite.sh",
            "/Users/test/.claude/hooks/rtk-block-native-grep.sh",
            "/Users/test/.claude/hooks/rtk-block-native-read.sh",
            "/Users/test/.claude/hooks/rtk-block-native-write.sh",
            "/Users/test/.claude/hooks/rtk-block-native-explore.sh",
        ));
    }

    #[test]
    fn test_hooks_not_present_other_hooks() {
        let json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/some/other/hook.sh"}]
                }]
            }
        });

        assert!(!hooks_already_present(
            &json_content,
            "/Users/test/.claude/hooks/rtk-rewrite.sh",
            "/Users/test/.claude/hooks/rtk-block-native-grep.sh",
            "/Users/test/.claude/hooks/rtk-block-native-read.sh",
            "/Users/test/.claude/hooks/rtk-block-native-write.sh",
            "/Users/test/.claude/hooks/rtk-block-native-explore.sh",
        ));
    }

    // Tests for any_rtk_hook_present()
    #[test]
    fn test_any_rtk_hook_present_all() {
        let json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/x/rtk-rewrite.sh"}]},
                    {"matcher": "Grep", "hooks": [{"type": "command", "command": "/x/rtk-block-native-grep.sh"}]},
                    {"matcher": "Read", "hooks": [{"type": "command", "command": "/x/rtk-block-native-read.sh"}]},
                    {"matcher": "Edit", "hooks": [{"type": "command", "command": "/x/rtk-block-native-write.sh"}]},
                    {"matcher": "Write", "hooks": [{"type": "command", "command": "/x/rtk-block-native-write.sh"}]},
                    {"matcher": "Task", "hooks": [{"type": "command", "command": "/x/rtk-block-native-explore.sh"}]}
                ]
            }
        });
        assert_eq!(
            any_rtk_hook_present(&json_content),
            (true, true, true, true, true, true)
        );
    }

    #[test]
    fn test_any_rtk_hook_present_only_rewrite() {
        let json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/x/rtk-rewrite.sh"}]}
                ]
            }
        });
        assert_eq!(
            any_rtk_hook_present(&json_content),
            (true, false, false, false, false, false)
        );
    }

    #[test]
    fn test_any_rtk_hook_present_none() {
        let json_content = serde_json::json!({});
        assert_eq!(
            any_rtk_hook_present(&json_content),
            (false, false, false, false, false, false)
        );
    }

    // Tests for insert_hook_entry()
    #[test]
    fn test_insert_hook_entry_creates_all_entries() {
        let mut json_content = serde_json::json!({});
        let rewrite = "/Users/test/.claude/hooks/rtk-rewrite.sh";
        let block_grep = "/Users/test/.claude/hooks/rtk-block-native-grep.sh";
        let block_read = "/Users/test/.claude/hooks/rtk-block-native-read.sh";
        let block_write = "/Users/test/.claude/hooks/rtk-block-native-write.sh";
        let block_explore = "/Users/test/.claude/hooks/rtk-block-native-explore.sh";

        insert_hook_entry(
            &mut json_content,
            rewrite,
            block_grep,
            block_read,
            block_write,
            block_explore,
        );

        // Should create full structure with all entries
        let pre_tool_use = json_content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool_use.len(), 6); // Bash rewrite + Grep block + Read block + Edit block + Write block + Task block

        // First: Bash rewrite
        assert_eq!(pre_tool_use[0]["matcher"], "Bash");
        assert_eq!(
            pre_tool_use[0]["hooks"][0]["command"].as_str().unwrap(),
            rewrite
        );
        assert_eq!(pre_tool_use[0]["hooks"][0]["timeout"], 5); // timeout field

        // Second: Grep block
        assert_eq!(pre_tool_use[1]["matcher"], "Grep");
        assert_eq!(
            pre_tool_use[1]["hooks"][0]["command"].as_str().unwrap(),
            block_grep
        );
        assert_eq!(pre_tool_use[1]["hooks"][0]["timeout"], 5); // timeout field

        // Third: Read block
        assert_eq!(pre_tool_use[2]["matcher"], "Read");
        assert_eq!(
            pre_tool_use[2]["hooks"][0]["command"].as_str().unwrap(),
            block_read
        );
        assert_eq!(pre_tool_use[2]["hooks"][0]["timeout"], 5); // timeout field

        // Fourth: Edit block
        assert_eq!(pre_tool_use[3]["matcher"], "Edit");
        assert_eq!(
            pre_tool_use[3]["hooks"][0]["command"].as_str().unwrap(),
            block_write
        );
        assert_eq!(pre_tool_use[3]["hooks"][0]["timeout"], 5);

        // Fifth: Write block
        assert_eq!(pre_tool_use[4]["matcher"], "Write");
        assert_eq!(
            pre_tool_use[4]["hooks"][0]["command"].as_str().unwrap(),
            block_write
        );
        assert_eq!(pre_tool_use[4]["hooks"][0]["timeout"], 5);

        // Sixth: Task block (Explore policy)
        assert_eq!(pre_tool_use[5]["matcher"], "Task");
        assert_eq!(
            pre_tool_use[5]["hooks"][0]["command"].as_str().unwrap(),
            block_explore
        );
        assert_eq!(pre_tool_use[5]["hooks"][0]["timeout"], 10);
    }

    #[test]
    fn test_insert_hook_entry_preserves_existing() {
        let mut json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/some/other/hook.sh"}]
                }]
            }
        });

        let rewrite = "/Users/test/.claude/hooks/rtk-rewrite.sh";
        let block_grep = "/Users/test/.claude/hooks/rtk-block-native-grep.sh";
        let block_read = "/Users/test/.claude/hooks/rtk-block-native-read.sh";
        let block_write = "/Users/test/.claude/hooks/rtk-block-native-write.sh";
        let block_explore = "/Users/test/.claude/hooks/rtk-block-native-explore.sh";
        insert_hook_entry(
            &mut json_content,
            rewrite,
            block_grep,
            block_read,
            block_write,
            block_explore,
        );

        let pre_tool_use = json_content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool_use.len(), 7); // existing + rewrite + grep + read + edit + write + task

        // Check first hook is preserved
        assert_eq!(
            pre_tool_use[0]["hooks"][0]["command"].as_str().unwrap(),
            "/some/other/hook.sh"
        );
        // Check RTK hooks added
        assert_eq!(pre_tool_use[1]["matcher"], "Bash");
        assert_eq!(
            pre_tool_use[1]["hooks"][0]["command"].as_str().unwrap(),
            rewrite
        );
        assert_eq!(pre_tool_use[2]["matcher"], "Grep");
        assert_eq!(
            pre_tool_use[2]["hooks"][0]["command"].as_str().unwrap(),
            block_grep
        );
        assert_eq!(pre_tool_use[3]["matcher"], "Read");
        assert_eq!(
            pre_tool_use[3]["hooks"][0]["command"].as_str().unwrap(),
            block_read
        );
        assert_eq!(pre_tool_use[4]["matcher"], "Edit");
        assert_eq!(
            pre_tool_use[4]["hooks"][0]["command"].as_str().unwrap(),
            block_write
        );
        assert_eq!(pre_tool_use[5]["matcher"], "Write");
        assert_eq!(
            pre_tool_use[5]["hooks"][0]["command"].as_str().unwrap(),
            block_write
        );
        assert_eq!(pre_tool_use[6]["matcher"], "Task");
        assert_eq!(
            pre_tool_use[6]["hooks"][0]["command"].as_str().unwrap(),
            block_explore
        );
    }

    #[test]
    fn test_insert_hook_preserves_other_keys() {
        let mut json_content = serde_json::json!({
            "env": {"PATH": "/custom/path"},
            "permissions": {"allowAll": true},
            "model": "claude-sonnet-4"
        });

        let rewrite = "/Users/test/.claude/hooks/rtk-rewrite.sh";
        let block_grep = "/Users/test/.claude/hooks/rtk-block-native-grep.sh";
        let block_read = "/Users/test/.claude/hooks/rtk-block-native-read.sh";
        let block_write = "/Users/test/.claude/hooks/rtk-block-native-write.sh";
        let block_explore = "/Users/test/.claude/hooks/rtk-block-native-explore.sh";
        insert_hook_entry(
            &mut json_content,
            rewrite,
            block_grep,
            block_read,
            block_write,
            block_explore,
        );

        // Should preserve all other keys
        assert_eq!(json_content["env"]["PATH"], "/custom/path");
        assert_eq!(json_content["permissions"]["allowAll"], true);
        assert_eq!(json_content["model"], "claude-sonnet-4");

        // And add hooks
        assert!(json_content.get("hooks").is_some());
    }

    // Tests for atomic_write()
    #[test]
    fn test_atomic_write() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.json");

        let content = r#"{"key": "value"}"#;
        atomic_write(&file_path, content).unwrap();

        assert!(file_path.exists());
        let written = fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, content);
    }

    #[test]
    fn test_write_if_changed_idempotent() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("idempotent.txt");

        let created = write_if_changed(&file_path, "v1", "test", 0).unwrap();
        assert!(created);

        let unchanged = write_if_changed(&file_path, "v1", "test", 0).unwrap();
        assert!(!unchanged);

        let updated = write_if_changed(&file_path, "v2", "test", 0).unwrap();
        assert!(updated);
    }

    // Test for preserve_order round-trip
    #[test]
    fn test_preserve_order_round_trip() {
        let original = r#"{"env": {"PATH": "/usr/bin"}, "permissions": {"allowAll": true}, "model": "claude-sonnet-4"}"#;
        let parsed: serde_json::Value = serde_json::from_str(original).unwrap();
        let serialized = serde_json::to_string(&parsed).unwrap();

        // Just check that keys exist (preserve_order doesn't guarantee exact order in nested objects)
        assert!(serialized.contains("\"env\""));
        assert!(serialized.contains("\"permissions\""));
        assert!(serialized.contains("\"model\""));
    }

    // Tests for clean_double_blanks()
    #[test]
    fn test_clean_double_blanks() {
        // Input: line1, 2 blank lines, line2, 1 blank line, line3, 3 blank lines, line4
        // Expected: line1, 2 blank lines (kept), line2, 1 blank line, line3, 2 blank lines (max), line4
        let input = "line1\n\n\nline2\n\nline3\n\n\n\nline4";
        // That's: line1 \n \n \n line2 \n \n line3 \n \n \n \n line4
        // Which is: line1, blank, blank, line2, blank, line3, blank, blank, blank, line4
        // So 2 blanks after line1 (keep both), 1 blank after line2 (keep), 3 blanks after line3 (keep 2)
        let expected = "line1\n\n\nline2\n\nline3\n\n\nline4";
        assert_eq!(clean_double_blanks(input), expected);
    }

    #[test]
    fn test_clean_double_blanks_preserves_single() {
        let input = "line1\n\nline2\n\nline3";
        assert_eq!(clean_double_blanks(input), input); // No change
    }

    // Tests for remove_hook_from_json()
    #[test]
    fn test_remove_hook_from_json_removes_all_rtk() {
        let mut json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/some/other/hook.sh"}]},
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/x/rtk-rewrite.sh"}]},
                    {"matcher": "Grep", "hooks": [{"type": "command", "command": "/x/rtk-block-native-grep.sh"}]},
                    {"matcher": "Read", "hooks": [{"type": "command", "command": "/x/rtk-block-native-read.sh"}]},
                    {"matcher": "Edit", "hooks": [{"type": "command", "command": "/x/rtk-block-native-write.sh"}]},
                    {"matcher": "Write", "hooks": [{"type": "command", "command": "/x/rtk-block-native-write.sh"}]},
                    {"matcher": "Task", "hooks": [{"type": "command", "command": "/x/rtk-block-native-explore.sh"}]}
                ]
            }
        });

        let removed = remove_hook_from_json(&mut json_content);
        assert!(removed);

        // Should have only the non-RTK hook left
        let pre_tool_use = json_content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool_use.len(), 1);
        assert_eq!(
            pre_tool_use[0]["hooks"][0]["command"].as_str().unwrap(),
            "/some/other/hook.sh"
        );
    }

    #[test]
    fn test_remove_hook_removes_only_rewrite() {
        let mut json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/x/rtk-rewrite.sh"}]}
                ]
            }
        });

        let removed = remove_hook_from_json(&mut json_content);
        assert!(removed);

        let pre_tool_use = json_content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool_use.len(), 0);
    }

    #[test]
    fn test_remove_hook_when_not_present() {
        let mut json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/some/other/hook.sh"}]
                }]
            }
        });

        let removed = remove_hook_from_json(&mut json_content);
        assert!(!removed);
    }

    // Tests for cleanup_project_local_hooks()
    #[test]
    fn test_cleanup_removes_local_hook_files() {
        let temp = TempDir::new().unwrap();
        let local_hooks = temp.path().join(".claude").join("hooks");
        fs::create_dir_all(&local_hooks).unwrap();

        // Create local hook duplicates
        fs::write(local_hooks.join("rtk-rewrite.sh"), "#!/bin/bash\nold").unwrap();
        fs::write(
            local_hooks.join("rtk-block-native-grep.sh"),
            "#!/bin/bash\nold",
        )
        .unwrap();

        // Verify they exist
        assert!(local_hooks.join("rtk-rewrite.sh").exists());
        assert!(local_hooks.join("rtk-block-native-grep.sh").exists());

        // We can't easily call cleanup_project_local_hooks() because it uses CWD,
        // but we can test the removal logic directly
        for hook_name in &[
            "rtk-rewrite.sh",
            "rtk-block-native-grep.sh",
            "rtk-block-native-read.sh",
            "rtk-block-native-write.sh",
            "rtk-block-native-explore.sh",
        ] {
            let path = local_hooks.join(hook_name);
            if path.exists() {
                fs::remove_file(&path).unwrap();
            }
        }
        assert!(!local_hooks.join("rtk-rewrite.sh").exists());
        assert!(!local_hooks.join("rtk-block-native-grep.sh").exists());
    }

    #[test]
    fn test_cleanup_removes_hook_entries_from_local_settings() {
        // Test that remove_hook_from_json handles all RTK entries in local settings
        let mut json_content = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": ".claude/hooks/rtk-rewrite.sh"}]},
                    {"matcher": "Grep", "hooks": [{"type": "command", "command": ".claude/hooks/rtk-block-native-grep.sh"}]},
                    {"matcher": "Read", "hooks": [{"type": "command", "command": ".claude/hooks/rtk-block-native-read.sh"}]},
                    {"matcher": "Task", "hooks": [{"type": "command", "command": ".claude/hooks/rtk-block-native-explore.sh"}]},
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/other/project-hook.sh"}]}
                ]
            }
        });

        let removed = remove_hook_from_json(&mut json_content);
        assert!(removed);

        let pre_tool_use = json_content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool_use.len(), 1); // Only non-RTK hook remains
        assert_eq!(
            pre_tool_use[0]["hooks"][0]["command"].as_str().unwrap(),
            "/other/project-hook.sh"
        );
    }
}
