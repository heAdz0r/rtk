use lazy_static::lazy_static;
use regex::{Regex, RegexSet};

/// A rule mapping a shell command pattern to its RTK equivalent.
struct RtkRule {
    rtk_cmd: &'static str,
    category: &'static str,
    savings_pct: f64,
    subcmd_savings: &'static [(&'static str, f64)],
    subcmd_status: &'static [(&'static str, super::report::RtkStatus)],
}

/// Result of classifying a command.
#[derive(Debug, PartialEq)]
pub enum Classification {
    Supported {
        rtk_equivalent: &'static str,
        category: &'static str,
        estimated_savings_pct: f64,
        status: super::report::RtkStatus,
    },
    Unsupported {
        base_command: String,
    },
    Ignored,
}

/// Average token counts per category for estimation when no output_len available.
pub fn category_avg_tokens(category: &str, subcmd: &str) -> usize {
    match category {
        "Git" => match subcmd {
            "log" | "diff" | "show" => 200,
            _ => 40,
        },
        "Cargo" => match subcmd {
            "test" => 500,
            _ => 150,
        },
        "Tests" => 800,
        "Files" => 100,
        "Build" => 300,
        "Infra" => 120,
        "Network" => 150,
        "GitHub" => 200,
        "PackageManager" => 150,
        _ => 150,
    }
}

// Patterns ordered to match RTK_RULES indices exactly.
const PATTERNS: &[&str] = &[
    r"^git\s+(?:(?:-C|-c)\s+[^[:space:]]+\s+|--[a-z-]+(?:=[^[:space:]]+)?\s+)*(status|log|diff|show|add|commit|push|pull|branch|fetch|stash|worktree|checkout|cherry-pick|remote|merge-base|rev-parse|ls-files|merge|rebase|rm)(\s|$)",
    r"^gh\s+(pr|issue|run|repo|api)",
    r"^(?:[^[:space:]]+/)?cargo\s+(?:\+[^[:space:]]+\s+)?(build|test|clippy|check|fmt|install|nextest|run)(\s|$)",
    r"^(?:[^[:space:]]+/)?(grepai|rgai)\s+search(\s|$)",
    r"^(?:[^[:space:]]+/)?(rgai)\s+",
    r"^go\s+(test|build|vet)(\s|$)",
    r"^pnpm\s+(list|ls|outdated|install)",
    r"^npm\s+(run|exec)",
    r"^bun\s+([^\s]+)(\s|$)",
    r"^npx\s+",
    r"^(cat|head)\s+",
    r"^(rg|grep)\s+",
    r"^ls(\s|$)",
    r"^tree(\s|$)",
    r"^find\s+",
    r"^((npx|bunx)\s+)?vue-tsc(\s|$)",
    r"^(npx\s+|pnpm\s+)?tsc(\s|$)",
    r"^(npx\s+|pnpm\s+)?(eslint|biome|lint)(\s|$)",
    r"^(npx\s+|pnpm\s+)?prettier",
    r"^(npx\s+|pnpm\s+)?next\s+build",
    r"^(pnpm\s+|npx\s+)?(vitest|jest|test)(\s|$)",
    r"^(?:python3?\s+-m\s+)?(pytest)(\s|$)",
    r"^(?:[^[:space:]]+/)?pip\s+(list|outdated|install|show|uninstall)(\s|$)",
    r"^golangci-lint\s+(run)(\s|$)",
    r"^(npx\s+|pnpm\s+)?playwright",
    r"^(npx\s+|pnpm\s+)?prisma",
    r"^docker\s+(ps|images|logs)",
    r"^kubectl\s+(get|logs)",
    r"^curl\s+",
    r"^wget\s+",
    r"^ssh\s+", // added: ssh command detection for discover
];

