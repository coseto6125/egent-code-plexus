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

    // 6. C# toolchain: *.csproj files
    for meta in parse_csproj_files(repo_path) {
        // Register each ProjectReference as a path alias so the resolver can
        // follow cross-project symbol references within the same solution.
        for proj_ref in &meta.project_references {
            // proj_ref is a relative path like "../OtherLib/OtherLib.csproj"
            // Strip the .csproj suffix and use the directory as the alias target.
            let ref_path = Path::new(proj_ref);
            let dir = ref_path
                .parent()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if !dir.is_empty() {
                let stem = ref_path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !stem.is_empty() {
                    aliases.add(&stem, vec![format!("{}/*", dir)]);
                }
            }
        }
    }

    aliases
}

// ─── C# Config ──────────────────────────────────────────────────────────────

/// Parsed metadata from a single `.csproj` file (Task F1).
///
/// The `kind` discriminator is always `"csproj"`.  The other fields reflect
/// the most useful elements extracted from `<PropertyGroup>` and `<ItemGroup>`.
#[derive(Debug, Clone, PartialEq)]
pub struct CsprojMeta {
    /// Always `"csproj"`.
    pub kind: &'static str,
    /// Relative path of the `.csproj` file within the repo (forward-slash).
    pub file_path: String,
    /// Value of `<TargetFramework>` or `<TargetFrameworks>`, if present.
    pub target_framework: Option<String>,
    /// Assembly name from `<AssemblyName>`, falling back to the file stem.
    pub assembly_name: Option<String>,
    /// NuGet package references: `(package_id, version)`.
    pub package_references: Vec<(String, String)>,
    /// Relative paths of `<ProjectReference Include="...">` entries.
    pub project_references: Vec<String>,
}

/// Parsed metadata from `global.json` (Task F1).
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalJsonMeta {
    /// Always `"global-json"`.
    pub kind: &'static str,
    /// The `sdk.version` field, e.g. `"8.0.100"`.
    pub sdk_version: Option<String>,
}

/// Parsed metadata from `NuGet.config` (Task F1).
#[derive(Debug, Clone, PartialEq)]
pub struct NugetConfigMeta {
    /// Always `"nuget-config"`.
    pub kind: &'static str,
    /// Feed entries: `(key, url)`.
    pub package_sources: Vec<(String, String)>,
}

/// Scan `repo_path` for all `*.csproj` files (up to 2 directory levels deep)
/// and parse each one.
pub fn parse_csproj_files(repo_path: &Path) -> Vec<CsprojMeta> {
    let mut results = Vec::new();
    collect_csproj(repo_path, repo_path, 0, &mut results);
    results
}

fn collect_csproj(root: &Path, dir: &Path, depth: u8, out: &mut Vec<CsprojMeta>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < 2 {
            collect_csproj(root, &path, depth + 1, out);
        } else if path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("csproj"))
            .unwrap_or(false)
        {
            if let Some(meta) = parse_single_csproj(&path, root) {
                out.push(meta);
            }
        }
    }
}

fn parse_single_csproj(path: &Path, repo_root: &Path) -> Option<CsprojMeta> {
    let content = std::fs::read_to_string(path).ok()?;
    let rel_path = path
        .strip_prefix(repo_root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"));

    let assembly_name = xml_first_text(&content, "AssemblyName").or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
    });

    let target_framework = xml_first_text(&content, "TargetFramework")
        .or_else(|| xml_first_text(&content, "TargetFrameworks"));

    let package_references = xml_attrs_pairs(&content, "PackageReference", "Include", "Version");
    let project_references = xml_attrs_single(&content, "ProjectReference", "Include");

    Some(CsprojMeta {
        kind: "csproj",
        file_path: rel_path,
        target_framework,
        assembly_name,
        package_references,
        project_references,
    })
}

/// Parse `global.json` from `repo_path`.  Returns `None` if absent or malformed.
pub fn parse_global_json(repo_path: &Path) -> Option<GlobalJsonMeta> {
    let content = std::fs::read_to_string(repo_path.join("global.json")).ok()?;
    let json = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    let sdk_version = json
        .pointer("/sdk/version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(GlobalJsonMeta {
        kind: "global-json",
        sdk_version,
    })
}

/// Parse `NuGet.config` from `repo_path`.  Returns `None` if absent.
pub fn parse_nuget_config(repo_path: &Path) -> Option<NugetConfigMeta> {
    let content = std::fs::read_to_string(repo_path.join("NuGet.config"))
        .or_else(|_| std::fs::read_to_string(repo_path.join("nuget.config")))
        .ok()?;

    let package_sources = xml_attrs_pairs(&content, "add", "key", "value");
    Some(NugetConfigMeta {
        kind: "nuget-config",
        package_sources,
    })
}

// ─── Minimal XML helpers (no external parser needed) ────────────────────────

