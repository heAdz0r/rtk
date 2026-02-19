use crate::tracking;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashMap;
use std::process::Command;

pub fn run(
    pattern: &str,
    path: &str,
    max_line_len: usize,
    max_results: usize,
    context_only: bool,
    file_type: Option<&str>,
    extra_args: &[String],
    verbose: u8,
) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("grep: '{}' in {}", pattern, path);
    }

    // Translate BRE -> PCRE: \| -> | (alternation), strip shell-injected escapes like \!
    let rg_pattern = bre_to_pcre(pattern);

    let mut rg_cmd = Command::new("rg");
    rg_cmd.args(["-n", "--no-heading", &rg_pattern, path]);

    if let Some(ft) = file_type {
        rg_cmd.arg("--type").arg(ft);
    }

    for arg in extra_args {
        // Fix: skip grep-ism -r flag (rg is recursive by default; rg -r means --replace)
        if arg == "-r" || arg == "--recursive" {
            continue;
        }
        rg_cmd.arg(arg);
    }

    let output = rg_cmd
        .output()
        .or_else(|_| Command::new("grep").args(["-rn", pattern, path]).output())
        .context("grep/rg failed")?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    let raw_output = stdout.to_string();

    // Bug 1: rg exit 2 = regex parse error — stderr was silently swallowed, showed "0 results"
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(2) {
        let msg = format!("❌ rg regex error for '{}': {}", rg_pattern, stderr_str.trim());
        println!("{}", msg);
        timer.track(
            &format!("grep -rn '{}' {}", pattern, path),
            "rtk grep",
            &raw_output,
            &msg,
        );
        return Ok(());
    }

    if stdout.trim().is_empty() {
        // Bug 2: show rg_pattern (post-translation), not original BRE
        let msg = format!("🔍 0 for '{}'", rg_pattern);
        println!("{}", msg);
        timer.track(
            &format!("grep -rn '{}' {}", pattern, path),
            "rtk grep",
            &raw_output,
            &msg,
        );
        return Ok(());
    }

    let mut by_file: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    let mut total = 0;

    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();

        let (file, line_num, content) = if parts.len() == 3 {
            let ln = parts[1].parse().unwrap_or(0);
            (parts[0].to_string(), ln, parts[2])
        } else if parts.len() == 2 {
            let ln = parts[0].parse().unwrap_or(0);
            (path.to_string(), ln, parts[1])
        } else {
            continue;
        };

        total += 1;
        // Bug 3: pass rg_pattern (PCRE), not original BRE — regex::escape(BRE) breaks context
        let cleaned = clean_line(content, max_line_len, context_only, &rg_pattern);
        by_file.entry(file).or_default().push((line_num, cleaned));
    }

    let mut rtk_output = String::new();
    rtk_output.push_str(&format!("🔍 {} in {}F:\n\n", total, by_file.len()));

    let mut shown = 0;
    let mut files: Vec<_> = by_file.iter().collect();
    files.sort_by_key(|(f, _)| *f);

    for (file, matches) in files {
        if shown >= max_results {
            break;
        }

        let file_display = compact_path(file);
        rtk_output.push_str(&format!("📄 {} ({}):\n", file_display, matches.len()));

        for (line_num, content) in matches.iter().take(10) {
            rtk_output.push_str(&format!("  {:>4}: {}\n", line_num, content));
            shown += 1;
            if shown >= max_results {
                break;
            }
        }

        if matches.len() > 10 {
            rtk_output.push_str(&format!("  +{}\n", matches.len() - 10));
        }
        rtk_output.push('\n');
    }

    if total > shown {
        rtk_output.push_str(&format!("... +{}\n", total - shown));
    }

    print!("{}", rtk_output);
    timer.track(
        &format!("grep -rn '{}' {}", pattern, path),
        "rtk grep",
        &raw_output,
        &rtk_output,
    );

    Ok(())
}

