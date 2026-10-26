// Task F1 (global.json / NuGet.config), F2 (composer.json), F3 (Package.swift)
// 的 parser 已寫好但尚未串到 main pipeline；保留 forward-looking 結構以利之後
// wave 串接。在 file 級先把 dead_code 改為 allow，等實際接上後再收掉。
#![allow(dead_code)]

use ecp_analyzer::c_sharp::partial::CsharpProjects;
use ecp_analyzer::resolution::path_aliases::PathAliases;
use ecp_core::contracts::workspace::{
    ConfigPresence, ConfigRead, Ecosystem, Membership, NameRank, Requirement, RequirementTarget,
    Scope, ScopeAlias, ScopeDraft, WorkspaceFacts,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn parse_configs(repo_path: &Path) -> PathAliases {
    parse_repo_configs(repo_path, &[]).aliases
}

/// What one read of the repo's config files gives the index build.
#[derive(Debug, Default)]
pub struct RepoConfigs {
    /// The repo-wide path aliases ([`parse_configs`]): root configs only,
    /// plus every `.csproj` `ProjectReference`.
    pub aliases: PathAliases,
    /// The C# projects the `.csproj` scan found, which the builder reads to
    /// merge the parts of a `partial` type.
    pub csharp_projects: CsharpProjects,
    /// The C2 scope rows of every ecosystem except Cargo, whose rows the
    /// Rust module tree builds from its own manifest parse.
    pub workspace: WorkspaceFacts,
}

/// Read the repo's config files once. `config_paths` are the repo-relative
/// config files the index walk found ([`is_workspace_config`]); the
/// `.csproj` files come from this function's own scan, which the C# rows
/// and the aliases share.
pub fn parse_repo_configs(repo_path: &Path, config_paths: &[String]) -> RepoConfigs {
    let mut aliases = root_aliases(repo_path);
    // The walk reads `.csproj` files down to this entry depth, so every
    // directory one level less deep was read whole.
    let mut csharp_projects =
        CsharpProjects::new(usize::from(resolve_csproj_max_depth()).saturating_sub(1));

    // 11. C# toolchain: *.csproj files
    let csproj_metas = parse_csproj_files(repo_path);
    for meta in &csproj_metas {
        let project_dir = Path::new(&meta.file_path)
            .parent()
            .map(|dir| dir.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        csharp_projects.add(&project_dir, meta.default_compile_items);
    }
    add_project_reference_aliases(&csproj_metas, &mut aliases);

    let workspace = workspace_scopes(repo_path, config_paths, &csproj_metas);
    RepoConfigs {
        aliases,
        csharp_projects,
        workspace,
    }
}

/// The repo-wide alias entries of the dirty state: the root configs'
/// entries, then the `ProjectReference` entries of each project in
/// `csproj_paths` (repo-relative), in that order, as
/// [`parse_repo_configs`] adds them. The caller names the projects, so
/// nothing is walked; the overlay of a dirty config passes the projects the
/// index stored, with the dirty ones added or removed
/// (`closure_store`).
pub fn repo_wide_aliases(repo_path: &Path, csproj_paths: &[String]) -> PathAliases {
    let mut aliases = root_aliases(repo_path);
    let metas: Vec<CsprojMeta> = csproj_paths
        .iter()
        .filter_map(|path| parse_single_csproj(&repo_path.join(path), repo_path))
        .collect();
    add_project_reference_aliases(&metas, &mut aliases);
    aliases
}

/// Each `ProjectReference` of `metas` as a path alias, so the resolver can
/// follow cross-project symbol references within the same solution.
fn add_project_reference_aliases(metas: &[CsprojMeta], aliases: &mut PathAliases) {
    for meta in metas {
        for proj_ref in &meta.project_references {
            // proj_ref is a relative path like "../OtherLib/OtherLib.csproj"
            // Strip the .csproj suffix and use the directory as the alias target.
            let ref_path = Path::new(proj_ref);
            let dir = ref_path
                .parent()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if !dir.is_empty() {
                // Reject ..-traversal: a malicious .csproj could otherwise
                // register an alias pointing outside the repo root, letting
                // the resolver follow it to read files outside the project.
                if dir.split('/').any(|seg| seg == "..") {
                    continue;
                }
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
}

/// The repo-wide alias entries of the repo-root configs, steps 1 to 9 of
/// [`parse_repo_configs`]. Reads only files at the repo root.
fn root_aliases(repo_path: &Path) -> PathAliases {
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
                    aliases.add(mod_name, vec!["./".to_string()]);
                    let pattern = format!("{}/*", mod_name);
                    aliases.add(&pattern, vec!["./*".to_string()]);
                }
                break;
            }
        }
    }

    // 4. Cargo.toml
    if let Some(name) = read_toml(&repo_path.join("Cargo.toml"))
        .as_ref()
        .and_then(|doc| toml_str(doc, &["package", "name"]))
    {
        aliases.add(format!("{name}/*"), vec!["src/*".to_string()]);
        aliases.add(
            name,
            vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
        );
    }

    // 5. pyproject.toml: PEP 621 `[project]`, else Poetry's `[tool.poetry]`.
    if let Some(name) = read_toml(&repo_path.join("pyproject.toml"))
        .as_ref()
        .and_then(|doc| {
            toml_str(doc, &["project", "name"])
                .or_else(|| toml_str(doc, &["tool", "poetry", "name"]))
        })
    {
        aliases.add(
            format!("{name}/*"),
            vec![format!("src/{name}/*"), format!("{name}/*")],
        );
    }

    // 6. jsconfig.json — identical schema to tsconfig.json `compilerOptions.paths`,
    //    used by JS-only projects that skip TypeScript compilation. Confidence 0.95.
    if let Ok(content) = std::fs::read_to_string(repo_path.join("jsconfig.json")) {
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

    // 7. jest.config.json — static JSON variant. Confidence 0.75.
    if let Ok(content) = std::fs::read_to_string(repo_path.join("jest.config.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            parse_jest_module_name_mapper(&json, &mut aliases);
        }
    }

    // 8. jest.config.js / jest.config.ts — JS/TS source, cannot eval.
    //    Regex-extract `moduleNameMapper` object literals; dynamic configs
    //    (variables, spreads, computed keys) are skipped with a BlindSpot note.
    //    Confidence 0.75.
    for jest_js in &["jest.config.js", "jest.config.ts"] {
        if let Ok(content) = std::fs::read_to_string(repo_path.join(jest_js)) {
            parse_jest_module_name_mapper_js(&content, &mut aliases);
            break; // prefer .js over .ts if both somehow exist
        }
    }

    // 9. webpack.config.js / webpack.config.ts — JS/TS source, cannot eval.
    //    Regex-extract `resolve.alias` object literals; `path.resolve(__dirname, ...)`
    //    captures the literal string argument (the `__dirname` substitution is
    //    documented as a limitation). Confidence 0.85.
    for webpack_js in &["webpack.config.js", "webpack.config.ts"] {
        if let Ok(content) = std::fs::read_to_string(repo_path.join(webpack_js)) {
            parse_webpack_resolve_alias(&content, &mut aliases);
            break;
        }
    }
    aliases
}

fn read_toml(path: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(path).ok()?.parse().ok()
}

fn toml_str<'a>(doc: &'a toml::Table, keys: &[&str]) -> Option<&'a str> {
    let (last, parents) = keys.split_last()?;
    let mut table = doc;
    for key in parents {
        table = table.get(*key)?.as_table()?;
    }
    table.get(*last)?.as_str().filter(|name| !name.is_empty())
}

// ─── Workspace scopes (C2 area 4) ───────────────────────────────────────────

/// Directories whose configs belong to installed dependencies, not to the
/// repo's own scopes. `ecp flow`'s source walk skips the same names.
const VENDORED_DIRS: [&str; 2] = ["node_modules", "vendor"];

/// Whether the index walk hands `rel_path` to [`parse_repo_configs`]: a
/// config name of every ecosystem but Cargo (the Rust module tree reads
/// `Cargo.toml`) and .NET (the `.csproj` scan reads those), outside a
/// vendored directory. The name is checked first, so a source path costs
/// one string match. Splits on `\` too, for a Windows relative path.
pub fn is_workspace_config(rel_path: &str) -> bool {
    let name = rel_path.rsplit(['/', '\\']).next().unwrap_or(rel_path);
    matches!(
        Ecosystem::classify_config(name),
        Some((ecosystem, _)) if !matches!(ecosystem, Ecosystem::Cargo | Ecosystem::DotNet)
    ) && !rel_path
        .split(['/', '\\'])
        .any(|segment| VENDORED_DIRS.contains(&segment))
}

/// The config files under `repo_path` that [`is_workspace_config`] takes,
/// repo-relative and sorted. For a caller with no index walk of its own;
/// the index build collects the same paths in its walk.
pub fn find_workspace_configs(repo_path: &Path) -> Vec<String> {
    let mut found: Vec<String> = ignore::WalkBuilder::new(repo_path)
        .hidden(false)
        .build()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let rel = entry.path().strip_prefix(repo_path).ok()?.to_str()?;
            let rel = rel.replace('\\', "/");
            (is_workspace_config(&rel) && entry.path().is_file()).then_some(rel)
        })
        .collect();
    found.sort_unstable();
    found
}

/// The scope rows of `config_paths` and of the `.csproj` scan. A row that
/// breaks a table invariant on its own is dropped; a row set that still
/// breaks one yields no rows, so a consumer never reads a half-built
/// table.
fn workspace_scopes(repo: &Path, config_paths: &[String], csproj: &[CsprojMeta]) -> WorkspaceFacts {
    WorkspaceFacts::from_drafts(scope_drafts(repo, config_paths, csproj)).unwrap_or_else(|err| {
        tracing::warn!("workspace scopes: the config rows break {err:?}; no rows stored");
        WorkspaceFacts::default()
    })
}