/// Extract the inner text of the first `<TagName>…</TagName>` occurrence.
fn xml_first_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let text = xml[start..end].trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// For every element matching `<ElemName … AttrA="…" AttrB="…" …>` return
/// `(value_of_AttrA, value_of_AttrB)` pairs.  Attribute order in source
/// doesn't matter; we scan for each name independently.
fn xml_attrs_pairs(
    xml: &str,
    elem: &str,
    attr_a: &str,
    attr_b: &str,
) -> Vec<(String, String)> {
    let open_tag = format!("<{}", elem);
    let mut results = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find(&open_tag) {
        rest = &rest[pos + open_tag.len()..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag_body = &rest[..end];
        if let (Some(a), Some(b)) = (
            xml_attr_value(tag_body, attr_a),
            xml_attr_value(tag_body, attr_b),
        ) {
            results.push((a, b));
        }
        rest = &rest[end.min(rest.len())..];
    }
    results
}

/// Like [`xml_attrs_pairs`] but only extracts a single attribute value per element.
fn xml_attrs_single(xml: &str, elem: &str, attr: &str) -> Vec<String> {
    let open_tag = format!("<{}", elem);
    let mut results = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find(&open_tag) {
        rest = &rest[pos + open_tag.len()..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag_body = &rest[..end];
        if let Some(val) = xml_attr_value(tag_body, attr) {
            results.push(val);
        }
        rest = &rest[end.min(rest.len())..];
    }
    results
}

/// Extract the value of `attr_name="…"` or `attr_name='…'` from a tag body
/// (the text between `<ElemName` and `>`).
fn xml_attr_value(tag_body: &str, attr_name: &str) -> Option<String> {
    let key_eq = format!("{}=", attr_name);
    let pos = tag_body.find(&key_eq)? + key_eq.len();
    let rest = &tag_body[pos..];
    let (quote, content_start) = if rest.starts_with('"') {
        ('"', 1)
    } else if rest.starts_with('\'') {
        ('\'', 1)
    } else {
        return None;
    };
    let end = rest[content_start..].find(quote)? + content_start;
    let value = rest[content_start..end].trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── Task F1: C# Config ────────────────────────────────────────────────

    #[test]
    fn csproj_extracts_target_framework_and_packages() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("MyApp.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <AssemblyName>MyApp</AssemblyName>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
    <PackageReference Include="Serilog" Version="3.0.0" />
    <ProjectReference Include="../Shared/Shared.csproj" />
  </ItemGroup>
</Project>"#,
        )
        .unwrap();

        let metas = parse_csproj_files(dir.path());
        assert_eq!(metas.len(), 1, "should find exactly one .csproj");
        let meta = &metas[0];

        assert_eq!(meta.kind, "csproj");
        assert_eq!(meta.target_framework.as_deref(), Some("net8.0"));
        assert_eq!(meta.assembly_name.as_deref(), Some("MyApp"));
        assert_eq!(
            meta.package_references,
            vec![
                ("Newtonsoft.Json".to_string(), "13.0.1".to_string()),
                ("Serilog".to_string(), "3.0.0".to_string()),
            ]
        );
        assert_eq!(
            meta.project_references,
            vec!["../Shared/Shared.csproj".to_string()]
        );
    }

    #[test]
    fn global_json_extracts_sdk_version() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("global.json"),
            r#"{ "sdk": { "version": "8.0.100", "rollForward": "latestMinor" } }"#,
        )
        .unwrap();

        let meta = parse_global_json(dir.path()).expect("global.json should parse");
        assert_eq!(meta.kind, "global-json");
        assert_eq!(meta.sdk_version.as_deref(), Some("8.0.100"));
    }

    #[test]
    fn nuget_config_extracts_package_sources() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("NuGet.config"),
            r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
    <add key="myget" value="https://www.myget.org/F/myfeed/api/v3/index.json" />
  </packageSources>
</configuration>"#,
        )
        .unwrap();

        let meta = parse_nuget_config(dir.path()).expect("NuGet.config should parse");
        assert_eq!(meta.kind, "nuget-config");
        assert_eq!(
            meta.package_sources,
            vec![
                (
                    "nuget.org".to_string(),
                    "https://api.nuget.org/v3/index.json".to_string()
                ),
                (
                    "myget".to_string(),
                    "https://www.myget.org/F/myfeed/api/v3/index.json".to_string()
                ),
            ]
        );
    }

    #[test]
    fn csproj_project_refs_added_to_path_aliases() {
        let dir = TempDir::new().unwrap();
        let shared_dir = dir.path().join("Shared");
        fs::create_dir_all(&shared_dir).unwrap();
        // Dummy referenced project
        fs::write(
            shared_dir.join("Shared.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup>
</Project>"#,
        )
        .unwrap();
        // Main project referencing Shared
        fs::write(
            dir.path().join("App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <ProjectReference Include="Shared/Shared.csproj" />
  </ItemGroup>
</Project>"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut found = Vec::new();
        aliases.expand("Shared", |c| {
            found.push(c.to_string());
            true
        });
        assert!(
            !found.is_empty(),
            "ProjectReference should register a path alias for `Shared`"
        );
    }
}
