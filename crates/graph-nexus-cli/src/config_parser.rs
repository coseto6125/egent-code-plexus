use graph_nexus_analyzer::resolution::path_aliases::PathAliases;
use std::path::Path;

pub fn parse_configs(repo_path: &Path) -> PathAliases {
    let mut aliases = PathAliases::new();

    // 1. tsconfig.json
    if let Ok(content) = std::fs::read_to_string(repo_path.join("tsconfig.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(paths) = json
                .pointer("/compilerOptions/paths")
                .and_then(|v| v.as_object())
            {
                for (pattern, targets) in paths {
                    if let Some(target_arr) = targets.as_array() {
                        let mut replacements = Vec::new();
                        for t in target_arr {
                            if let Some(t_str) = t.as_str() {
                                replacements.push(t_str.to_string());
                            }
                        }
                        if !replacements.is_empty() {
                            aliases.add(pattern, replacements);
                        }
                    }
                }
            }
        }
    }

    // 2. package.json ("imports" subpath patterns)
    if let Ok(content) = std::fs::read_to_string(repo_path.join("package.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(imports) = json.get("imports").and_then(|v| v.as_object()) {
                for (pattern, target) in imports {
                    if let Some(t_str) = target.as_str() {
                        aliases.add(pattern, vec![t_str.to_string()]);
                    }
                }
            }
        }
    }

    // 3. go.mod
    if let Ok(content) = std::fs::read_to_string(repo_path.join("go.mod")) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("module ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let mod_name = parts[1];
                    let pattern = format!("{}/*", mod_name);
                    aliases.add(&pattern, vec!["./*".to_string()]);
                }
                break;
            }
        }
    }

    // 4. Cargo.toml
    if let Ok(content) = std::fs::read_to_string(repo_path.join("Cargo.toml")) {
        let mut in_package = false;
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("[package]") {
                in_package = true;
            } else if line.starts_with('[') {
                in_package = false;
            } else if in_package && line.starts_with("name") {
                let parts: Vec<&str> = line.split('=').collect();
                if parts.len() >= 2 {
                    let name = parts[1].trim().trim_matches('"').trim_matches('\'').trim();
                    let pattern = format!("{}/*", name);
                    aliases.add(&pattern, vec!["src/*".to_string()]);
                    aliases.add(
                        name,
                        vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
                    );
                }
            }
        }
    }

    // 5. pyproject.toml
    if let Ok(content) = std::fs::read_to_string(repo_path.join("pyproject.toml")) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("name") {
                let parts: Vec<&str> = line.split('=').collect();
                if parts.len() >= 2 {
                    let name = parts[1].trim().trim_matches('"').trim_matches('\'').trim();
                    let pattern = format!("{}/*", name);
                    aliases.add(
                        &pattern,
                        vec![format!("src/{}/*", name), format!("{}/*", name)],
                    );
                }
            }
        }
    }

    aliases
}