/// The scope rows the configs `config_paths` (repo-relative) and the
/// projects `csproj` make, as [`workspace_scopes`] stores them, for a caller
/// that merges them into a stored table: the overlay of a dirty config
/// (`closure_store`). Each `.csproj` path is parsed alone; no scan runs.
/// A node row reads only the configs listed here, so the caller passes
/// every config the rows it wants read (`Scope::config_files`).
pub fn parse_config_rows(repo_path: &Path, config_paths: &[String]) -> Vec<ScopeDraft> {
    let csproj: Vec<CsprojMeta> = config_paths
        .iter()
        .filter(|path| {
            matches!(
                Ecosystem::classify_config(file_name(path)),
                Some((Ecosystem::DotNet, _))
            )
        })
        .filter_map(|path| parse_single_csproj(&repo_path.join(path), repo_path))
        .collect();
    scope_drafts(repo_path, config_paths, &csproj)
}

/// Whether the parser rejects the config at the repo-relative `path`: a
/// JSON config that is not JSON with comments, a TOML config that is not
/// TOML, or a project file with no `<Project>` element. A config a line or
/// pattern reader takes (`go.mod`, `go.work`, a jest or webpack script,
/// `Package.swift`) is never rejected, and neither is an unreadable one:
/// the caller treats that as deleted.
pub fn config_rejected(repo_path: &Path, path: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(repo_path.join(path)) else {
        return false;
    };
    let name = file_name(path);
    if name.ends_with(".json") {
        parse_jsonc(&text).is_none()
    } else if name.ends_with(".toml") {
        text.parse::<toml::Table>().is_err()
    } else if name.to_ascii_lowercase().ends_with(".csproj") {
        let xml = strip_xml_comments(&text);
        !(xml.contains("<Project") && (xml.contains("</Project>") || xml.contains("/>")))
    } else {
        false
    }
}

/// The scope drafts of [`workspace_scopes`], each row that breaks a table
/// invariant on its own dropped.
fn scope_drafts(repo: &Path, config_paths: &[String], csproj: &[CsprojMeta]) -> Vec<ScopeDraft> {
    let mut paths: Vec<&str> = config_paths
        .iter()
        .map(String::as_str)
        .filter(|&path| is_workspace_config(path) && resolve_rel("", path).as_deref() == Some(path))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let reader = ConfigReader { repo };
    let mut drafts = node_scopes(&reader, &paths);
    drafts.extend(go_scopes(&reader, &paths));
    drafts.extend(
        paths_named(&paths, "pyproject.toml").filter_map(|path| python_scope(&reader, path)),
    );
    drafts.extend(
        paths_named(&paths, "composer.json").filter_map(|path| composer_scope(&reader, path)),
    );
    drafts
        .extend(paths_named(&paths, "Package.swift").filter_map(|path| swift_scope(&reader, path)));
    drafts.extend(csproj.iter().filter_map(|meta| dotnet_scope(&reader, meta)));
    // A malformed row costs that row only, never the table.
    drafts.retain(|draft| {
        let alone = WorkspaceFacts::from_drafts(vec![draft.clone()]);
        if let Err(err) = &alone {
            tracing::warn!(
                "workspace scopes: {} breaks {err:?}; row dropped",
                draft.scope.manifest()
            );
        }
        alone.is_ok()
    });
    drafts
}

struct ConfigReader<'a> {
    repo: &'a Path,
}

impl ConfigReader<'_> {
    /// The text of the repo-relative `path`; `None` when it cannot be read.
    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.repo.join(path)).ok()
    }

    fn is_file(&self, path: &str) -> bool {
        self.repo.join(path).is_file()
    }

    fn json(&self, path: &str) -> Option<serde_json::Value> {
        parse_jsonc(&self.read(path)?)
    }
}

fn paths_named<'a>(paths: &'a [&'a str], name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    paths
        .iter()
        .copied()
        .filter(move |path| file_name(path) == name)
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `rel` resolved against the repo-relative directory `base`, with `.` and
/// `..` folded and `\` read as `/`. `None` for an absolute path or one that
/// climbs above the repo root. The repo root is the empty string.
fn resolve_rel(base: &str, rel: &str) -> Option<String> {
    if rel.starts_with(['/', '\\']) || rel.contains(':') {
        return None;
    }
    let mut segments: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for segment in rel.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            segment => segments.push(segment),
        }
    }
    Some(segments.join("/"))
}

/// `dir` is `root` or below it, by whole segments.
fn is_within(dir: &str, root: &str) -> bool {
    root.is_empty()
        || dir
            .strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// An alias replacement as a scope row stores it (C2 `Scope.aliases`).
/// Relative to the repo root it stays verbatim, so a root scope's rows
/// equal today's `alias_table` rows; else it is resolved against `base`.
/// `None` for a replacement that leaves the repo or names nothing.
fn scope_replacement(base: &str, replacement: &str) -> Option<String> {
    if base.is_empty() {
        return (!replacement.trim().is_empty()).then(|| replacement.to_string());
    }
    resolve_rel(base, replacement).filter(|path| !path.is_empty())
}

/// Append an alias whose replacements are relative to `base`; an entry with
/// no replacement left is dropped (C2: each alias has one or more).
fn push_alias<'r>(
    aliases: &mut Vec<ScopeAlias>,
    base: &str,
    pattern: &str,
    replacements: impl IntoIterator<Item = &'r str>,
) {
    let replacements: Vec<String> = replacements
        .into_iter()
        .filter_map(|replacement| scope_replacement(base, replacement))
        .collect();
    if !pattern.is_empty() && !replacements.is_empty() {
        aliases.push(ScopeAlias {
            pattern: pattern.to_string(),
            replacements,
        });
    }
}

fn push_config(config_files: &mut Vec<ConfigRead>, path: String, presence: ConfigPresence) {
    if !config_files.iter().any(|config| config.path == path) {
        config_files.push(ConfigRead { path, presence });
    }
}

fn subtree_scope(ecosystem: Ecosystem, manifest: &str) -> Scope {
    Scope {
        ecosystem,
        root: parent_dir(manifest).to_string(),
        config_files: vec![ConfigRead {
            path: manifest.to_string(),
            presence: ConfigPresence::Read,
        }],
        membership: Membership::Subtree,
        entry_files: Vec::new(),
        aliases: Vec::new(),
        search_roots: Vec::new(),
        dependencies: Vec::new(),
    }
}

/// A trimmed string that is not blank: a `name` field as a scope name.
fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// JSON, else JSON with comments and trailing commas, as `tsconfig.json`
/// allows.
fn parse_jsonc(text: &str) -> Option<serde_json::Value> {
    serde_json::from_str(text)
        .ok()
        .or_else(|| serde_json::from_str(&strip_jsonc(text)).ok())
}

/// `text` without `//` and `/* */` comments and without a comma that only
/// whitespace separates from the next `}` or `]`. String contents stay.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                while chars.next_if(|&next| next != '\n').is_some() {}
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for next in chars.by_ref() {
                    if prev == '*' && next == '/' {
                        break;
                    }
                    prev = next;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    let mut cleaned = String::with_capacity(out.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in out.char_indices() {
        if in_string {
            in_string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else if c == '"' {
            in_string = true;
        } else if c == ',' && matches!(out[i + 1..].trim_start().chars().next(), Some('}' | ']')) {
            continue;
        }
        cleaned.push(c);
    }
    cleaned
}

fn json_str<'a>(json: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    json.get(key)?.as_str()
}

/// The strings of a JSON value that is a string or a list of strings; the
/// other items of a list are skipped.
fn json_strings(value: Option<&serde_json::Value>) -> Vec<&str> {
    match value {
        Some(serde_json::Value::String(s)) => vec![s.as_str()],
        Some(serde_json::Value::Array(items)) => {
            items.iter().filter_map(serde_json::Value::as_str).collect()
        }
        _ => Vec::new(),
    }
}

// ─── Node: package.json, tsconfig.json, jsconfig.json, jest, webpack ────────

/// The Node configs that make their directory a scope, in manifest order:
/// `package.json` first, so a `file:` dependency finds the scope by its
/// `package.json` path.
const NODE_MANIFESTS: [&str; 3] = ["package.json", "tsconfig.json", "jsconfig.json"];
const TS_CONFIGS: [&str; 2] = ["tsconfig.json", "jsconfig.json"];
const NODE_DEPENDENCY_KEYS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];
/// `package.json` entry fields, best first: a source entry before the
/// built ones, which a repo often does not hold.
const NODE_ENTRY_KEYS: [&str; 5] = ["source", "types", "typings", "module", "main"];

