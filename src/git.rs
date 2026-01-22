use anyhow::{Context, Result};
use std::process::Command;

#[derive(Debug, Clone, Copy)]
pub enum GitCommand {
    Diff,
    Log,
    Status,
}

pub fn run(cmd: GitCommand, args: &[String], max_lines: Option<usize>, verbose: u8) -> Result<()> {
    match cmd {
        GitCommand::Diff => run_diff(args, max_lines, verbose),
        GitCommand::Log => run_log(args, max_lines, verbose),
        GitCommand::Status => run_status(verbose),
    }
}

fn run_diff(args: &[String], max_lines: Option<usize>, verbose: u8) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("diff").arg("--stat");

    for arg in args {
        cmd.arg(arg);
    }

    let output = cmd.output().context("Failed to run git diff")?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    if verbose > 0 {
        eprintln!("Git diff summary:");
    }

    // Print stat summary first
    println!("{}", stdout.trim());

    // Now get actual diff but compact it
    let mut diff_cmd = Command::new("git");
    diff_cmd.arg("diff");
    for arg in args {
        diff_cmd.arg(arg);
    }

    let diff_output = diff_cmd.output().context("Failed to run git diff")?;
    let diff_stdout = String::from_utf8_lossy(&diff_output.stdout);

    if !diff_stdout.is_empty() {
        println!("\n--- Changes ---");
        let compacted = compact_diff(&diff_stdout, max_lines.unwrap_or(100));
        println!("{}", compacted);
    }

    Ok(())
}

fn compact_diff(diff: &str, max_lines: usize) -> String {
    let mut result = Vec::new();
    let mut current_file = String::new();
    let mut added = 0;
    let mut removed = 0;
    let mut in_hunk = false;
    let mut hunk_lines = 0;
    let max_hunk_lines = 10;

    for line in diff.lines() {
        if line.starts_with("diff --git") {
            // New file
            if !current_file.is_empty() && (added > 0 || removed > 0) {
                result.push(format!("  +{} -{}", added, removed));
            }
            current_file = line
                .split(" b/")
                .nth(1)
                .unwrap_or("unknown")
                .to_string();
            result.push(format!("\n📄 {}", current_file));
            added = 0;
            removed = 0;
            in_hunk = false;
        } else if line.starts_with("@@") {
            // New hunk
            in_hunk = true;
            hunk_lines = 0;
            let hunk_info = line.split("@@").nth(1).unwrap_or("").trim();
            result.push(format!("  @@ {} @@", hunk_info));
        } else if in_hunk {
            if line.starts_with('+') && !line.starts_with("+++") {
                added += 1;
                if hunk_lines < max_hunk_lines {
                    result.push(format!("  {}", line));
                    hunk_lines += 1;
                }
            } else if line.starts_with('-') && !line.starts_with("---") {
                removed += 1;
                if hunk_lines < max_hunk_lines {
                    result.push(format!("  {}", line));
                    hunk_lines += 1;
                }
            } else if hunk_lines < max_hunk_lines && !line.starts_with("\\") {
                // Context line
                if hunk_lines > 0 {
                    result.push(format!("  {}", line));
                    hunk_lines += 1;
                }
            }

            if hunk_lines == max_hunk_lines {
                result.push("  ... (truncated)".to_string());
                hunk_lines += 1;
            }
        }

        if result.len() >= max_lines {
            result.push("\n... (more changes truncated)".to_string());
            break;
        }
    }

    if !current_file.is_empty() && (added > 0 || removed > 0) {
        result.push(format!("  +{} -{}", added, removed));
    }

    result.join("\n")
}

fn run_log(args: &[String], max_lines: Option<usize>, verbose: u8) -> Result<()> {
    let limit = max_lines.unwrap_or(10);

    let mut cmd = Command::new("git");
    cmd.args([
        "log",
        &format!("-{}", limit),
        "--pretty=format:%h %s (%ar) <%an>",
        "--no-merges",
    ]);

    for arg in args {
        cmd.arg(arg);
    }

    let output = cmd.output().context("Failed to run git log")?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    if verbose > 0 {
        eprintln!("Last {} commits:", limit);
    }

    for line in stdout.lines().take(limit) {
        println!("{}", line);
    }

    Ok(())
}

fn run_status(_verbose: u8) -> Result<()> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "-b"])
        .output()
        .context("Failed to run git status")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    if lines.is_empty() {
        println!("Clean working tree");
        return Ok(());
    }

    // Parse branch info
    if let Some(branch_line) = lines.first() {
        if branch_line.starts_with("##") {
            let branch = branch_line.trim_start_matches("## ");
            println!("📌 {}", branch);
        }
    }

    // Count changes by type
    let mut staged = 0;
    let mut modified = 0;
    let mut untracked = 0;
    let mut conflicts = 0;

    let mut staged_files = Vec::new();
    let mut modified_files = Vec::new();
    let mut untracked_files = Vec::new();

    for line in lines.iter().skip(1) {
        if line.len() < 3 {
            continue;
        }
        let status = &line[0..2];
        let file = &line[3..];

        match status.chars().next().unwrap_or(' ') {
            'M' | 'A' | 'D' | 'R' | 'C' => {
                staged += 1;
                staged_files.push(file);
            }
            'U' => conflicts += 1,
            _ => {}
        }

        match status.chars().nth(1).unwrap_or(' ') {
            'M' | 'D' => {
                modified += 1;
                modified_files.push(file);
            }
            _ => {}
        }

        if status == "??" {
            untracked += 1;
            untracked_files.push(file);
        }
    }

    // Print summary
    if staged > 0 {
        println!("✅ Staged: {} files", staged);
        for f in staged_files.iter().take(5) {
            println!("   {}", f);
        }
        if staged_files.len() > 5 {
            println!("   ... +{} more", staged_files.len() - 5);
        }
    }

    if modified > 0 {
        println!("📝 Modified: {} files", modified);
        for f in modified_files.iter().take(5) {
            println!("   {}", f);
        }
        if modified_files.len() > 5 {
            println!("   ... +{} more", modified_files.len() - 5);
        }
    }

    if untracked > 0 {
        println!("❓ Untracked: {} files", untracked);
        for f in untracked_files.iter().take(3) {
            println!("   {}", f);
        }
        if untracked_files.len() > 3 {
            println!("   ... +{} more", untracked_files.len() - 3);
        }
    }

    if conflicts > 0 {
        println!("⚠️  Conflicts: {} files", conflicts);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_diff() {
        let diff = r#"diff --git a/foo.rs b/foo.rs
--- a/foo.rs
+++ b/foo.rs
@@ -1,3 +1,4 @@
 fn main() {
+    println!("hello");
 }
"#;
        let result = compact_diff(diff, 100);
        assert!(result.contains("foo.rs"));
        assert!(result.contains("+"));
    }
}
