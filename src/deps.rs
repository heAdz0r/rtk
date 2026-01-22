use anyhow::Result;
use std::fs;
use std::path::Path;
use regex::Regex;

/// Summarize project dependencies
pub fn run(path: &Path, verbose: u8) -> Result<()> {
    let dir = if path.is_file() {
        path.parent().unwrap_or(Path::new("."))
    } else {
        path
    };

    if verbose > 0 {
        eprintln!("Scanning dependencies in: {}", dir.display());
    }

    let mut found = false;

    // Rust - Cargo.toml
    let cargo_path = dir.join("Cargo.toml");
    if cargo_path.exists() {
        found = true;
        println!("📦 Rust (Cargo.toml):");
        summarize_cargo(&cargo_path)?;
    }

    // Node.js - package.json
    let package_path = dir.join("package.json");
    if package_path.exists() {
        found = true;
        println!("📦 Node.js (package.json):");
        summarize_package_json(&package_path)?;
    }

    // Python - requirements.txt
    let requirements_path = dir.join("requirements.txt");
    if requirements_path.exists() {
        found = true;
        println!("📦 Python (requirements.txt):");
        summarize_requirements(&requirements_path)?;
    }

    // Python - pyproject.toml
    let pyproject_path = dir.join("pyproject.toml");
    if pyproject_path.exists() {
        found = true;
        println!("📦 Python (pyproject.toml):");
        summarize_pyproject(&pyproject_path)?;
    }

    // Go - go.mod
    let gomod_path = dir.join("go.mod");
    if gomod_path.exists() {
        found = true;
        println!("📦 Go (go.mod):");
        summarize_gomod(&gomod_path)?;
    }

    if !found {
        println!("No dependency files found in {}", dir.display());
    }

    Ok(())
}

fn summarize_cargo(path: &Path) -> Result<()> {
    let content = fs::read_to_string(path)?;

    let dep_re = Regex::new(r#"^([a-zA-Z0-9_-]+)\s*=\s*(?:"([^"]+)"|.*version\s*=\s*"([^"]+)")"#).unwrap();
    let section_re = Regex::new(r"^\[([^\]]+)\]").unwrap();

    let mut current_section = String::new();
    let mut deps = Vec::new();
    let mut dev_deps = Vec::new();
    let mut build_deps = Vec::new();

    for line in content.lines() {
        if let Some(caps) = section_re.captures(line) {
            current_section = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        } else if let Some(caps) = dep_re.captures(line) {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let version = caps.get(2).or(caps.get(3)).map(|m| m.as_str()).unwrap_or("*");

            let dep = format!("{} ({})", name, version);
            match current_section.as_str() {
                "dependencies" => deps.push(dep),
                "dev-dependencies" => dev_deps.push(dep),
                "build-dependencies" => build_deps.push(dep),
                _ => {}
            }
        }
    }

    if !deps.is_empty() {
        println!("  Dependencies ({}):", deps.len());
        for d in deps.iter().take(10) {
            println!("    {}", d);
        }
        if deps.len() > 10 {
            println!("    ... +{} more", deps.len() - 10);
        }
    }

    if !dev_deps.is_empty() {
        println!("  Dev ({}):", dev_deps.len());
        for d in dev_deps.iter().take(5) {
            println!("    {}", d);
        }
        if dev_deps.len() > 5 {
            println!("    ... +{} more", dev_deps.len() - 5);
        }
    }

    Ok(())
}

fn summarize_package_json(path: &Path) -> Result<()> {
    let content = fs::read_to_string(path)?;
    let json: serde_json::Value = serde_json::from_str(&content)?;

    if let Some(name) = json.get("name").and_then(|v| v.as_str()) {
        let version = json.get("version").and_then(|v| v.as_str()).unwrap_or("?");
        println!("  {} @ {}", name, version);
    }

    if let Some(deps) = json.get("dependencies").and_then(|v| v.as_object()) {
        println!("  Dependencies ({}):", deps.len());
        for (i, (name, version)) in deps.iter().enumerate() {
            if i >= 10 {
                println!("    ... +{} more", deps.len() - 10);
                break;
            }
            let v = version.as_str().unwrap_or("*");
            println!("    {} ({})", name, v);
        }
    }

    if let Some(dev_deps) = json.get("devDependencies").and_then(|v| v.as_object()) {
        println!("  Dev Dependencies ({}):", dev_deps.len());
        for (i, (name, _)) in dev_deps.iter().enumerate() {
            if i >= 5 {
                println!("    ... +{} more", dev_deps.len() - 5);
                break;
            }
            println!("    {}", name);
        }
    }

    Ok(())
}

fn summarize_requirements(path: &Path) -> Result<()> {
    let content = fs::read_to_string(path)?;

    let dep_re = Regex::new(r"^([a-zA-Z0-9_-]+)([=<>!~]+.*)?$").unwrap();
    let mut deps = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(caps) = dep_re.captures(line) {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let version = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            deps.push(format!("{}{}", name, version));
        }
    }

    println!("  Packages ({}):", deps.len());
    for d in deps.iter().take(15) {
        println!("    {}", d);
    }
    if deps.len() > 15 {
        println!("    ... +{} more", deps.len() - 15);
    }

    Ok(())
}

fn summarize_pyproject(path: &Path) -> Result<()> {
    let content = fs::read_to_string(path)?;

    // Simple extraction - look for dependencies section
    let mut in_deps = false;
    let mut deps = Vec::new();

    for line in content.lines() {
        if line.contains("dependencies") && line.contains("[") {
            in_deps = true;
            continue;
        }
        if in_deps {
            if line.trim() == "]" {
                break;
            }
            let line = line.trim().trim_matches(|c| c == '"' || c == '\'' || c == ',');
            if !line.is_empty() {
                deps.push(line.to_string());
            }
        }
    }

    if !deps.is_empty() {
        println!("  Dependencies ({}):", deps.len());
        for d in deps.iter().take(10) {
            println!("    {}", d);
        }
        if deps.len() > 10 {
            println!("    ... +{} more", deps.len() - 10);
        }
    }

    Ok(())
}

fn summarize_gomod(path: &Path) -> Result<()> {
    let content = fs::read_to_string(path)?;

    let mut module_name = String::new();
    let mut go_version = String::new();
    let mut deps = Vec::new();

    let mut in_require = false;

    for line in content.lines() {
        let line = line.trim();

        if line.starts_with("module ") {
            module_name = line.trim_start_matches("module ").to_string();
        } else if line.starts_with("go ") {
            go_version = line.trim_start_matches("go ").to_string();
        } else if line == "require (" {
            in_require = true;
        } else if line == ")" {
            in_require = false;
        } else if in_require && !line.starts_with("//") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                deps.push(format!("{} {}", parts[0], parts[1]));
            }
        } else if line.starts_with("require ") && !line.contains("(") {
            let rest = line.trim_start_matches("require ");
            deps.push(rest.to_string());
        }
    }

    if !module_name.is_empty() {
        println!("  {} (go {})", module_name, go_version);
    }

    if !deps.is_empty() {
        println!("  Dependencies ({}):", deps.len());
        for d in deps.iter().take(10) {
            println!("    {}", d);
        }
        if deps.len() > 10 {
            println!("    ... +{} more", deps.len() - 10);
        }
    }

    Ok(())
}