/// One Node scope per directory that holds a `package.json`, a
/// `tsconfig.json` or a `jsconfig.json`. The configs of one directory make
/// one row, so a package with both files is one scope, not two ambiguous
/// ones. A directory with no `tsconfig.json` / `jsconfig.json` of its own
/// reads the nearest ancestor's, as `tsc` does. Aliases keep today's order:
/// `tsconfig.json` paths, `package.json` imports, `jsconfig.json` paths,
/// jest, webpack.
fn node_scopes(reader: &ConfigReader, paths: &[&str]) -> Vec<ScopeDraft> {
    let mut dirs: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for &path in paths {
        if matches!(
            Ecosystem::classify_config(file_name(path)),
            Some((Ecosystem::Node, _))
        ) {
            dirs.entry(parent_dir(path))
                .or_default()
                .insert(file_name(path));
        }
    }
    let has = |dir: &str, name: &str| dirs.get(dir).is_some_and(|names| names.contains(name));
    let packages: BTreeMap<&str, serde_json::Value> = dirs
        .keys()
        .filter(|&&dir| has(dir, "package.json"))
        .filter_map(|&dir| Some((dir, reader.json(&join_path(dir, "package.json"))?)))
        .collect();
    let scope_dirs: Vec<&str> = dirs
        .keys()
        .copied()
        .filter(|&dir| NODE_MANIFESTS.iter().any(|&name| has(dir, name)))
        .collect();
    let workspace_of = node_workspace_members(&packages, &scope_dirs);
    scope_dirs
        .iter()
        .map(|&dir| {
            let manifest_name = NODE_MANIFESTS
                .iter()
                .find(|&&name| has(dir, name))
                .expect("a scope dir holds a manifest");
            let mut scope = subtree_scope(Ecosystem::Node, &join_path(dir, manifest_name));
            let ts_dir = ancestor_dirs(dir)
                .find(|&candidate| TS_CONFIGS.iter().any(|&name| has(candidate, name)));
            let ts_scope = |name: &str, scope: &mut Scope| {
                if let Some(ts_dir) = ts_dir.filter(|&ts_dir| has(ts_dir, name)) {
                    apply_tsconfig(reader, &join_path(ts_dir, name), scope);
                }
            };
            ts_scope("tsconfig.json", &mut scope);
            let package = packages.get(dir);
            if let Some(imports) = package
                .and_then(|json| json.get("imports"))
                .and_then(|v| v.as_object())
            {
                for (pattern, target) in imports {
                    push_alias(&mut scope.aliases, dir, pattern, target.as_str());
                }
            }
            ts_scope("jsconfig.json", &mut scope);
            apply_node_contributors(reader, dir, &has, &mut scope);
            let workspace_root = workspace_of.get(dir).copied();
            if let Some(root) = workspace_root.filter(|root| *root != dir) {
                push_config(
                    &mut scope.config_files,
                    join_path(root, "package.json"),
                    ConfigPresence::Read,
                );
            }
            let mut names = Vec::new();
            let mut requires = Vec::new();
            if let Some(json) = package {
                names.extend(non_blank(json_str(json, "name")).map(|n| (n, NameRank::Primary)));
                scope.entry_files = node_entry_files(json, dir);
                requires = node_requirements(json, dir, workspace_root.is_some());
            }
            ScopeDraft {
                scope,
                names,
                requires,
            }
        })
        .collect()
}

/// `dir`, then each ancestor up to the repo root.
fn ancestor_dirs(dir: &str) -> impl Iterator<Item = &str> {
    let mut next = Some(dir);
    std::iter::from_fn(move || {
        let current = next?;
        next = (!current.is_empty()).then(|| parent_dir(current));
        Some(current)
    })
}

/// The workspace root of each Node scope directory that some
/// `package.json` `workspaces` list names, the root included. A root's
/// globs are relative to the root; a `!` glob excludes. The innermost root
/// that lists a directory wins.
fn node_workspace_members<'a>(
    packages: &BTreeMap<&'a str, serde_json::Value>,
    scope_dirs: &[&'a str],
) -> BTreeMap<&'a str, &'a str> {
    let mut members = BTreeMap::new();
    for (&root, json) in packages {
        let globs = match json.get("workspaces") {
            Some(serde_json::Value::Object(table)) => json_strings(table.get("packages")),
            other => json_strings(other),
        };
        if globs.is_empty() {
            continue;
        }
        members.insert(root, root);
        for &dir in scope_dirs {
            let Some(rel) = dir
                .strip_prefix(root)
                .map(|rest| rest.trim_start_matches('/'))
                .filter(|rest| !rest.is_empty() && is_within(dir, root))
            else {
                continue;
            };
            let listed = globs
                .iter()
                .any(|glob| !glob.starts_with('!') && glob_matches(glob, rel));
            let excluded = globs
                .iter()
                .filter_map(|glob| glob.strip_prefix('!'))
                .any(|glob| glob_matches(glob, rel));
            if listed && !excluded {
                // `packages` iterates roots in path order, so a deeper root
                // comes later and overwrites an ancestor's claim.
                members.insert(dir, root);
            }
        }
    }
    members
}

/// A workspace glob against a relative directory, segment by segment: `*`
/// matches within one segment, `**` matches any number of segments.
fn glob_matches(glob: &str, path: &str) -> bool {
    fn segments(glob: &[&str], path: &[&str]) -> bool {
        match glob.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|skip| segments(rest, &path[skip..])),
            Some((head, rest)) => path
                .split_first()
                .is_some_and(|(first, tail)| wildcard(head, first) && segments(rest, tail)),
        }
    }
    let glob: Vec<&str> = glob
        .trim_start_matches("./")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&glob, &path)
}

/// `pattern` with `*` wildcards against one whole segment.
fn wildcard(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// The effective `compilerOptions` of one `tsconfig.json` chain.
#[derive(Default)]
struct TsOptions {
    /// The `paths` entries and the directory of the config that declared
    /// them, which they are relative to when no `baseUrl` is set.
    paths: Option<(serde_json::Map<String, serde_json::Value>, String)>,
    /// `baseUrl`, repo-relative.
    base_url: Option<String>,
}

/// Fold the `tsconfig.json` (or `jsconfig.json`) at `path` and its
/// `extends` chain into `scope`: every file of the chain goes into
/// `config_files`, a missing `extends` target as `Missing`; the effective
/// `paths` become aliases and `baseUrl` a search root.
fn apply_tsconfig(reader: &ConfigReader, path: &str, scope: &mut Scope) {
    let mut visited = Vec::new();
    let options = load_tsconfig(reader, path, &mut visited, &mut scope.config_files);
    if let Some(base_url) = &options.base_url {
        if !scope.search_roots.contains(base_url) {
            scope.search_roots.push(base_url.clone());
        }
    }
    let Some((paths, declared_in)) = options.paths else {
        return;
    };
    let base = options.base_url.as_deref().unwrap_or(&declared_in);
    for (pattern, targets) in &paths {
        // A string where TypeScript wants a list is a config error that
        // `tsc` reports; today's root parse skips it too.
        if let Some(targets) = targets.as_array() {
            push_alias(
                &mut scope.aliases,
                base,
                pattern,
                targets.iter().filter_map(serde_json::Value::as_str),
            );
        }
    }
}

/// The options of the config at `path` after its `extends` chain. A later
/// `extends` entry overrides an earlier one, and the file's own options
/// override both, as `tsc` merges them. A path seen before in this chain
/// is a cycle and adds nothing. A non-relative `extends` names an npm
/// package, which this parse does not follow.
fn load_tsconfig(
    reader: &ConfigReader,
    path: &str,
    visited: &mut Vec<String>,
    config_files: &mut Vec<ConfigRead>,
) -> TsOptions {
    if visited.iter().any(|seen| seen == path) {
        return TsOptions::default();
    }
    visited.push(path.to_string());
    let Some(text) = reader.read(path) else {
        push_config(config_files, path.to_string(), ConfigPresence::Missing);
        return TsOptions::default();
    };
    push_config(config_files, path.to_string(), ConfigPresence::Read);
    let Some(json) = parse_jsonc(&text) else {
        return TsOptions::default();
    };
    let dir = parent_dir(path);
    let mut options = TsOptions::default();
    for target in json_strings(json.get("extends")) {
        if !target.starts_with('.') {
            continue;
        }
        let Some(resolved) = resolve_rel(dir, target).filter(|p| !p.is_empty()) else {
            continue;
        };
        let resolved = if resolved.ends_with(".json") || reader.is_file(&resolved) {
            resolved
        } else {
            format!("{resolved}.json")
        };
        let inherited = load_tsconfig(reader, &resolved, visited, config_files);
        options.paths = inherited.paths.or(options.paths);
        options.base_url = inherited.base_url.or(options.base_url);
    }
    let compiler = json.get("compilerOptions");
    if let Some(base_url) = compiler
        .and_then(|c| c.get("baseUrl"))
        .and_then(|v| v.as_str())
        .and_then(|v| resolve_rel(dir, v))
    {
        options.base_url = Some(base_url);
    }
    if let Some(paths) = compiler
        .and_then(|c| c.get("paths"))
        .and_then(|v| v.as_object())
    {
        options.paths = Some((paths.clone(), dir.to_string()));
    }
    options
}

/// The jest and webpack configs of the scope's own directory, which today's
/// root parse reads only at the repo root. Their aliases are relative to
/// that directory.
fn apply_node_contributors(
    reader: &ConfigReader,
    dir: &str,
    has: &dyn Fn(&str, &str) -> bool,
    scope: &mut Scope,
) {
    let mut found = PathAliases::new();
    let read = |name: &str, scope: &mut Scope| -> Option<String> {
        if !has(dir, name) {
            return None;
        }
        let path = join_path(dir, name);
        let text = reader.read(&path)?;
        push_config(&mut scope.config_files, path, ConfigPresence::Read);
        Some(text)
    };
    if let Some(json) = read("jest.config.json", &mut *scope).and_then(|text| parse_jsonc(&text)) {
        parse_jest_module_name_mapper(&json, &mut found);
    }
    // Today's root parse prefers `.js` over `.ts`; so does this one.
    if let Some(text) = ["jest.config.js", "jest.config.ts"]
        .iter()
        .find_map(|&name| read(name, &mut *scope))
    {
        parse_jest_module_name_mapper_js(&text, &mut found);
    }
    if let Some(text) = ["webpack.config.js", "webpack.config.ts"]
        .iter()
        .find_map(|&name| read(name, &mut *scope))
    {
        parse_webpack_resolve_alias(&text, &mut found);
    }
    for (pattern, replacements) in found.entries() {
        push_alias(
            &mut scope.aliases,
            dir,
            pattern,
            replacements.iter().map(String::as_str),
        );
    }
}

fn node_entry_files(package: &serde_json::Value, dir: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    for key in NODE_ENTRY_KEYS {
        if let Some(entry) = json_str(package, key)
            .and_then(|value| resolve_rel(dir, value))
            .filter(|path| !path.is_empty() && !entries.contains(path))
        {
            entries.push(entry);
        }
    }
    entries
}

/// The declared dependencies of a `package.json`. A `file:` or `link:`
/// version names a directory in the repo; `workspace:` and `portal:` name
/// a workspace package; a bare version names an in-repo package only when
/// the declaring package is in a workspace, else the registry copy.
fn node_requirements(
    package: &serde_json::Value,
    dir: &str,
    in_workspace: bool,
) -> Vec<Requirement> {
    let mut requires = Vec::new();
    for key in NODE_DEPENDENCY_KEYS {
        let Some(table) = package.get(key).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, version) in table {
            let version = version.as_str().unwrap_or("").trim();
            let local = version
                .strip_prefix("file:")
                .or_else(|| version.strip_prefix("link:"));
            let target = if let Some(local) = local {
                match resolve_rel(dir, local) {
                    Some(target_dir) => {
                        RequirementTarget::ByManifest(join_path(&target_dir, "package.json"))
                    }
                    None => RequirementTarget::External,
                }
            } else if in_workspace
                || version.starts_with("workspace:")
                || version.starts_with("portal:")
            {
                RequirementTarget::ByName
            } else {
                RequirementTarget::External
            };
            requires.push(Requirement {
                name: name.clone(),
                target,
            });
        }
    }
    requires
}

