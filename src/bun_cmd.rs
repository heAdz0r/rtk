use crate::tracking;
use anyhow::{Context, Result};
use regex::Regex;
use std::process::Command;
use std::sync::OnceLock;

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
    let rendered = render_bun_output(args, &raw);

    if let Some(hint) = crate::tee::tee_and_hint(&raw, "bun", exit_code) {
        println!("{}\n{}", rendered, hint);
    } else {
        println!("{}", rendered);
    }

    timer.track(
        &format!("bun {}", args.join(" ")),
        &format!("rtk bun {}", args.join(" ")),
        &raw,
        &rendered,
    );

    if !output.status.success() {
        std::process::exit(exit_code);
    }

    Ok(())
}

fn render_bun_output(args: &[String], output: &str) -> String {
    // Version/short commands should stay as-is.
    if matches!(args.first().map(|s| s.as_str()), Some("--version" | "-v")) {
        return filter_bun_output(output);
    }

    let filtered = filter_bun_output(output);
    let is_noisy = filtered.lines().count() > 80
        || output.contains("Test Files")
        || output.contains("vite v")
        || output.contains("modules transformed")
        || output.contains("vite-plugin-compression");

    if is_noisy {
        summarize_bun_output(args, output)
    } else {
        filtered
    }
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

fn summarize_bun_output(args: &[String], output: &str) -> String {
    static TEST_FILES_RE: OnceLock<Regex> = OnceLock::new();
    static TESTS_RE: OnceLock<Regex> = OnceLock::new();
    static MODULES_RE: OnceLock<Regex> = OnceLock::new();
    static BUILT_RE: OnceLock<Regex> = OnceLock::new();

    let test_files_re = TEST_FILES_RE.get_or_init(|| {
        Regex::new(r"^\s*Test Files\s+(\d+)\s+passed(?:\s+\((\d+)\))?")
            .expect("invalid TEST_FILES_RE")
    });
    let tests_re = TESTS_RE.get_or_init(|| {
        Regex::new(r"^\s*Tests\s+(\d+)\s+passed(?:\s+\((\d+)\))?").expect("invalid TESTS_RE")
    });
    let modules_re = MODULES_RE
        .get_or_init(|| Regex::new(r"^\s*✓\s+(\d+)\s+modules transformed\.").expect("invalid"));
    let built_re =
        BUILT_RE.get_or_init(|| Regex::new(r"^\s*✓\s+built in ([0-9.]+s)").expect("invalid"));

    let mut test_files: Option<(usize, usize)> = None;
    let mut tests: Option<(usize, usize)> = None;
    let mut modules: Option<usize> = None;
    let mut build_duration: Option<String> = None;

    for line in output.lines() {
        if let Some(caps) = test_files_re.captures(line) {
            let passed = caps
                .get(1)
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(0);
            let total = caps
                .get(2)
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(passed);
            test_files = Some((passed, total));
            continue;
        }
        if let Some(caps) = tests_re.captures(line) {
            let passed = caps
                .get(1)
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(0);
            let total = caps
                .get(2)
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(passed);
            tests = Some((passed, total));
            continue;
        }
        if let Some(caps) = modules_re.captures(line) {
            modules = caps.get(1).and_then(|m| m.as_str().parse::<usize>().ok());
            continue;
        }
        if let Some(caps) = built_re.captures(line) {
            build_duration = caps.get(1).map(|m| m.as_str().to_string());
            continue;
        }
    }

    let mut parts = Vec::new();
    if let Some((passed, total)) = tests {
        parts.push(format!("tests: {passed}/{total}"));
    }
    if let Some((passed, total)) = test_files {
        parts.push(format!("files: {passed}/{total}"));
    }
    if let Some(m) = modules {
        parts.push(format!("modules: {m}"));
    }
    if let Some(d) = build_duration {
        parts.push(format!("build: {d}"));
    }

    let headline = if parts.is_empty() {
        format!("✓ bun {}", args.join(" "))
    } else {
        format!("✓ bun {} ({})", args.join(" "), parts.join(", "))
    };

    let diag = crate::diag_summary::analyze_output(output);
    format!(
        "{}\n{}\n{}",
        headline,
        diag.warnings_line(),
        diag.errors_line()
    )
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

    #[test]
    fn test_summarize_bun_build_compact() {
        let output = r#"
bun run v1.2.0
$ vitest run && vite build
Test Files  1 passed (1)
Tests  10 passed (10)
✓ 2748 modules transformed.
✓ built in 4.88s
dist/assets/index.js 147.72 kB
"#;
        let args = vec!["run".to_string(), "build".to_string()];
        let result = summarize_bun_output(&args, output);
        assert!(result.contains("✓ bun run build"));
        assert!(result.contains("tests: 10/10"));
        assert!(result.contains("files: 1/1"));
        assert!(result.contains("modules: 2748"));
        assert!(result.contains("build: 4.88s"));
        assert!(result.contains("warnings: 0"));
        assert!(result.contains("errors: 0"));
        assert!(!result.contains("dist/assets/index.js"));
    }
}