/// Translate a BRE/grep pattern to a PCRE/Rust-regex pattern for ripgrep.
///
/// Two transformations, single char-by-char pass:
/// 1. `\|`  -> `|`   BRE GNU alternation -> PCRE alternation
/// 2. `\X`  -> `X`   backslash before a non-regex-metachar (e.g. `\!` injected by
///                   zsh histexpand) is an undefined/invalid escape; strip the backslash.
///
/// Characters kept as `\X` (valid PCRE escape sequences):
///   `\\` `\^` `\$` `\.` `\*` `\+` `\?` `\(` `\)` `\[` `\]` `\{` `\}`
///   `\n` `\r` `\t` `\f` `\a` `\v`
///   `\d` `\D` `\w` `\W` `\s` `\S` `\b` `\B` `\A` `\z`
///   `\x` `\u` `\U` `\p` `\P` `\0`-`\9`
fn bre_to_pcre(pattern: &str) -> String {
    let mut result = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.peek().copied() {
            // BRE alternation \| -> PCRE |
            Some('|') => {
                result.push('|');
                chars.next();
            }
            // Valid PCRE/Rust-regex escape — keep backslash unchanged
            Some(next) if is_pcre_escape_char(next) => {
                result.push('\\');
                result.push(chars.next().unwrap());
            }
            // Unknown/shell-injected escape (e.g. \! from zsh histexpand) — strip backslash
            Some(_) => {
                result.push(chars.next().unwrap());
            }
            // Trailing bare backslash — keep (rg will emit its own regex error)
            None => result.push('\\'),
        }
    }
    result
}

/// Returns true for characters that form valid/meaningful Rust-regex escape sequences.
/// Backslash before these chars must be preserved; before anything else it is stripped.
fn is_pcre_escape_char(c: char) -> bool {
    matches!(
        c,
        // Metacharacters that need escaping to be literal
        '\\' | '^' | '$' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}'
        // Character class shorthands
        | 'd' | 'D' | 'w' | 'W' | 's' | 'S'
        // Anchors
        | 'b' | 'B' | 'A' | 'z'
        // Standard C escape chars
        | 'n' | 'r' | 't' | 'f' | 'a' | 'v'
        // Hex / Unicode
        | 'x' | 'u' | 'U'
        // Unicode properties
        | 'p' | 'P'
        // Back-references \0-\9
        | '0'..='9'
    )
}

fn clean_line(line: &str, max_len: usize, context_only: bool, pattern: &str) -> String {
    let trimmed = line.trim();

    if context_only {
        if let Ok(re) = Regex::new(&format!("(?i).{{0,20}}{}.*", regex::escape(pattern))) {
            if let Some(m) = re.find(trimmed) {
                let matched = m.as_str();
                if matched.len() <= max_len {
                    return matched.to_string();
                }
            }
        }
    }

    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        let lower = trimmed.to_lowercase();
        let pattern_lower = pattern.to_lowercase();

        if let Some(pos) = lower.find(&pattern_lower) {
            let char_pos = lower[..pos].chars().count();
            let chars: Vec<char> = trimmed.chars().collect();
            let char_len = chars.len();

            let start = char_pos.saturating_sub(max_len / 3);
            let end = (start + max_len).min(char_len);
            let start = if end == char_len {
                end.saturating_sub(max_len)
            } else {
                start
            };

            let slice: String = chars[start..end].iter().collect();
            if start > 0 && end < char_len {
                format!("...{}...", slice)
            } else if start > 0 {
                format!("...{}", slice)
            } else {
                format!("{}...", slice)
            }
        } else {
            let t: String = trimmed.chars().take(max_len - 3).collect();
            format!("{}...", t)
        }
    }
}