// ─── Go: go.mod, go.work ────────────────────────────────────────────────────

/// One Go scope per `go.mod`; the innermost one governs a file (C2
/// `scope_of`). A `go.work` adds itself to each module it uses; a used
/// directory with no `go.mod` makes no scope, and every member of that
/// `go.work` lists the absent `go.mod` as `Missing`.
fn go_scopes(reader: &ConfigReader, paths: &[&str]) -> Vec<ScopeDraft> {
    let mut drafts: Vec<ScopeDraft> = paths_named(paths, "go.mod")
        .filter_map(|path| go_module(reader, path))
        .collect();
    for work in paths_named(paths, "go.work") {
        let Some(text) = reader.read(work) else {
            continue;
        };
        let used: Vec<String> = go_directives(&text, "use")
            .iter()
            .filter_map(|entry| entry.first())
            .filter_map(|dir| resolve_rel(parent_dir(work), dir))
            .map(|dir| join_path(&dir, "go.mod"))
            .collect();
        let (members, absent): (Vec<&String>, Vec<&String>) = used
            .iter()
            .partition(|manifest| drafts.iter().any(|d| d.scope.manifest() == *manifest));
        for draft in drafts
            .iter_mut()
            .filter(|draft| members.iter().any(|m| draft.scope.manifest() == *m))
        {
            let config_files = &mut draft.scope.config_files;
            push_config(config_files, work.to_string(), ConfigPresence::Read);
            for manifest in &absent {
                push_config(config_files, (*manifest).clone(), ConfigPresence::Missing);
            }
        }
    }
    drafts
}

/// The scope of one `go.mod`. The module path is its `Primary` name and two
/// aliases, `module` and `module/*`, joined to the scope root. A `replace`
/// to a local directory requires that directory's module by manifest; every
/// other `require` goes by name, so a required module of the repo is a
/// workspace dependency.
fn go_module(reader: &ConfigReader, path: &str) -> Option<ScopeDraft> {
    let text = reader.read(path)?;
    let mut scope = subtree_scope(Ecosystem::Go, path);
    let root = scope.root.clone();
    let module = go_directives(&text, "module")
        .into_iter()
        .next()
        .and_then(|words| words.into_iter().next());
    let mut names = Vec::new();
    if let Some(module) = module {
        push_alias(&mut scope.aliases, &root, &module, ["./"]);
        push_alias(&mut scope.aliases, &root, &format!("{module}/*"), ["./*"]);
        names.push((module, NameRank::Primary));
    }
    let mut requires: Vec<Requirement> = go_directives(&text, "replace")
        .into_iter()
        .filter_map(|words| {
            let arrow = words.iter().position(|word| word == "=>")?;
            let local = words.get(arrow + 1)?;
            if !local.starts_with('.') {
                return None;
            }
            let dir = resolve_rel(&root, local)?;
            Some(Requirement {
                name: words.first()?.clone(),
                target: RequirementTarget::ByManifest(join_path(&dir, "go.mod")),
            })
        })
        .collect();
    requires.extend(
        go_directives(&text, "require")
            .into_iter()
            .filter_map(|words| words.into_iter().next())
            .map(|name| Requirement {
                name,
                target: RequirementTarget::ByName,
            }),
    );
    Some(ScopeDraft {
        scope,
        names,
        requires,
    })
}

/// The entries of one `go.mod` / `go.work` directive, single-line or in a
/// `( … )` block, each split into words with quotes and `//` comments
/// removed.
fn go_directives(text: &str, directive: &str) -> Vec<Vec<String>> {
    let mut entries = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if in_block {
            if line.starts_with(')') {
                in_block = false;
            } else if !line.is_empty() {
                entries.push(go_words(line));
            }
            continue;
        }
        let Some(rest) = line.strip_prefix(directive) else {
            continue;
        };
        if !rest.is_empty() && !rest.starts_with([' ', '\t', '(']) {
            continue;
        }
        let rest = rest.trim();
        if rest.starts_with('(') {
            in_block = true;
        } else if !rest.is_empty() {
            entries.push(go_words(rest));
        }
    }
    entries
}

fn go_words(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(|word| word.trim_matches(['"', '`']).to_string())
        .filter(|word| !word.is_empty())
        .collect()
}

// ─── Python: pyproject.toml ─────────────────────────────────────────────────

/// The scope of one `pyproject.toml`. The project name (`[project] name`,
/// else `[tool.poetry] name`) keeps today's alias `name/*` →
/// `src/name/*`, `name/*`; its import spelling (`-` → `_`) is the
/// `Primary` name and a different raw name the `Alternate` one. The search
/// roots are the package roots the build backend declares, else the
/// project root. Dependencies are not read: a PEP 508 distribution name is
/// not the name an `import` writes.
fn python_scope(reader: &ConfigReader, path: &str) -> Option<ScopeDraft> {
    let text = reader.read(path)?;
    let mut scope = subtree_scope(Ecosystem::Python, path);
    let root = scope.root.clone();
    let doc: Option<toml::Table> = text.parse().ok();
    let mut names = Vec::new();
    if let Some(name) = doc.as_ref().and_then(|doc| {
        non_blank(
            toml_str(doc, &["project", "name"])
                .or_else(|| toml_str(doc, &["tool", "poetry", "name"])),
        )
    }) {
        push_alias(
            &mut scope.aliases,
            &root,
            &format!("{name}/*"),
            [
                format!("src/{name}/*").as_str(),
                format!("{name}/*").as_str(),
            ],
        );
        let import_name = name.replace('-', "_");
        if import_name != name {
            names.push((name, NameRank::Alternate));
        }
        names.push((import_name, NameRank::Primary));
    }
    let mut roots: Vec<String> = Vec::new();
    for dir in doc.as_ref().map(python_package_roots).unwrap_or_default() {
        if let Some(dir) = resolve_rel(&root, &dir) {
            if !roots.contains(&dir) {
                roots.push(dir);
            }
        }
    }
    if roots.is_empty() {
        roots.push(root);
    }
    scope.search_roots = roots;
    Some(ScopeDraft {
        scope,
        names,
        requires: Vec::new(),
    })
}

/// The package roots a `pyproject.toml` declares, relative to its
/// directory: setuptools `package-dir` `""` and `packages.find.where`
/// (a string or a list), Poetry `packages[].from`, and the parent of each
/// hatch wheel package.
fn python_package_roots(doc: &toml::Table) -> Vec<String> {
    fn value_at<'d>(doc: &'d toml::Table, keys: &[&str]) -> Option<&'d toml::Value> {
        let (last, parents) = keys.split_last()?;
        let mut table = doc;
        for key in parents {
            table = table.get(*key)?.as_table()?;
        }
        table.get(*last)
    }
    let at = |keys: &[&str]| value_at(doc, keys);
    let strings = |value: Option<&toml::Value>| -> Vec<String> {
        match value {
            Some(toml::Value::String(s)) => vec![s.clone()],
            Some(toml::Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        }
    };
    let mut roots = Vec::new();
    if let Some(dir) = at(&["tool", "setuptools", "package-dir"])
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(""))
        .and_then(|v| v.as_str())
    {
        roots.push(dir.to_string());
    }
    roots.extend(strings(at(&[
        "tool",
        "setuptools",
        "packages",
        "find",
        "where",
    ])));
    if let Some(packages) = at(&["tool", "poetry", "packages"]).and_then(|v| v.as_array()) {
        roots.extend(
            packages
                .iter()
                .filter_map(|p| p.get("from")?.as_str().map(str::to_string)),
        );
    }
    roots.extend(
        strings(at(&[
            "tool", "hatch", "build", "targets", "wheel", "packages",
        ]))
        .iter()
        .map(|package| parent_dir(package.trim_end_matches('/')).to_string()),
    );
    roots.retain(|dir| dir.is_empty() || !dir.trim().is_empty());
    roots
}