const RULES: &[RtkRule] = &[
    RtkRule {
        rtk_cmd: "rtk git",
        category: "Git",
        savings_pct: 70.0,
        subcmd_savings: &[
            ("diff", 80.0),
            ("show", 80.0),
            ("add", 59.0),
            ("commit", 59.0),
            ("checkout", 0.0),
            ("cherry-pick", 0.0),
            ("remote", 0.0),
            ("merge-base", 0.0),
            ("rev-parse", 0.0),
            ("ls-files", 0.0),
            ("merge", 0.0),
            ("rebase", 0.0),
            ("rm", 0.0),
        ],
        subcmd_status: &[
            ("checkout", super::report::RtkStatus::Passthrough),
            ("cherry-pick", super::report::RtkStatus::Passthrough),
            ("remote", super::report::RtkStatus::Passthrough),
            ("merge-base", super::report::RtkStatus::Passthrough),
            ("rev-parse", super::report::RtkStatus::Passthrough),
            ("ls-files", super::report::RtkStatus::Passthrough),
            ("merge", super::report::RtkStatus::Passthrough),
            ("rebase", super::report::RtkStatus::Passthrough),
            ("rm", super::report::RtkStatus::Passthrough),
        ],
    },
    RtkRule {
        rtk_cmd: "rtk gh",
        category: "GitHub",
        savings_pct: 82.0,
        subcmd_savings: &[("pr", 87.0), ("run", 82.0), ("issue", 80.0)],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk cargo",
        category: "Cargo",
        savings_pct: 80.0,
        subcmd_savings: &[("test", 90.0), ("check", 80.0), ("run", 0.0)],
        subcmd_status: &[
            ("fmt", super::report::RtkStatus::Passthrough),
            ("run", super::report::RtkStatus::Passthrough),
        ],
    },
    RtkRule {
        rtk_cmd: "rtk rgai",
        category: "Files",
        savings_pct: 85.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk rgai",
        category: "Files",
        savings_pct: 85.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk go",
        category: "Build",
        savings_pct: 80.0,
        subcmd_savings: &[("test", 90.0)],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk pnpm",
        category: "PackageManager",
        savings_pct: 80.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk npm",
        category: "PackageManager",
        savings_pct: 70.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk bun",
        category: "PackageManager",
        savings_pct: 70.0,
        subcmd_savings: &[
            ("--version", 0.0),
            ("-v", 0.0),
            ("install", 0.0),
            ("add", 0.0),
            ("remove", 0.0),
            ("update", 0.0),
            ("upgrade", 0.0),
            ("pm", 0.0),
        ],
        subcmd_status: &[
            ("--version", super::report::RtkStatus::Passthrough),
            ("-v", super::report::RtkStatus::Passthrough),
            ("install", super::report::RtkStatus::Passthrough),
            ("add", super::report::RtkStatus::Passthrough),
            ("remove", super::report::RtkStatus::Passthrough),
            ("update", super::report::RtkStatus::Passthrough),
            ("upgrade", super::report::RtkStatus::Passthrough),
            ("pm", super::report::RtkStatus::Passthrough),
        ],
    },
    RtkRule {
        rtk_cmd: "rtk npx",
        category: "PackageManager",
        savings_pct: 70.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk read",
        category: "Files",
        savings_pct: 60.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk grep",
        category: "Files",
        savings_pct: 75.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk ls",
        category: "Files",
        savings_pct: 65.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk tree",
        category: "Files",
        savings_pct: 65.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk find",
        category: "Files",
        savings_pct: 70.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk tsc",
        category: "Build",
        savings_pct: 83.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk tsc",
        category: "Build",
        savings_pct: 83.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk lint",
        category: "Build",
        savings_pct: 84.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk prettier",
        category: "Build",
        savings_pct: 70.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk next",
        category: "Build",
        savings_pct: 87.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk vitest",
        category: "Tests",
        savings_pct: 99.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk pytest",
        category: "Tests",
        savings_pct: 90.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk pip",
        category: "PackageManager",
        savings_pct: 70.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk golangci-lint",
        category: "Build",
        savings_pct: 80.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk playwright",
        category: "Tests",
        savings_pct: 94.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk prisma",
        category: "Build",
        savings_pct: 88.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk docker",
        category: "Infra",
        savings_pct: 85.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk kubectl",
        category: "Infra",
        savings_pct: 85.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk curl",
        category: "Network",
        savings_pct: 70.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        rtk_cmd: "rtk wget",
        category: "Network",
        savings_pct: 65.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
    RtkRule {
        // added: ssh rule for discover
        rtk_cmd: "rtk ssh",
        category: "Network",
        savings_pct: 75.0,
        subcmd_savings: &[],
        subcmd_status: &[],
    },
];

/// Commands to ignore (shell builtins, trivial, already rtk).
const IGNORED_PREFIXES: &[&str] = &[
    "#",
    "cd ",
    "cd\t",
    "echo ",
    "printf ",
    "export ",
    "source ",
    "mkdir ",
    "rm ",
    "mv ",
    "cp ",
    "chmod ",
    "chown ",
    "touch ",
    "which ",
    "type ",
    "command ",
    "test ",
    "true",
    "false",
    "sleep ",
    "wait",
    "kill ",
    "set ",
    "unset ",
    "wc ",
    "sort ",
    "uniq ",
    "tr ",
    "cut ",
    "awk ",
    "sed ",
    "python3 -c",
    "python -c",
    "node -e",
    "ruby -e",
    "rtk ",
    "pwd",
    "bash ",
    "sh ",
    "then\n",
    "then ",
    "else\n",
    "else ",
    "fi",
    "do\n",
    "do ",
    "done",
    "for ",
    "while ",
    "if ",
    "case ",
];

const IGNORED_EXACT: &[&str] = &["cd", "echo", "true", "false", "wait", "pwd", "bash", "sh"];

lazy_static! {
    static ref REGEX_SET: RegexSet = RegexSet::new(PATTERNS).expect("invalid regex patterns");
    static ref COMPILED: Vec<Regex> = PATTERNS
        .iter()
        .map(|p| Regex::new(p).expect("invalid regex"))
        .collect();
    static ref ENV_PREFIX: Regex =
        Regex::new(r"^(?:sudo\s+|env\s+|[A-Z_][A-Z0-9_]*=[^\s]*\s+)+").unwrap();
}

/// Classify a single (already-split) command.
pub fn classify_command(cmd: &str) -> Classification {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return Classification::Ignored;
    }
    if trimmed.contains("<<") {
        return Classification::Ignored;
    }
    if trimmed.chars().all(|c| !c.is_alphanumeric()) {
        return Classification::Ignored;
    }
    if let Some(first_raw) = trimmed.split_whitespace().next() {
        if first_raw.starts_with("--")
            || first_raw.starts_with('"')
            || first_raw.starts_with('\'')
            || first_raw.starts_with('\\')
        {
            return Classification::Ignored;
        }
    }

    // Check ignored
    for exact in IGNORED_EXACT {
        if trimmed == *exact {
            return Classification::Ignored;
        }
    }
    for prefix in IGNORED_PREFIXES {
        if trimmed.starts_with(prefix) {
            return Classification::Ignored;
        }
    }

    // Strip env prefixes (sudo, env VAR=val, VAR=val)
    let stripped = ENV_PREFIX.replace(trimmed, "");
    let cmd_clean = stripped.trim();
    if cmd_clean.is_empty() {
        return Classification::Ignored;
    }
    if let Some(first) = cmd_clean.split_whitespace().next() {
        if matches!(
            first,
            "SELECT"
                | "INSERT"
                | "UPDATE"
                | "DELETE"
                | "CREATE"
                | "ALTER"
                | "DROP"
                | "PRAGMA"
                | "BEGIN"
                | "COMMIT"
                | "ROLLBACK"
        ) {
            return Classification::Ignored;
        }
    }

    // Fast check with RegexSet — take the last (most specific) match
    let matches: Vec<usize> = REGEX_SET.matches(cmd_clean).into_iter().collect();
    if let Some(&idx) = matches.last() {
        let rule = &RULES[idx];

        // Extract subcommand for savings override and status detection
        let (savings, status) = if let Some(caps) = COMPILED[idx].captures(cmd_clean) {
            if let Some(sub) = caps.get(1) {
                let subcmd = sub.as_str();
                // Check if this subcommand has a special status
                let status = rule
                    .subcmd_status
                    .iter()
                    .find(|(s, _)| *s == subcmd)
                    .map(|(_, st)| *st)
                    .unwrap_or(super::report::RtkStatus::Existing);

                // Check if this subcommand has custom savings
                let savings = rule
                    .subcmd_savings
                    .iter()
                    .find(|(s, _)| *s == subcmd)
                    .map(|(_, pct)| *pct)
                    .unwrap_or(rule.savings_pct);

                (savings, status)
            } else {
                (rule.savings_pct, super::report::RtkStatus::Existing)
            }
        } else {
            (rule.savings_pct, super::report::RtkStatus::Existing)
        };

        Classification::Supported {
            rtk_equivalent: rule.rtk_cmd,
            category: rule.category,
            estimated_savings_pct: savings,
            status,
        }
    } else {
        // Extract base command for unsupported
        let base = extract_base_command(cmd_clean);
        if base.is_empty() {
            Classification::Ignored
        } else {
            Classification::Unsupported {
                base_command: base.to_string(),
            }
        }
    }
}