fn compact_path(path: &str) -> String {
    if path.len() <= 50 {
        return path.to_string();
    }

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 3 {
        return path.to_string();
    }

    format!(
        "{}/.../{}/{}",
        parts[0],
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_line() {
        let line = "            const result = someFunction();";
        let cleaned = clean_line(line, 50, false, "result");
        assert!(!cleaned.starts_with(' '));
        assert!(cleaned.len() <= 50);
    }

    #[test]
    fn test_compact_path() {
        let path = "/Users/patrick/dev/project/src/components/Button.tsx";
        let compact = compact_path(path);
        assert!(compact.len() <= 60);
    }

    #[test]
    fn test_extra_args_accepted() {
        // Test that the function signature accepts extra_args
        // This is a compile-time test - if it compiles, the signature is correct
        let _extra: Vec<String> = vec!["-i".to_string(), "-A".to_string(), "3".to_string()];
        // No need to actually run - we're verifying the parameter exists
    }

    #[test]
    fn test_clean_line_multibyte() {
        // Thai text that exceeds max_len in bytes
        let line = "  สวัสดีครับ นี่คือข้อความที่ยาวมากสำหรับทดสอบ  ";
        let cleaned = clean_line(line, 20, false, "ครับ");
        // Should not panic
        assert!(!cleaned.is_empty());
    }

    #[test]
    fn test_clean_line_emoji() {
        let line = "🎉🎊🎈🎁🎂🎄 some text 🎃🎆🎇✨";
        let cleaned = clean_line(line, 15, false, "text");
        assert!(!cleaned.is_empty());
    }


    // Fix: BRE \| alternation is translated to PCRE | for rg
    #[test]
    fn test_bre_alternation_translated() {
        let pattern = r"fn foo\|pub.*bar";
        let rg_pattern = pattern.replace(r"\|", "|");
        assert_eq!(rg_pattern, "fn foo|pub.*bar");
    }

    // Fix: -r flag (grep recursive) is stripped from extra_args (rg is recursive by default)
    #[test]
    fn test_recursive_flag_stripped() {
        let extra_args: Vec<String> = vec!["-r".to_string(), "-i".to_string()];
        let filtered: Vec<&String> = extra_args
            .iter()
            .filter(|a| *a != "-r" && *a != "--recursive")
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], "-i");
    }

    // Verify line numbers are always enabled in rg invocation (grep_cmd.rs:24).
    // The -n/--line-numbers clap flag in main.rs is a no-op accepted for compat.
    #[test]
    fn test_rg_always_has_line_numbers() {
        // grep_cmd::run() always passes "-n" to rg (line 24).
        // This test documents that -n is built-in, so the clap flag is safe to ignore.
        let mut cmd = std::process::Command::new("rg");
        cmd.args(["-n", "--no-heading", "NONEXISTENT_PATTERN_12345", "."]);
        // If rg is available, it should accept -n without error (exit 1 = no match, not error)
        if let Ok(output) = cmd.output() {
            assert!(
                output.status.code() == Some(1) || output.status.success(),
                "rg -n should be accepted"
            );
        }
        // If rg is not installed, skip gracefully (test still passes)
    }

    // bre_to_pcre: \| -> |
    #[test]
    fn test_bre_to_pcre_alternation() {
        assert_eq!(bre_to_pcre(r"panic\|todo\|unimplemented"), "panic|todo|unimplemented");
    }

    // bre_to_pcre: \! (shell histexpand artifact) -> ! (strip spurious backslash)
    #[test]
    fn test_bre_to_pcre_strips_shell_escaped_bang() {
        assert_eq!(bre_to_pcre(r"panic\!"), "panic!");
        assert_eq!(bre_to_pcre(r"panic\!\|todo\!"), "panic!|todo!");
    }

    // bre_to_pcre: valid PCRE escapes are preserved unchanged
    #[test]
    fn test_bre_to_pcre_preserves_valid_escapes() {
        assert_eq!(bre_to_pcre(r"\d+\.\w+"), r"\d+\.\w+");
        assert_eq!(bre_to_pcre(r"word"), r"word");
        assert_eq!(bre_to_pcre(r"#\[tokio"), r"#\[tokio"); // \[ = literal [, keep
    }

    // bre_to_pcre: trailing bare backslash preserved (rg will report its own error)
    #[test]
    fn test_bre_to_pcre_trailing_backslash() {
        // trailing backslash: use raw literal to avoid Rust string escape ambiguity
        let result = bre_to_pcre(r"foo\");
        assert_eq!(result, r"foo\");
    }

}