// ─── PHP: composer.json ─────────────────────────────────────────────────────

/// The scope of one `composer.json`. The package `name` is the `Primary`
/// name. Each `autoload` / `autoload-dev` PSR-4 prefix is an alias: the
/// namespace with `/` for `\` (`App\Models\` is `App/Models/*`), to each of
/// its directories (a string or a list). An empty prefix is the fallback
/// `*`. `require` / `require-dev` keys other than platform packages (`php`,
/// `ext-*`, `lib-*`, `composer-*`) are requirements by name.
fn composer_scope(reader: &ConfigReader, path: &str) -> Option<ScopeDraft> {
    if !reader.is_file(path) {
        return None;
    }
    let mut scope = subtree_scope(Ecosystem::Composer, path);
    let root = scope.root.clone();
    let meta = parse_single_composer_json(&reader.repo.join(path), reader.repo);
    let Some(meta) = meta else {
        return Some(ScopeDraft {
            scope,
            names: Vec::new(),
            requires: Vec::new(),
        });
    };
    for (prefix, dirs) in &meta.psr4 {
        let namespace = prefix.replace('\\', "/");
        let namespace = namespace.trim_matches('/');
        let pattern = if namespace.is_empty() {
            "*".to_string()
        } else {
            format!("{namespace}/*")
        };
        let replacements: Vec<String> = dirs
            .iter()
            .filter_map(|dir| resolve_rel(&root, dir))
            .map(|dir| join_path(&dir, "*"))
            .collect();
        push_alias(
            &mut scope.aliases,
            "",
            &pattern,
            replacements.iter().map(String::as_str),
        );
    }
    let names = non_blank(meta.name.as_deref())
        .map(|name| vec![(name, NameRank::Primary)])
        .unwrap_or_default();
    let requires = meta
        .requires
        .iter()
        .chain(&meta.requires_dev)
        .filter(|name| {
            *name != "php"
                && !name.starts_with("ext-")
                && !name.starts_with("lib-")
                && !name.starts_with("composer-")
        })
        .map(|name| Requirement {
            name: name.clone(),
            target: RequirementTarget::ByName,
        })
        .collect();
    Some(ScopeDraft {
        scope,
        names,
        requires,
    })
}

// ─── Swift: Package.swift ───────────────────────────────────────────────────

/// The scope of one `Package.swift`. Each target is a `Primary` name, since
/// `import X` names a target module, and an exact alias `X` → the target
/// directory (`path:`, else `Sources/X`, `Tests/X` for a test target). The
/// package name is an `Alternate` name. A `.package(path:)` dependency
/// requires that directory's package by manifest; a `.package(url:)` one is
/// not read, because its URL does not give the module names code imports.
fn swift_scope(reader: &ConfigReader, path: &str) -> Option<ScopeDraft> {
    let text = reader.read(path)?;
    let mut scope = subtree_scope(Ecosystem::SwiftPm, path);
    let root = scope.root.clone();
    // Whole-line comments only: `//` also starts a URL inside a string.
    let code: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut names = Vec::new();
    let targets: Vec<regex::Captures> = SWIFT_TARGET_RE.captures_iter(&code).collect();
    for (i, target) in targets.iter().enumerate() {
        let (Some(kind), Some(name)) = (target.get(1), target.get(2)) else {
            continue;
        };
        let body_end = targets
            .get(i + 1)
            .and_then(|next| next.get(0))
            .map_or(code.len(), |next| next.start());
        let body = &code[name.end()..body_end];
        let default_dir = if kind.as_str() == "testTarget" {
            format!("Tests/{}", name.as_str())
        } else {
            format!("Sources/{}", name.as_str())
        };
        let dir = SWIFT_PATH_ARG_RE
            .captures(body)
            .and_then(|c| c.get(1))
            .map_or(default_dir, |m| m.as_str().to_string());
        push_alias(&mut scope.aliases, &root, name.as_str(), [dir.as_str()]);
        names.push((name.as_str().to_string(), NameRank::Primary));
    }
    if let Some(package) = non_blank(
        PACKAGE_NAME_RE
            .captures(&code)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str()),
    ) {
        names.push((package, NameRank::Alternate));
    }
    let requires = SWIFT_PATH_DEPENDENCY_RE
        .captures_iter(&code)
        .filter_map(|c| resolve_rel(&root, c.get(1)?.as_str()))
        .filter(|dir| !dir.is_empty())
        .map(|dir| Requirement {
            name: file_name(&dir).to_string(),
            target: RequirementTarget::ByManifest(join_path(&dir, "Package.swift")),
        })
        .collect();
    Some(ScopeDraft {
        scope,
        names,
        requires,
    })
}

static SWIFT_TARGET_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"\.(target|executableTarget|testTarget|macro|plugin|systemLibrary)\s*\(\s*name\s*:\s*"([^"]+)""#,
    )
    .unwrap()
});
static SWIFT_PATH_ARG_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r#"\bpath\s*:\s*"([^"]+)""#).unwrap());
static SWIFT_PATH_DEPENDENCY_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"\.package\s*\(\s*(?:name\s*:\s*"[^"]*"\s*,\s*)?path\s*:\s*"([^"]+)""#)
            .unwrap()
    });

// ─── C#: *.csproj ───────────────────────────────────────────────────────────

/// The scope of one `.csproj`, from the scan's parse. Two projects in one
/// directory are two scopes (C2 identity). An SDK-style project governs its
/// subtree; any other lists its `<Compile Include>` files. `RootNamespace`
/// (else `AssemblyName`) is the `Primary` name, a different `AssemblyName`
/// the `Alternate` one. Each `ProjectReference` requires its project by
/// manifest under the file stem; a referenced file that does not exist is
/// a `Missing` config, so its appearance makes the scope stale.
fn dotnet_scope(reader: &ConfigReader, meta: &CsprojMeta) -> Option<ScopeDraft> {
    let manifest = resolve_rel("", &meta.file_path).filter(|path| !path.is_empty())?;
    // The scan matches `.csproj` in any case; a C2 manifest name is lower.
    let dotnet_name = matches!(
        Ecosystem::classify_config(file_name(&manifest)),
        Some((Ecosystem::DotNet, _))
    );
    if !dotnet_name || !is_workspace_config_dir(&manifest) {
        return None;
    }
    let mut scope = subtree_scope(Ecosystem::DotNet, &manifest);
    let root = scope.root.clone();
    if !meta.sdk_style {
        let mut files: Vec<String> = meta
            .compile_includes
            .iter()
            .filter(|include| !include.contains('*'))
            .filter_map(|include| resolve_rel(&root, include))
            .filter(|file| {
                file.to_ascii_lowercase().ends_with(".cs") && is_within(parent_dir(file), &root)
            })
            .collect();
        files.sort_unstable();
        files.dedup();
        scope.membership = Membership::Listed(files);
    }
    let mut names = Vec::new();
    let primary = non_blank(meta.root_namespace.as_deref())
        .or_else(|| non_blank(meta.assembly_name.as_deref()));
    if let Some(assembly) = non_blank(meta.assembly_name.as_deref())
        .filter(|assembly| Some(assembly) != primary.as_ref())
    {
        names.push((assembly, NameRank::Alternate));
    }
    names.extend(primary.map(|name| (name, NameRank::Primary)));
    let mut requires = Vec::new();
    for reference in &meta.project_references {
        let Some(target) = resolve_rel(&root, reference).filter(|path| !path.is_empty()) else {
            continue;
        };
        if !reader.is_file(&target) {
            push_config(
                &mut scope.config_files,
                target.clone(),
                ConfigPresence::Missing,
            );
        }
        let stem = file_name(&target);
        let stem = stem.rsplit_once('.').map_or(stem, |(stem, _)| stem);
        requires.push(Requirement {
            name: stem.to_string(),
            target: RequirementTarget::ByManifest(target.clone()),
        });
    }
    Some(ScopeDraft {
        scope,
        names,
        requires,
    })
}

/// A config path outside every vendored directory.
fn is_workspace_config_dir(path: &str) -> bool {
    !path
        .split('/')
        .any(|segment| VENDORED_DIRS.contains(&segment))
}

// ─── Jest / Webpack helpers ──────────────────────────────────────────────────

/// Parse `moduleNameMapper` from a `jest.config.json` value already
/// deserialised into [`serde_json::Value`].  Called for both the JSON variant
/// and the JS/TS regex-extracted fragment.
///
/// Jest keys are regex patterns.  We only promote patterns following the
/// `^prefix/(.*)$` shape to ecp aliases; anything else is too dynamic to
/// represent and is silently skipped (the skipped entry still maps in Jest —
/// we just can't pre-compute a path for the resolver).
///
/// Values are `string | [string, ...]`; arrays are registered in order so
/// fallback paths match Jest's resolution behavior.
fn parse_jest_module_name_mapper(json: &serde_json::Value, aliases: &mut PathAliases) {
    let Some(mapper) = json
        .pointer("/moduleNameMapper")
        .and_then(|v| v.as_object())
    else {
        return;
    };
    for (regex_key, target_val) in mapper {
        let Some(alias_pattern) = jest_regex_to_alias(regex_key) else {
            continue;
        };
        let replacements = match target_val {
            serde_json::Value::String(s) => vec![jest_replacement_to_glob(s)],
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(jest_replacement_to_glob)
                .collect(),
            _ => Vec::new(),
        };
        if !replacements.is_empty() {
            aliases.add(alias_pattern, replacements);
        }
    }
}