/// Extract the base command (first word, or first two if it looks like a subcommand pattern).
fn extract_base_command(cmd: &str) -> &str {
    let parts: Vec<&str> = cmd.splitn(3, char::is_whitespace).collect();
    match parts.len() {
        0 => "",
        1 => parts[0],
        _ => {
            let second = parts[1];
            // If the second token looks like a subcommand (no leading -)
            if !second.starts_with('-') && !second.contains('/') && !second.contains('.') {
                // Return "cmd subcmd"
                let end = cmd
                    .find(char::is_whitespace)
                    .and_then(|i| {
                        let rest = &cmd[i..];
                        let trimmed = rest.trim_start();
                        trimmed
                            .find(char::is_whitespace)
                            .map(|j| i + (rest.len() - trimmed.len()) + j)
                    })
                    .unwrap_or(cmd.len());
                &cmd[..end]
            } else {
                parts[0]
            }
        }
    }
}

/// Split a command chain on `&&`, `||`, `;` outside quotes.
/// For pipes `|`, only keep the first command.
/// Lines with `<<` (heredoc) or `$((` are returned whole.
pub fn split_command_chain(cmd: &str) -> Vec<&str> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    // Heredoc or arithmetic expansion: treat as single command
    if trimmed.contains("<<") || trimmed.contains("$((") {
        return vec![trimmed];
    }

    let mut results = Vec::new();
    let mut start = 0;
    let bytes = trimmed.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut pipe_seen = false;

    while i < len {
        let b = bytes[i];
        match b {
            b'\'' if !in_double => {
                in_single = !in_single;
                i += 1;
            }
            b'"' if !in_single => {
                in_double = !in_double;
                i += 1;
            }
            b'|' if !in_single && !in_double => {
                if i + 1 < len && bytes[i + 1] == b'|' {
                    // ||
                    let segment = trimmed[start..i].trim();
                    if !segment.is_empty() {
                        results.push(segment);
                    }
                    i += 2;
                    start = i;
                } else {
                    // pipe: keep only first command
                    let segment = trimmed[start..i].trim();
                    if !segment.is_empty() {
                        results.push(segment);
                    }
                    pipe_seen = true;
                    break;
                }
            }
            b'&' if !in_single && !in_double && i + 1 < len && bytes[i + 1] == b'&' => {
                let segment = trimmed[start..i].trim();
                if !segment.is_empty() {
                    results.push(segment);
                }
                i += 2;
                start = i;
            }
            b';' if !in_single && !in_double => {
                let segment = trimmed[start..i].trim();
                if !segment.is_empty() {
                    results.push(segment);
                }
                i += 1;
                start = i;
            }
            _ => {
                i += 1;
            }
        }
    }

    if !pipe_seen && start < len {
        let segment = trimmed[start..].trim();
        if !segment.is_empty() {
            results.push(segment);
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::super::report::RtkStatus;
    use super::*;

    #[test]
    fn test_classify_git_status() {
        assert_eq!(
            classify_command("git status"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 70.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_git_diff_cached() {
        assert_eq!(
            classify_command("git diff --cached"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 80.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_git_with_global_options() {
        assert_eq!(
            classify_command("git -C /Users/andrew/Programming/rtk status -s"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 70.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_git_checkout_passthrough() {
        assert_eq!(
            classify_command("git checkout feat/gain-project-scope"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_git_cherry_pick_passthrough() {
        assert_eq!(
            classify_command("git cherry-pick 3d08e6c"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_git_remote_passthrough() {
        assert_eq!(
            classify_command("git remote -v"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_git_merge_base_passthrough() {
        assert_eq!(
            classify_command("git merge-base HEAD origin/master"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_git_rev_parse_passthrough() {
        assert_eq!(
            classify_command("git rev-parse HEAD"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_git_ls_files_passthrough() {
        assert_eq!(
            classify_command("git ls-files --cached"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_git_merge_passthrough() {
        assert_eq!(
            classify_command("git merge origin/master"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_git_rebase_passthrough() {
        assert_eq!(
            classify_command("git rebase upstream/master"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_git_rm_passthrough() {
        assert_eq!(
            classify_command("git rm --cached .grepai/config.yaml"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_cargo_test_filter() {
        assert_eq!(
            classify_command("cargo test filter::"),
            Classification::Supported {
                rtk_equivalent: "rtk cargo",
                category: "Cargo",
                estimated_savings_pct: 90.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_cargo_install() {
        assert_eq!(
            classify_command("cargo install --path ."),
            Classification::Supported {
                rtk_equivalent: "rtk cargo",
                category: "Cargo",
                estimated_savings_pct: 80.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_cargo_run_passthrough() {
        assert_eq!(
            classify_command("cargo run -- rgai --builtin \"token trace\""),
            Classification::Supported {
                rtk_equivalent: "rtk cargo",
                category: "Cargo",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_absolute_path_cargo_test() {
        assert_eq!(
            classify_command("/Users/andrew/.cargo/bin/cargo test -q"),
            Classification::Supported {
                rtk_equivalent: "rtk cargo",
                category: "Cargo",
                estimated_savings_pct: 90.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_npx_tsc() {
        assert_eq!(
            classify_command("npx tsc --noEmit"),
            Classification::Supported {
                rtk_equivalent: "rtk tsc",
                category: "Build",
                estimated_savings_pct: 83.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_bunx_vue_tsc() {
        assert_eq!(
            classify_command("bunx vue-tsc --noEmit"),
            Classification::Supported {
                rtk_equivalent: "rtk tsc",
                category: "Build",
                estimated_savings_pct: 83.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_bun_run() {
        assert_eq!(
            classify_command("bun run typecheck"),
            Classification::Supported {
                rtk_equivalent: "rtk bun",
                category: "PackageManager",
                estimated_savings_pct: 70.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_bun_version_passthrough() {
        assert_eq!(
            classify_command("bun --version"),
            Classification::Supported {
                rtk_equivalent: "rtk bun",
                category: "PackageManager",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_bun_direct_script() {
        assert_eq!(
            classify_command("bun packages/server/src/index.ts"),
            Classification::Supported {
                rtk_equivalent: "rtk bun",
                category: "PackageManager",
                estimated_savings_pct: 70.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_bun_install_passthrough() {
        assert_eq!(
            classify_command("bun install"),
            Classification::Supported {
                rtk_equivalent: "rtk bun",
                category: "PackageManager",
                estimated_savings_pct: 0.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_python3_pytest() {
        assert_eq!(
            classify_command("python3 -m pytest benchmarks/tests/test_baseline.py"),
            Classification::Supported {
                rtk_equivalent: "rtk pytest",
                category: "Tests",
                estimated_savings_pct: 90.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_pip_absolute_path() {
        assert_eq!(
            classify_command("/Users/andrew/anaconda3/bin/pip install requests"),
            Classification::Supported {
                rtk_equivalent: "rtk pip",
                category: "PackageManager",
                estimated_savings_pct: 70.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_grepai_search() {
        assert_eq!(
            classify_command("grepai search \"SharedDefaults App Group\""),
            Classification::Supported {
                rtk_equivalent: "rtk rgai",
                category: "Files",
                estimated_savings_pct: 85.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_grepai_search_absolute_path() {
        assert_eq!(
            classify_command("/Users/andrew/.local/bin/grepai search \"token trace\""),
            Classification::Supported {
                rtk_equivalent: "rtk rgai",
                category: "Files",
                estimated_savings_pct: 85.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_rgai_direct() {
        assert_eq!(
            classify_command("rgai token traces"),
            Classification::Supported {
                rtk_equivalent: "rtk rgai",
                category: "Files",
                estimated_savings_pct: 85.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_go_build() {
        assert_eq!(
            classify_command("go build ./..."),
            Classification::Supported {
                rtk_equivalent: "rtk go",
                category: "Build",
                estimated_savings_pct: 80.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_go_test_savings() {
        assert_eq!(
            classify_command("go test ./..."),
            Classification::Supported {
                rtk_equivalent: "rtk go",
                category: "Build",
                estimated_savings_pct: 90.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_golangci_lint_run() {
        assert_eq!(
            classify_command("golangci-lint run ./..."),
            Classification::Supported {
                rtk_equivalent: "rtk golangci-lint",
                category: "Build",
                estimated_savings_pct: 80.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_cat_file() {
        assert_eq!(
            classify_command("cat src/main.rs"),
            Classification::Supported {
                rtk_equivalent: "rtk read",
                category: "Files",
                estimated_savings_pct: 60.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_head_file() {
        assert_eq!(
            classify_command("head -20 src/main.rs"),
            Classification::Supported {
                rtk_equivalent: "rtk read",
                category: "Files",
                estimated_savings_pct: 60.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_tail_file_unsupported() {
        assert_eq!(
            classify_command("tail -20 src/main.rs"),
            Classification::Unsupported {
                base_command: "tail".to_string(),
            }
        );
    }

    #[test]
    fn test_classify_tree_supported() {
        assert_eq!(
            classify_command("tree src/"),
            Classification::Supported {
                rtk_equivalent: "rtk tree",
                category: "Files",
                estimated_savings_pct: 65.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_cd_ignored() {
        assert_eq!(classify_command("cd /tmp"), Classification::Ignored);
    }

    #[test]
    fn test_classify_rtk_already() {
        assert_eq!(classify_command("rtk git status"), Classification::Ignored);
    }

    #[test]
    fn test_classify_echo_ignored() {
        assert_eq!(
            classify_command("echo hello world"),
            Classification::Ignored
        );
    }

    #[test]
    fn test_classify_comment_ignored() {
        assert_eq!(
            classify_command("# Test hook rewrite for ssh commands"),
            Classification::Ignored
        );
    }

    #[test]
    fn test_classify_dash_dash_noise_ignored() {
        assert_eq!(
            classify_command("-- Индексы для promo_code_user"),
            Classification::Ignored
        );
    }

    #[test]
    fn test_classify_quote_noise_ignored() {
        assert_eq!(classify_command("\"'"), Classification::Ignored);
    }

    #[test]
    fn test_classify_sql_ignored() {
        assert_eq!(
            classify_command("CREATE INDEX IF NOT EXISTS idx_leaderboard_entries"),
            Classification::Ignored
        );
    }

    #[test]
    fn test_classify_quoted_noise_with_redirection_ignored() {
        assert_eq!(classify_command("\\\"\\\" 2>&1"), Classification::Ignored);
    }

    #[test]
    fn test_classify_heredoc_ignored() {
        assert_eq!(
            classify_command("python3 << 'PYEOF'"),
            Classification::Ignored
        );
    }

    #[test]
    fn test_classify_terraform_unsupported() {
        match classify_command("terraform plan -var-file=prod.tfvars") {
            Classification::Unsupported { base_command } => {
                assert_eq!(base_command, "terraform plan");
            }
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_env_prefix_stripped() {
        assert_eq!(
            classify_command("GIT_SSH_COMMAND=ssh git push"),
            Classification::Supported {
                rtk_equivalent: "rtk git",
                category: "Git",
                estimated_savings_pct: 70.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_sudo_stripped() {
        assert_eq!(
            classify_command("sudo docker ps"),
            Classification::Supported {
                rtk_equivalent: "rtk docker",
                category: "Infra",
                estimated_savings_pct: 85.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_cargo_check() {
        assert_eq!(
            classify_command("cargo check"),
            Classification::Supported {
                rtk_equivalent: "rtk cargo",
                category: "Cargo",
                estimated_savings_pct: 80.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_cargo_check_all_targets() {
        assert_eq!(
            classify_command("cargo check --all-targets"),
            Classification::Supported {
                rtk_equivalent: "rtk cargo",
                category: "Cargo",
                estimated_savings_pct: 80.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_classify_cargo_fmt_passthrough() {
        assert_eq!(
            classify_command("cargo fmt"),
            Classification::Supported {
                rtk_equivalent: "rtk cargo",
                category: "Cargo",
                estimated_savings_pct: 80.0,
                status: RtkStatus::Passthrough,
            }
        );
    }

    #[test]
    fn test_classify_cargo_clippy_savings() {
        assert_eq!(
            classify_command("cargo clippy --all-targets"),
            Classification::Supported {
                rtk_equivalent: "rtk cargo",
                category: "Cargo",
                estimated_savings_pct: 80.0,
                status: RtkStatus::Existing,
            }
        );
    }

    #[test]
    fn test_patterns_rules_length_match() {
        assert_eq!(
            PATTERNS.len(),
            RULES.len(),
            "PATTERNS and RULES must be aligned"
        );
    }

    #[test]
    fn test_registry_covers_all_cargo_subcommands() {
        // Verify that every CargoCommand variant with a dedicated handler
        // except Other has a matching pattern in the registry
        for subcmd in [
            "build", "test", "clippy", "check", "fmt", "install", "nextest",
        ] {
            let cmd = format!("cargo {subcmd}");
            match classify_command(&cmd) {
                Classification::Supported { .. } => {}
                other => panic!("cargo {subcmd} should be Supported, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_registry_covers_all_git_subcommands() {
        // Verify that every GitCommand subcommand has a matching pattern
        for subcmd in [
            "status",
            "log",
            "diff",
            "show",
            "add",
            "commit",
            "push",
            "pull",
            "branch",
            "fetch",
            "stash",
            "worktree",
            "checkout",
            "cherry-pick",
            "remote",
            "merge-base",
            "rev-parse",
            "ls-files",
            "merge",
            "rebase",
            "rm",
        ] {
            let cmd = format!("git {subcmd}");
            match classify_command(&cmd) {
                Classification::Supported { .. } => {}
                other => panic!("git {subcmd} should be Supported, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_registry_covers_all_go_subcommands() {
        for subcmd in ["test", "build", "vet"] {
            let cmd = format!("go {subcmd}");
            match classify_command(&cmd) {
                Classification::Supported { .. } => {}
                other => panic!("go {subcmd} should be Supported, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_split_chain_and() {
        assert_eq!(split_command_chain("a && b"), vec!["a", "b"]);
    }

    #[test]
    fn test_split_chain_semicolon() {
        assert_eq!(split_command_chain("a ; b"), vec!["a", "b"]);
    }

    #[test]
    fn test_split_pipe_first_only() {
        assert_eq!(split_command_chain("a | b"), vec!["a"]);
    }

    #[test]
    fn test_split_single() {
        assert_eq!(split_command_chain("git status"), vec!["git status"]);
    }

    #[test]
    fn test_split_quoted_and() {
        assert_eq!(
            split_command_chain(r#"echo "a && b""#),
            vec![r#"echo "a && b""#]
        );
    }

    #[test]
    fn test_split_heredoc_no_split() {
        let cmd = "cat <<'EOF'\nhello && world\nEOF";
        assert_eq!(split_command_chain(cmd), vec![cmd]);
    }
}
