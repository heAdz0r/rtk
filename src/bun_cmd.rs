use crate::tracking;
use anyhow::{Context, Result};
use std::process::Command;

pub fn run(args: &[String], verbose: u8, skip_env: bool) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("bun requires arguments (e.g., `bun run <script>`)");
    }

    let timer = tracking::TimedExecution::start();

    let mut cmd = Command::new("bun");
    for arg in args {
        cmd.arg(arg);
    }

    if skip_env {
        cmd.env("SKIP_ENV_VALIDATION", "1");
    }

    if verbose > 0 {
        eprintln!("Running: bun {}", args.join(" "));
    }

    let output = cmd
        .output()
        .context("Failed to run bun. Is Bun installed?")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = format!("{}\n{}", stdout, stderr);

    let exit_code = output
        .status
        .code()
        .unwrap_or(if output.status.success() { 0 } else { 1 });
    let filtered = filter_bun_output(&raw);

    if let Some(hint) = crate::tee::tee_and_hint(&raw, "bun", exit_code) {
        println!("{}\n{}", filtered, hint);
    } else {
        println!("{}", filtered);
    }

    timer.track(
        &format!("bun {}", args.join(" ")),
        &format!("rtk bun {}", args.join(" ")),
        &raw,
        &filtered,
    );

    if !output.status.success() {
        std::process::exit(exit_code);
    }

    Ok(())
}

fn filter_bun_output(output: &str) -> String {
    let mut result = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Common Bun script-run boilerplate, not useful for diagnostics.
        if trimmed.starts_with("bun run v") || trimmed.starts_with("bun test v") {
            continue;
        }
        if trimmed.starts_with("$ ") {
            continue;
        }
        if trimmed.starts_with("Done in ") {
            continue;
        }

        result.push(line.to_string());
    }

    if result.is_empty() {
        "ok ✓".to_string()
    } else {
        result.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_bun_output_strips_boilerplate() {
        let output = r#"
bun run v1.1.42
$ vue-tsc --noEmit
src/main.ts:12:3 - error TS7006: Parameter 'x' implicitly has an 'any' type.
Done in 0.27s
"#;

        let result = filter_bun_output(output);
        assert!(!result.contains("bun run v"));
        assert!(!result.contains("$ vue-tsc --noEmit"));
        assert!(!result.contains("Done in"));
        assert!(result.contains("TS7006"));
    }

    #[test]
    fn test_filter_bun_output_keeps_version_output() {
        let output = "1.2.0\n";
        let result = filter_bun_output(output);
        assert_eq!(result, "1.2.0");
    }
}