/// Extract `moduleNameMapper` entries from a JS/TS jest config file via regex.
///
/// Handles static object-literal keys of the form `"^prefix/(.*)$": "path/$1"`.
/// Dynamic JS constructs (variables, template literals, spreads, `require(…)`)
/// are not supported; those entries are silently skipped.
///
/// **BlindSpot note**: configs that export a function (`module.exports = () => …`)
/// or use `jest.config()` builders will yield zero entries — the caller should
/// surface a `BlindSpot` record if zero aliases were added from an otherwise
/// non-empty JS file.  That integration is left to the pipeline wiring layer;
/// this function focuses purely on extraction.
fn parse_jest_module_name_mapper_js(content: &str, aliases: &mut PathAliases) {
    // Match key-value pairs like:
    //   "^@app/(.*)$": "<rootDir>/src/app/$1"
    //   "^@app/(.*)$": ["<rootDir>/src/app/$1"]
    // Keys and values must be string literals (double-quoted).
    // We deliberately do not support single-quoted or backtick strings.
    let re = &JEST_MAPPER_PAIR_RE;
    for cap in re.captures_iter(content) {
        let key = cap.get(1).map_or("", |m| m.as_str());
        // Group 2 = plain string value, group 3 = first element of array value.
        let val = cap.get(2).or_else(|| cap.get(3)).map_or("", |m| m.as_str());
        if val.is_empty() {
            continue;
        }
        let Some(alias_pattern) = jest_regex_to_alias(key) else {
            continue;
        };
        aliases.add(alias_pattern, vec![jest_replacement_to_glob(val)]);
    }
}

/// Extract `resolve.alias` entries from a JS/TS webpack config file via regex.
///
/// Handles static object-literal values:
/// - Plain string:  `'@app': path.resolve(__dirname, 'src/app')` → `src/app/*`
/// - Plain string:  `'@app': './src/app'` → `./src/app/*`
///
/// The `__dirname` variable is treated as the repo root (empty prefix);
/// the literal path argument of `path.resolve(…)` is used directly.
///
/// **BlindSpot note**: keys that are not string literals (computed properties,
/// variables) are silently skipped.
fn parse_webpack_resolve_alias(content: &str, aliases: &mut PathAliases) {
    let re = &WEBPACK_ALIAS_PAIR_RE;
    for cap in re.captures_iter(content) {
        let key = cap.get(1).map_or("", |m| m.as_str());
        // Two capture groups: plain string (group 2) or path.resolve arg (group 3).
        let raw_val = cap.get(2).or_else(|| cap.get(3)).map_or("", |m| m.as_str());
        if key.is_empty() || raw_val.is_empty() {
            continue;
        }
        // Trim leading "./" or "/" to produce a relative path; keep it if not present.
        let val = raw_val.trim_start_matches("./");
        let pattern = format!("{}/*", key);
        let replacement = format!("{}/*", val);
        aliases.add(&pattern, vec![replacement]);
    }
}

/// Convert a Jest regex pattern to an ecp alias pattern, or return `None` if
/// the pattern is too dynamic to represent as a simple prefix wildcard.
///
/// Accepted shape: `^<literal_prefix>/(.*)$`  →  `<literal_prefix>/*`
/// Everything else (no `^`, no capturing group, infix patterns) is rejected.
fn jest_regex_to_alias(regex_key: &str) -> Option<String> {
    // Must start with `^` and end with `(.*)$` or `(.*)`
    let key = regex_key.trim_matches('"');
    if !key.starts_with('^') {
        return None;
    }
    let body = &key[1..]; // strip leading `^`
                          // Strip trailing `$` if present, then check for `(.*)`
    let body = body.strip_suffix('$').unwrap_or(body);
    let prefix = body.strip_suffix("(.*)")?;
    // Reject empty prefix — `^(.*)$` is a catch-all, not an alias.
    if prefix.is_empty() {
        return None;
    }
    // The ecp pattern is `prefix/*` where `*` captures the wildcard part.
    Some(format!("{}*", prefix))
}

/// Convert a Jest `$1` replacement to an ecp glob replacement (`*`).
///
/// `<rootDir>/src/app/$1` → `src/app/*`  (strips `<rootDir>/` prefix)
/// `./src/app/$1`         → `src/app/*`  (strips leading `./`)
/// `src/app/$1`           → `src/app/*`
fn jest_replacement_to_glob(val: &str) -> String {
    let v = val.trim_matches('"');
    // Strip `<rootDir>/` — jest's reference to the project root maps to the
    // repo root in ecp's path model.
    let v = v
        .strip_prefix("<rootDir>/")
        .or_else(|| v.strip_prefix("<rootDir>"))
        .unwrap_or(v);
    let v = v.trim_start_matches("./");
    // Replace Jest's `$1` with ecp's `*`.
    v.replace("$1", "*")
}

// Compile-once regexes for JS config extraction.
static JEST_MAPPER_PAIR_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // Matches: "^key$": "value"  or  "^key$": ["value", ...]
    // Group 1 = key (regex pattern), Group 2 = first string value.
    // Handles the array case by capturing only the first quoted element.
    regex::Regex::new(r#""(\^[^"]+)"\s*:\s*(?:"([^"]+)"|(?:\[\s*"([^"]+)"[^\]]*\]))"#).unwrap()
});

static WEBPACK_ALIAS_PAIR_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // Matches: '@key': 'value'  or  '@key': path.resolve(__dirname, 'value')
    // Also handles double-quoted keys/values.
    // Group 1 = alias key, Group 2 = plain string value, Group 3 = path.resolve arg.
    regex::Regex::new(
            r#"['"]([^'"]+)['"]\s*:\s*(?:path\.resolve\s*\(\s*(?:__dirname\s*,\s*)?['"]([^'"]+)['"]\s*\)|['"]([^'"]+)['"])"#,
        )
        .unwrap()
});

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
    /// Whether the project compiles every `.cs` file below its directory:
    /// an SDK-style project (`Sdk="…"`) that keeps its default compile
    /// items and removes none (`<Compile Remove=…>`). Any other project
    /// lists its files (`compile_includes`).
    pub default_compile_items: bool,
    /// Whether the project names an SDK (`Sdk="…"`): its compile items
    /// default to every `.cs` file below its directory.
    pub sdk_style: bool,
    /// `<RootNamespace>`, the namespace prefix of the project's code.
    pub root_namespace: Option<String>,
    /// `<Compile Include="…">` values as written, `\` included.
    pub compile_includes: Vec<String>,
}

/// Parsed metadata from `composer.json` (Task F2).
///
/// Mirrors the [`CsprojMeta`] shape: a `kind` discriminator plus the most
/// useful fields extracted from a PHP package manifest. `requires` /
/// `requires_dev` are key-only (we don't keep the version constraint
/// strings, mirroring how the matrix only tracks the dependency set).
#[derive(Debug, Clone, PartialEq)]
pub struct ComposerJsonMeta {
    /// Always `"composer-json"`.
    pub kind: &'static str,
    /// Relative path of the `composer.json` within the repo (forward-slash).
    pub file_path: String,
    /// Top-level `"name"` field, e.g. `"vendor/pkg"`.
    pub name: Option<String>,
    /// PHP version constraint from `require.php`, e.g. `"^8.0"`.
    pub php_version: Option<String>,
    /// Keys of the `"require"` object (excluding `"php"`).
    pub requires: Vec<String>,
    /// Keys of the `"require-dev"` object.
    pub requires_dev: Vec<String>,
    /// `autoload` then `autoload-dev` PSR-4 entries: the namespace prefix as
    /// written (`App\`) and its directories, one string or a list.
    pub psr4: Vec<(String, Vec<String>)>,
}

/// Parsed metadata from `Package.swift` (Task F3).
///
/// Mirrors the [`CsprojMeta`] shape. `Package.swift` is Swift source — not
/// JSON — so the parser is regex-based and intentionally conservative: it
/// extracts only the top-level package name, the leading
/// `// swift-tools-version:` magic comment, and every `.package(url: "…")`
/// dependency URL.
#[derive(Debug, Clone, PartialEq)]
pub struct SwiftPackageMeta {
    /// Always `"swift-package"`.
    pub kind: &'static str,
    /// Relative path of the `Package.swift` within the repo (forward-slash).
    pub file_path: String,
    /// Value parsed from `// swift-tools-version:<X.Y>` magic comment.
    pub tools_version: Option<String>,
    /// First `name: "…"` argument of the top-level `Package(…)` initializer.
    pub name: Option<String>,
    /// Every URL captured from `.package(url: "https://…", …)` calls.
    pub dependency_urls: Vec<String>,
}

/// Scan `repo_path` for all `*.csproj` files (up to 2 directory levels deep)
/// and parse each one.
/// Default directory-recursion depth for `*.csproj` discovery. Real .NET
/// monorepos often nest `src/<area>/<project>/<project>.csproj` (depth 3) or
/// `eng/templates/<thing>.csproj` (depth 2+); 4 covers the common cases
/// while still bounding worst-case I/O. Override at runtime via
/// `ECP_CSPROJ_MAX_DEPTH`.
const CSPROJ_MAX_DEPTH_DEFAULT: u8 = 4;

fn resolve_csproj_max_depth() -> u8 {
    std::env::var("ECP_CSPROJ_MAX_DEPTH")
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(CSPROJ_MAX_DEPTH_DEFAULT)
}

pub fn parse_csproj_files(repo_path: &Path) -> Vec<CsprojMeta> {
    // CI-N: replace the per-thread serial recursive read_dir walk with the
    // `ignore::WalkBuilder` parallel walker. The old impl issued one syscall
    // per directory on the main thread (.sample_repo: 2022 dirs at depth 4)
    // — measurable share of parse_configs wall. The new impl fans the walk
    // across rayon workers and uses `.csproj`-only collection.
    let max_depth = resolve_csproj_max_depth();
    let (tx, rx) = std::sync::mpsc::channel::<std::path::PathBuf>();
    let repo_root = repo_path.to_path_buf();
    ignore::WalkBuilder::new(repo_path)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .require_git(false)
        .max_depth(Some(max_depth as usize))
        .build_parallel()
        .run(|| {
            let tx = tx.clone();
            Box::new(move |result| {
                if let Ok(entry) = result {
                    let path = entry.path();
                    if path.is_file()
                        && path
                            .extension()
                            .map(|e| e.eq_ignore_ascii_case("csproj"))
                            .unwrap_or(false)
                    {
                        let _ = tx.send(path.to_path_buf());
                    }
                }
                ignore::WalkState::Continue
            })
        });
    drop(tx);
    rx.into_iter()
        .filter_map(|p| parse_single_csproj(&p, &repo_root))
        .collect()
}

fn parse_single_csproj(path: &Path, repo_root: &Path) -> Option<CsprojMeta> {
    let raw = std::fs::read_to_string(path).ok()?;
    let content = strip_xml_comments(&raw);
    let rel_path = path
        .strip_prefix(repo_root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"));

    let assembly_name = xml_first_text(&content, "AssemblyName")
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()));

    let target_framework = xml_first_text(&content, "TargetFramework")
        .or_else(|| xml_first_text(&content, "TargetFrameworks"));

    let package_references = xml_attrs_pairs(&content, "PackageReference", "Include", "Version");
    let project_references = xml_attrs_single(&content, "ProjectReference", "Include");
    let sdk_style = !xml_attrs_single(&content, "Project", "Sdk").is_empty()
        || !xml_attrs_single(&content, "Import", "Sdk").is_empty()
        || !xml_attrs_single(&content, "Sdk", "Name").is_empty();
    let disabled = |property: &str| {
        xml_first_text(&content, property).is_some_and(|value| value.eq_ignore_ascii_case("false"))
    };
    let default_compile_items = sdk_style
        && !disabled("EnableDefaultItems")
        && !disabled("EnableDefaultCompileItems")
        && xml_attrs_single(&content, "Compile", "Remove").is_empty();

    Some(CsprojMeta {
        kind: "csproj",
        file_path: rel_path,
        target_framework,
        assembly_name,
        package_references,
        project_references,
        default_compile_items,
        sdk_style,
        root_namespace: xml_first_text(&content, "RootNamespace"),
        compile_includes: xml_attrs_single(&content, "Compile", "Include"),
    })
}

// ─── PHP Config: composer.json (Task F2) ────────────────────────────────────

/// serde shape for `composer.json` — only the fields we care about.
/// Unknown keys are silently dropped (composer manifests carry many
/// optional fields we don't need: `autoload`, `scripts`, `config`, …).
#[derive(serde::Deserialize)]
struct ComposerJsonRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    require: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(rename = "require-dev", default)]
    require_dev: Option<serde_json::Map<String, serde_json::Value>>,
    // Values, not typed tables: composer writes an empty table as `[]`.
    #[serde(default)]
    autoload: Option<serde_json::Value>,
    #[serde(rename = "autoload-dev", default)]
    autoload_dev: Option<serde_json::Value>,
}

/// Parse a single `composer.json` file.  Returns `None` on read error or
/// malformed JSON (the caller decides whether absence is an error).
pub fn parse_single_composer_json(path: &Path, repo_root: &Path) -> Option<ComposerJsonMeta> {
    let content = std::fs::read_to_string(path).ok()?;
    let raw: ComposerJsonRaw = serde_json::from_str(&content).ok()?;
    let rel_path = path
        .strip_prefix(repo_root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"));

    let (php_version, requires) = raw.require.map_or((None, Vec::new()), |map| {
        let mut php = None;
        let mut keys = Vec::with_capacity(map.len());
        for (k, v) in map {
            if k == "php" {
                php = v.as_str().map(str::to_string);
            } else {
                keys.push(k);
            }
        }
        (php, keys)
    });
    let requires_dev = raw
        .require_dev
        .map(|m| m.into_iter().map(|(k, _)| k).collect())
        .unwrap_or_default();
    let psr4 = [raw.autoload, raw.autoload_dev]
        .iter()
        .flatten()
        .filter_map(|autoload| autoload.get("psr-4")?.as_object())
        .flat_map(|table| table.iter())
        .map(|(prefix, dirs)| {
            let dirs = json_strings(Some(dirs)).into_iter().map(str::to_string);
            (prefix.clone(), dirs.collect())
        })
        .collect();

    Some(ComposerJsonMeta {
        kind: "composer-json",
        file_path: rel_path,
        name: raw.name,
        php_version,
        requires,
        requires_dev,
        psr4,
    })
}

// ─── Swift Config: Package.swift (Task F3) ──────────────────────────────────

/// Parse a single `Package.swift` file via regex (it's Swift source, not JSON).
/// Returns `None` on read error.  Missing fields produce `None` / empty
/// collections — a syntactically valid but minimal Package.swift parses fine.
pub fn parse_single_swift_package(path: &Path, repo_root: &Path) -> Option<SwiftPackageMeta> {
    let content = std::fs::read_to_string(path).ok()?;
    let rel_path = path
        .strip_prefix(repo_root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"));

    // `// swift-tools-version:5.9` or `// swift-tools-version: 5.9` — the
    // magic comment is conventionally on the very first line, but we scan
    // the leading comment block to tolerate banner comments above it.
    let tools_version = TOOLS_VERSION_RE
        .captures(&content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string());

    // Top-level `Package(name: "MyPkg", …)` — we look for the first
    // `name:` argument after the `Package(` token. A more thorough parser
    // would walk the Swift AST, but Package.swift is highly stylized and
    // the regex captures the conventional shape used by every package on
    // the SwiftPM index.
    let name = PACKAGE_NAME_RE
        .captures(&content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());

    let dependency_urls: Vec<String> = DEPENDENCY_URL_RE
        .captures_iter(&content)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();

    Some(SwiftPackageMeta {
        kind: "swift-package",
        file_path: rel_path,
        tools_version,
        name,
        dependency_urls,
    })
}

// Compile-once regexes — `Package.swift` parsing happens once per repo so
// the static guard amortizes the regex build across any future callers
// that loop over multiple packages.
static TOOLS_VERSION_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"//\s*swift-tools-version\s*:\s*([0-9][^\s\r\n]*)").unwrap()
});
static PACKAGE_NAME_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r#"Package\s*\(\s*name\s*:\s*"([^"]+)""#).unwrap()
});
static DEPENDENCY_URL_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r#"\.package\s*\(\s*[^)]*?url\s*:\s*"([^"]+)""#).unwrap()
});

// ─── Minimal XML helpers (no external parser needed) ────────────────────────

/// Replace every `<!-- ... -->` block in `xml` with whitespace of equal length
/// (so byte offsets / line numbers are preserved if anything ever cares).
/// Returns the cleaned text. Unterminated comments are dropped from `start`
/// to end-of-input.
fn strip_xml_comments(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 4..];
        if let Some(close) = after_open.find("-->") {
            // Preserve newlines inside the comment so line numbers don't drift.
            for ch in after_open[..close].chars() {
                out.push(if ch == '\n' { '\n' } else { ' ' });
            }
            out.push_str("   "); // standing in for `-->` (3 chars)
            rest = &after_open[close + 3..];
        } else {
            // Unterminated comment — drop the rest.
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Extract the inner text of the first `<TagName>…</TagName>` occurrence.
fn xml_first_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let text = xml[start..end].trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// For every element matching `<ElemName … AttrA="…" AttrB="…" …>` return
/// `(value_of_AttrA, value_of_AttrB)` pairs.  Attribute order in source
/// doesn't matter; we scan for each name independently.
fn xml_attrs_pairs(xml: &str, elem: &str, attr_a: &str, attr_b: &str) -> Vec<(String, String)> {
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
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
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

    /// When a `<TargetFramework>` tag is commented-out (e.g. an older value
    /// left in a `<!-- ... -->` block above the real one), the parser must
    /// skip the commented value and return the real TFM. The previous
    /// implementation used naive string-find and could pick the comment.
    #[test]
    fn csproj_commented_target_framework_is_skipped() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <!-- legacy: <TargetFramework>net5.0</TargetFramework> -->
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
</Project>"#,
        )
        .unwrap();
        let metas = parse_csproj_files(dir.path());
        assert_eq!(metas.len(), 1);
        assert_eq!(
            metas[0].target_framework.as_deref(),
            Some("net8.0"),
            "commented <TargetFramework>net5.0</TargetFramework> must not shadow the real value; got {:?}",
            metas[0].target_framework,
        );
    }

    /// A malicious or compromised .csproj with a `..`-traversal
    /// `<ProjectReference Include="...">` must not register an alias that
    /// escapes the repo root — otherwise the downstream resolver would follow
    /// it to read files outside the project.
    #[test]
    fn csproj_traversal_path_rejected_from_aliases() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Evil.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <ProjectReference Include="../../../../etc/shadow.csproj" />
  </ItemGroup>
</Project>"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut traversal_found = false;
        aliases.expand("shadow", |c| {
            if c.split('/').any(|seg| seg == "..") {
                traversal_found = true;
            }
            true
        });
        assert!(
            !traversal_found,
            "..-traversal ProjectReference must not be registered as an alias"
        );
    }

    // ── jsconfig.json ─────────────────────────────────────────────────────

    #[test]
    fn jsconfig_json_compiler_options_paths_loaded() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("jsconfig.json"),
            r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@ui/*": ["src/ui/*"] } } }"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut found = Vec::new();
        aliases.expand("@ui/Button", |c| {
            found.push(c.to_string());
            true
        });
        assert_eq!(
            found,
            vec!["src/ui/Button"],
            "jsconfig.json paths must resolve"
        );
    }

    // ── jest.config.json ──────────────────────────────────────────────────

    #[test]
    fn jest_config_json_module_name_mapper_string_value() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("jest.config.json"),
            r#"{ "moduleNameMapper": { "^@app/(.*)$": "<rootDir>/src/app/$1" } }"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut found = Vec::new();
        aliases.expand("@app/services/Auth", |c| {
            found.push(c.to_string());
            true
        });
        assert_eq!(
            found,
            vec!["src/app/services/Auth"],
            "jest.config.json moduleNameMapper string value must resolve"
        );
    }

    #[test]
    fn jest_config_json_module_name_mapper_array_value() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("jest.config.json"),
            r#"{ "moduleNameMapper": { "^@lib/(.*)$": ["<rootDir>/src/lib/$1", "<rootDir>/lib/$1"] } }"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut found = Vec::new();
        aliases.expand("@lib/utils", |c| {
            found.push(c.to_string());
            true
        });
        assert_eq!(
            found,
            vec!["src/lib/utils", "lib/utils"],
            "jest.config.json array values must resolve in order"
        );
    }

    #[test]
    fn jest_config_json_non_prefix_regex_is_skipped() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("jest.config.json"),
            // This pattern has no `^` anchor — too dynamic to represent.
            r#"{ "moduleNameMapper": { "dynamic(.*)": "src/$1" } }"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut found = Vec::new();
        aliases.expand("dynamic/foo", |c| {
            found.push(c.to_string());
            true
        });
        assert!(
            found.is_empty(),
            "non-prefix regex patterns must be skipped; got {:?}",
            found
        );
    }

    // ── jest.config.js (JS text extraction) ──────────────────────────────

    #[test]
    fn jest_config_js_module_name_mapper_extracted() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("jest.config.js"),
            r#"
module.exports = {
  moduleNameMapper: {
    "^@shared/(.*)$": "<rootDir>/packages/shared/$1",
  },
};
"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut found = Vec::new();
        aliases.expand("@shared/hooks/useAuth", |c| {
            found.push(c.to_string());
            true
        });
        assert_eq!(
            found,
            vec!["packages/shared/hooks/useAuth"],
            "jest.config.js moduleNameMapper must be extracted via regex"
        );
    }

    // ── webpack.config.js ─────────────────────────────────────────────────

    #[test]
    fn webpack_config_js_resolve_alias_plain_string() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("webpack.config.js"),
            r#"
const path = require('path');
module.exports = {
  resolve: {
    alias: {
      '@components': './src/components',
      '@utils': './src/utils',
    },
  },
};
"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut components = Vec::new();
        aliases.expand("@components/Button", |c| {
            components.push(c.to_string());
            true
        });
        assert_eq!(
            components,
            vec!["src/components/Button"],
            "webpack.config.js plain string alias must resolve"
        );

        let mut utils = Vec::new();
        aliases.expand("@utils/format", |c| {
            utils.push(c.to_string());
            true
        });
        assert_eq!(
            utils,
            vec!["src/utils/format"],
            "webpack.config.js second alias must resolve"
        );
    }

    #[test]
    fn webpack_config_js_resolve_alias_path_resolve() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("webpack.config.js"),
            r#"
const path = require('path');
module.exports = {
  resolve: {
    alias: {
      '@api': path.resolve(__dirname, 'src/api'),
    },
  },
};
"#,
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        let mut found = Vec::new();
        aliases.expand("@api/client", |c| {
            found.push(c.to_string());
            true
        });
        assert_eq!(
            found,
            vec!["src/api/client"],
            "webpack.config.js path.resolve alias must resolve (capturing literal arg, __dirname substituted)"
        );
    }

    fn expansions(aliases: &PathAliases, specifier: &str) -> Vec<String> {
        let mut found = Vec::new();
        aliases.expand(specifier, |c| {
            found.push(c.to_string());
            true
        });
        found
    }

    #[test]
    fn test_parse_configs_pyproject_other_name_keys_no_alias() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"pkg\"\nnamespace_packages = true\n\n[[tool.poetry.source]]\nname = \"pypi\"\n",
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        assert!(
            expansions(&aliases, "pkg/mod")
                .iter()
                .any(|c| c.starts_with("src/pkg")),
            "[project] name registers the package alias"
        );
        assert!(expansions(&aliases, "pypi/mod").is_empty());
        assert!(expansions(&aliases, "true/mod").is_empty());
    }

    #[test]
    fn test_parse_configs_pyproject_poetry_name_registers_alias() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"poetrypkg\"\n",
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        assert!(expansions(&aliases, "poetrypkg/mod")
            .iter()
            .any(|c| c.starts_with("src/poetrypkg")));
    }

    #[test]
    fn test_parse_configs_cargo_name_with_trailing_comment_registers_clean_name() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"mycrate\" # the crate\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let aliases = parse_configs(dir.path());
        assert!(expansions(&aliases, "mycrate").contains(&"src/lib.rs".to_string()));
    }

    // ── R-A4d: the rows of a given config list ────────────────────────────

    fn tree(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (path, text) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        dir
    }

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|p| p.to_string()).collect()
    }

    /// Stands for: the rows of a config list equal the rows the index
    /// stores for the same configs, so the overlay's merge and a fresh
    /// build agree on every row it rewrites.
    #[test]
    fn test_parse_config_rows_equal_index_rows_for_same_configs() {
        let dir = tree(&[
            (
                "tsconfig.json",
                r#"{ "compilerOptions": { "paths": { "@/*": ["src/b/*"] } } }"#,
            ),
            ("go.mod", "module example.com/m\n\ngo 1.21\n"),
            ("svc/pyproject.toml", "[project]\nname = \"svc-a\"\n"),
            (
                "php/composer.json",
                r#"{ "name": "acme/a", "autoload": { "psr-4": { "App\\": "lib/" } } }"#,
            ),
            ("App/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />\n"),
        ]);
        let configs = paths(&[
            "App/App.csproj",
            "go.mod",
            "php/composer.json",
            "svc/pyproject.toml",
            "tsconfig.json",
        ]);
        let rows = WorkspaceFacts::from_drafts(parse_config_rows(dir.path(), &configs)).unwrap();
        let index = parse_repo_configs(dir.path(), &configs).workspace;
        assert_eq!(rows, index);
        let node = rows
            .scopes
            .iter()
            .find(|s| s.ecosystem == Ecosystem::Node)
            .unwrap();
        assert_eq!(node.aliases[0].replacements, vec!["src/b/*".to_string()]);
    }

    /// Stands for: an empty list, and a list whose only config is absent
    /// from disk, make no row.
    #[test]
    fn test_parse_config_rows_empty_or_absent_returns_no_row() {
        let dir = tree(&[]);
        assert!(parse_config_rows(dir.path(), &[]).is_empty());
        assert!(parse_config_rows(dir.path(), &paths(&["go.mod", "A.csproj"])).is_empty());
    }

    /// Stands for: the parser rejects malformed JSON, TOML and project XML,
    /// and an empty or whitespace-only JSON config; it takes JSON with
    /// comments, an empty TOML file, and a config a line reader reads.
    #[test]
    fn test_config_rejected_malformed_json_toml_xml_true() {
        let dir = tree(&[
            ("tsconfig.json", "{ \"compilerOptions\": "),
            ("blank/tsconfig.json", "   \n"),
            (
                "ok/tsconfig.json",
                "{ // comment\n \"compilerOptions\": {}, }",
            ),
            ("pyproject.toml", "[project\nname = "),
            ("empty/pyproject.toml", ""),
            ("Bad.csproj", "<Proj"),
            ("Good.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />"),
            ("go.mod", "}}} not go"),
        ]);
        assert!(config_rejected(dir.path(), "tsconfig.json"));
        assert!(config_rejected(dir.path(), "blank/tsconfig.json"));
        assert!(!config_rejected(dir.path(), "ok/tsconfig.json"));
        assert!(config_rejected(dir.path(), "pyproject.toml"));
        assert!(!config_rejected(dir.path(), "empty/pyproject.toml"));
        assert!(config_rejected(dir.path(), "Bad.csproj"));
        assert!(!config_rejected(dir.path(), "Good.csproj"));
        assert!(!config_rejected(dir.path(), "go.mod"));
        assert!(!config_rejected(dir.path(), "absent/tsconfig.json"));
    }

    /// Stands for: the repo-wide entries of a named project list equal the
    /// index's, whose scan finds the same projects.
    #[test]
    fn test_repo_wide_aliases_named_projects_equal_index_entries() {
        let dir = tree(&[
            (
                "tsconfig.json",
                r#"{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }"#,
            ),
            (
                "App.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <ProjectReference Include=\"Shared/Shared.csproj\" />\n  </ItemGroup>\n</Project>\n",
            ),
            ("Shared/Shared.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />\n"),
        ]);
        let named = repo_wide_aliases(dir.path(), &paths(&["App.csproj", "Shared/Shared.csproj"]));
        let index = parse_configs(dir.path());
        let entries = |a: &PathAliases| -> Vec<(String, Vec<String>)> {
            a.entries()
                .map(|(p, r)| (p.to_string(), r.to_vec()))
                .collect()
        };
        assert_eq!(entries(&named), entries(&index));
        assert!(entries(&named).iter().any(|(p, _)| p == "Shared"));
    }
}
