//! `cgn tool-map` — enumerate calls to external HTTP / DB / Redis / queue
//! clients by tracking **package imports**, not hard-coded callee names.
//!
//! Strategy:
//!
//! 1. **Package catalog** (`PACKAGE_CATEGORY`) maps a stable set of
//!    third-party package names (`axios` / `requests` / `redis` / …) to
//!    their tool category. This is the only hard-coded list — it stays
//!    smaller and more stable than enumerating every callee form like
//!    `axios.get` / `axios.head` / `axios.options`.
//! 2. **Per file, lang-aware import parsing** finds bindings introduced
//!    by imports whose source matches a catalog package. The binding
//!    might be the package name itself (`import axios from "axios"`) or
//!    an alias (`import req from "axios"`), or a named import
//!    (`from requests import get` → binding `get`).
//! 3. **Usage scan** locates every occurrence of those bindings in the
//!    file body — `axios.head(…)` / `req(…)` / `get(url)` — using AST-
//!    like word-boundary checks so `axiosFoo` doesn't match `axios`.
//!
//! Compared to the previous hard-coded-callee version this catches:
//! - aliased imports (`import req from "axios"` → any `req.x()`)
//! - any method on a known package (`axios.head` / `axios.options` etc.)
//! - bare-call clients (`requests(...)` if someone aliased the module)
//!
//! The catalog stays minimal: only well-known packages, additions go in
//! one place. Languages covered: TS / JS, Python, Go, Rust.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use cgn_core::graph::ArchivedFileCategory;
use cgn_core::GnxError;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Package source → category. Match is exact-string against the import
/// source as it appears in source code (TS `"axios"`, Python `requests`,
/// Go `net/http`, Rust `reqwest`). Sub-paths like `"axios/lib"` are
/// matched via prefix-with-`/` so deep imports still classify.
const PACKAGE_CATEGORY: &[(&str, Category)] = &[
    // HTTP — JS/TS
    ("axios", Category::Http),
    ("got", Category::Http),
    ("ky", Category::Http),
    ("node-fetch", Category::Http),
    ("undici", Category::Http),
    // HTTP — Python
    ("requests", Category::Http),
    ("httpx", Category::Http),
    ("aiohttp", Category::Http),
    ("urllib3", Category::Http),
    ("urllib.request", Category::Http),
    // HTTP — Go (stdlib)
    ("net/http", Category::Http),
    // HTTP — Rust
    ("reqwest", Category::Http),
    ("hyper", Category::Http),
    ("ureq", Category::Http),
    // DB — SQL clients
    ("pg", Category::Db),
    ("mysql", Category::Db),
    ("mysql2", Category::Db),
    ("psycopg", Category::Db),
    ("psycopg2", Category::Db),
    ("sqlalchemy", Category::Db),
    ("sqlx", Category::Db),
    // DB — ORM
    ("mongoose", Category::Db),
    ("prisma", Category::Db),
    ("drizzle-orm", Category::Db),
    ("typeorm", Category::Db),
    ("sequelize", Category::Db),
    // DB — Mongo
    ("mongodb", Category::Db),
    ("pymongo", Category::Db),
    // Redis
    ("redis", Category::Redis),
    ("ioredis", Category::Redis),
    ("redis-py", Category::Redis),
    // Queue / messaging
    ("celery", Category::Queue),
    ("bull", Category::Queue),
    ("bullmq", Category::Queue),
    ("amqplib", Category::Queue),
    ("kafkajs", Category::Queue),
    ("aiokafka", Category::Queue),
    ("@aws-sdk/client-sqs", Category::Queue),
    ("boto3", Category::Queue),
];

/// Built-in JS / Web identifiers that don't appear in imports but still
/// behave as HTTP clients. Detected by bare usage anywhere in the file.
/// Kept short — only globally available, broadly recognized names.
const GLOBAL_HTTP_BINDINGS: &[&str] = &["fetch", "XMLHttpRequest"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Http,
    Db,
    Redis,
    Queue,
}

impl Category {
    fn as_key(self) -> &'static str {
        match self {
            Category::Http => "http",
            Category::Db => "db",
            Category::Redis => "redis",
            Category::Queue => "queue",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "http" => Some(Category::Http),
            "db" => Some(Category::Db),
            "redis" => Some(Category::Redis),
            "queue" => Some(Category::Queue),
            _ => None,
        }
    }
}

/// Classify an import source. Matches the catalog exactly, or by `pkg/...`
/// sub-path prefix so `axios/lib/foo` still classifies as `axios`.
fn classify_source(src: &str) -> Option<Category> {
    for (pkg, cat) in PACKAGE_CATEGORY {
        if src == *pkg || src.starts_with(&format!("{pkg}/")) {
            return Some(*cat);
        }
    }
    None
}

#[derive(Args, Debug, Clone)]
pub struct ToolMapArgs {
    /// Filter to a specific category. Comma-separated: `http`, `db`,
    /// `redis`, `queue`. Empty = all.
    #[arg(long, alias = "categories")]
    pub category: Option<String>,

    #[arg(long)]
    pub repo: Option<String>,

    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: ToolMapArgs, engine: &Engine) -> Result<(), GnxError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let payload = build_payload(&args, engine)?;
    emit(&payload, format)
}

pub fn build_payload(args: &ToolMapArgs, engine: &Engine) -> Result<serde_json::Value, GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    let filter: Option<Vec<Category>> = match args.category.as_deref() {
        None => None,
        Some(s) => {
            let mut parsed = Vec::new();
            for tok in s.split(',') {
                let tok = tok.trim();
                if tok.is_empty() {
                    continue;
                }
                match Category::parse(tok) {
                    Some(c) => parsed.push(c),
                    None => {
                        return Err(GnxError::InvalidArgument(format!(
                            "unknown category '{tok}' — valid: http, db, redis, queue"
                        )));
                    }
                }
            }
            if parsed.is_empty() {
                None
            } else {
                Some(parsed)
            }
        }
    };

    let category_allowed = |c: Category| -> bool {
        match filter.as_ref() {
            None => true,
            Some(list) => list.contains(&c),
        }
    };

    let mut calls: HashMap<&'static str, Vec<serde_json::Value>> = HashMap::new();
    let mut totals: HashMap<&'static str, usize> = HashMap::new();
    for c in [
        Category::Http,
        Category::Db,
        Category::Redis,
        Category::Queue,
    ] {
        if category_allowed(c) {
            calls.insert(c.as_key(), Vec::new());
            totals.insert(c.as_key(), 0);
        }
    }

    let repo_root: PathBuf = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    for file in graph.files.iter() {
        // `Example` is intentionally NOT skipped: framework example apps
        // (NestJS `sample/`, Flask `examples/tutorial/`) demonstrate the
        // canonical tool / handler shapes a user would copy from. Only
        // `Test` (test fixtures) and `Reference` (vendor / node_modules)
        // are filtered. Symmetric with the routes.rs and builder.rs
        // route-emission filters.
        if matches!(
            file.category,
            ArchivedFileCategory::Test | ArchivedFileCategory::Reference
        ) {
            continue;
        }
        let rel = file.path.resolve(&graph.string_pool);
        let abs = repo_root.join(rel);
        let Ok(src) = std::fs::read_to_string(&abs) else {
            continue;
        };

        // Step 1: per-file binding map — name → (category, package).
        let bindings = collect_tool_bindings(rel, &src);
        if bindings.is_empty() {
            continue;
        }

        // Step 2: scan body for binding usage (`<binding>.x(...)` or bare
        // `<binding>(...)`). Skip lines that look like the import declaration
        // itself so we don't double-count.
        for (line_idx, line) in src.lines().enumerate() {
            if is_import_line(rel, line) {
                continue;
            }
            for hit in find_binding_uses(line, &bindings) {
                if !category_allowed(hit.category) {
                    continue;
                }
                let entry = serde_json::json!({
                    "callee": hit.callee,
                    "package": hit.package,
                    "filePath": rel,
                    "line": line_idx + 1,
                    "col": hit.col + 1,
                });
                let key = hit.category.as_key();
                calls.get_mut(key).expect("bucket pre-seeded").push(entry);
                *totals.get_mut(key).expect("bucket pre-seeded") += 1;
            }
        }
    }

    Ok(serde_json::json!({
        "status": "success",
        "totals": totals,
        "calls": calls,
    }))
}

/// Per-file binding: name as it appears in source → (category, package
/// that brought it in). Includes `GLOBAL_HTTP_BINDINGS` so platform
/// primitives like `fetch` register without an import.
fn collect_tool_bindings(path: &str, src: &str) -> HashMap<String, (Category, String)> {
    let mut out: HashMap<String, (Category, String)> = HashMap::new();

    let ext = path.rsplit('.').next().unwrap_or("");
    let is_js_family = matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs");

    // `fetch` / `XMLHttpRequest` are JS/browser/Deno globals — `.rs` `.py` `.go`
    // each have their own `fetch` vocabulary (graph node fetch, dict.fetch, …)
    // that must NOT be tagged HTTP. Gate on the JS family before inserting.
    if is_js_family {
        for &g in GLOBAL_HTTP_BINDINGS {
            out.insert(g.to_string(), (Category::Http, g.to_string()));
        }
    }

    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => parse_js_imports(src, &mut out),
        "py" => parse_python_imports(src, &mut out),
        "go" => parse_go_imports(src, &mut out),
        "rs" => parse_rust_imports(src, &mut out),
        _ => {}
    }

    out
}

/// Is `line` an import declaration? Lang-aware: skip the line entirely so
/// the import statement itself doesn't count as a usage.
fn is_import_line(path: &str, line: &str) -> bool {
    let trimmed = line.trim_start();
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            trimmed.starts_with("import ") || trimmed.starts_with("import{")
        }
        "py" => {
            trimmed.starts_with("import ")
                || trimmed.starts_with("from ")
                || trimmed.starts_with("from\t")
        }
        "go" => trimmed.starts_with("import ") || trimmed.starts_with("import("),
        "rs" => trimmed.starts_with("use ") || trimmed.starts_with("extern crate"),
        _ => false,
    }
}

struct Hit {
    callee: String,
    package: String,
    category: Category,
    col: usize,
}

/// Find every occurrence of any binding name on this line, in two forms:
/// - `binding.member(...)` — emit `binding.member` as callee
/// - bare `binding(...)`   — emit `binding` as callee
///
/// Word-boundary check ensures `axiosFoo` doesn't match `axios`.
fn find_binding_uses(line: &str, bindings: &HashMap<String, (Category, String)>) -> Vec<Hit> {
    let mut hits = Vec::new();
    let bytes = line.as_bytes();
    let mut seen_cols: HashSet<usize> = HashSet::new();
    for (name, (cat, pkg)) in bindings {
        let nb = name.as_bytes();
        if nb.is_empty() {
            continue;
        }
        let mut start = 0usize;
        while start + nb.len() <= bytes.len() {
            let Some(pos_rel) = find_subslice(&bytes[start..], nb) else {
                break;
            };
            let abs = start + pos_rel;
            let before_ok = abs == 0 || !is_word_char(bytes[abs - 1]);
            let after_idx = abs + nb.len();
            // Two acceptance shapes after the binding name:
            //   1. `.<word>` → method call form  → callee = `name.<word>`
            //   2. bare      → must be followed by `(` to count as a call
            if before_ok && after_idx < bytes.len() && bytes[after_idx] == b'.' {
                // Member form. Extract the member identifier.
                let mut end = after_idx + 1;
                while end < bytes.len() && is_word_char(bytes[end]) {
                    end += 1;
                }
                if end > after_idx + 1 && !seen_cols.contains(&abs) {
                    let member = std::str::from_utf8(&bytes[after_idx + 1..end]).unwrap_or("");
                    hits.push(Hit {
                        callee: format!("{name}.{member}"),
                        package: pkg.clone(),
                        category: *cat,
                        col: abs,
                    });
                    seen_cols.insert(abs);
                }
            } else if before_ok
                && after_idx < bytes.len()
                && bytes[after_idx] == b'('
                && !seen_cols.contains(&abs)
            {
                hits.push(Hit {
                    callee: name.clone(),
                    package: pkg.clone(),
                    category: *cat,
                    col: abs,
                });
                seen_cols.insert(abs);
            }
            start = abs + 1;
        }
    }
    hits
}

// ─────────────────────────────────────────────────────────────────────────
// Per-language import parsers — line-oriented, deliberately small. We
// match the common idioms; weird forms (dynamic require, conditional
// imports) fall through.
// ─────────────────────────────────────────────────────────────────────────

/// JS / TS:
/// - `import axios from "axios"`           → axios
/// - `import { get } from "requests"`      → get  (named)
/// - `import { get as gg } from "req"`     → gg   (aliased)
/// - `import * as ax from "axios"`         → ax
/// - `const axios = require("axios")`      → axios
/// - `const { get } = require("redis")`    → get
fn parse_js_imports(src: &str, out: &mut HashMap<String, (Category, String)>) {
    for line in src.lines() {
        let line = line.trim();

        // `import ... from "<src>"` shape
        if let Some(rest) = line.strip_prefix("import ") {
            if let Some((before, source)) = split_import_from_source(rest) {
                if let Some(cat) = classify_source(source) {
                    for b in extract_js_bindings(before) {
                        out.insert(b, (cat, source.to_string()));
                    }
                }
            }
        }

        // `const NAME = require("<src>")` / `const { ... } = require("<src>")`
        if let Some(source) = extract_require_source(line) {
            if let Some(cat) = classify_source(source) {
                for b in extract_js_require_bindings(line) {
                    out.insert(b, (cat, source.to_string()));
                }
            }
        }
    }
}

fn split_import_from_source(rest: &str) -> Option<(&str, &str)> {
    let from_idx = rest.rfind(" from ")?;
    let before = rest[..from_idx].trim();
    let after = rest[from_idx + 6..].trim();
    let after = after.trim_end_matches(';').trim();
    let after = after
        .strip_prefix('"')
        .or_else(|| after.strip_prefix('\''))?;
    let after = after
        .strip_suffix('"')
        .or_else(|| after.strip_suffix('\''))?;
    Some((before, after))
}

fn extract_js_bindings(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let spec = spec.trim();
    // `* as ax`
    if let Some(rest) = spec.strip_prefix("* as ") {
        out.push(rest.trim().to_string());
        return out;
    }
    // Default + named: `axios, { get }` or just default `axios`
    let mut work = spec.to_string();
    if let Some(open) = work.find('{') {
        if let Some(close) = work.find('}') {
            let names = &work[open + 1..close];
            for n in names.split(',') {
                let n = n.trim();
                if n.is_empty() {
                    continue;
                }
                // `get as gg` → take `gg`; plain `get` → take `get`
                let bind = match n.rsplit_once(" as ") {
                    Some((_, alias)) => alias.trim(),
                    None => n,
                };
                if !bind.is_empty() {
                    out.push(bind.to_string());
                }
            }
            work = format!("{}{}", &work[..open], &work[close + 1..])
                .trim_end_matches(',')
                .to_string();
        }
    }
    let default = work.trim().trim_end_matches(',').trim();
    if !default.is_empty() && !default.starts_with('{') {
        out.push(default.to_string());
    }
    out
}

fn extract_require_source(line: &str) -> Option<&str> {
    let idx = line.find("require(")?;
    let after = &line[idx + 8..];
    let after = after
        .strip_prefix('"')
        .or_else(|| after.strip_prefix('\''))?;
    let end = after.find('"').or_else(|| after.find('\''))?;
    Some(&after[..end])
}

fn extract_js_require_bindings(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let trimmed = line.trim();
    // Strip any leading `const|let|var ` keyword
    let body = trimmed
        .strip_prefix("const ")
        .or_else(|| trimmed.strip_prefix("let "))
        .or_else(|| trimmed.strip_prefix("var "))
        .unwrap_or(trimmed);
    let Some(eq) = body.find('=') else {
        return out;
    };
    let lhs = body[..eq].trim();
    if let Some(open) = lhs.find('{') {
        if let Some(close) = lhs.find('}') {
            for n in lhs[open + 1..close].split(',') {
                let bind = match n.trim().rsplit_once(':') {
                    Some((_, alias)) => alias.trim(),
                    None => n.trim(),
                };
                if !bind.is_empty() {
                    out.push(bind.to_string());
                }
            }
            return out;
        }
    }
    if !lhs.is_empty() {
        out.push(lhs.to_string());
    }
    out
}

/// Python:
/// - `import requests`              → requests
/// - `import requests as r`         → r
/// - `from requests import get`     → get
/// - `from requests import get as g`→ g
fn parse_python_imports(src: &str, out: &mut HashMap<String, (Category, String)>) {
    for line in src.lines() {
        let line = line.trim();

        if let Some(rest) = line.strip_prefix("from ") {
            let Some((source, after)) = rest.split_once(" import ") else {
                continue;
            };
            let source = source.trim();
            let Some(cat) = classify_source(source) else {
                continue;
            };
            for n in after.split(',') {
                let n = n.trim().trim_end_matches(';');
                if n.is_empty() {
                    continue;
                }
                let bind = match n.rsplit_once(" as ") {
                    Some((_, alias)) => alias.trim(),
                    None => n,
                };
                if !bind.is_empty() {
                    out.insert(bind.to_string(), (cat, source.to_string()));
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("import ") {
            let rest = rest.trim_end_matches(';').trim();
            for n in rest.split(',') {
                let n = n.trim();
                if n.is_empty() {
                    continue;
                }
                let (source, bind) = match n.rsplit_once(" as ") {
                    Some((src, alias)) => (src.trim(), alias.trim()),
                    None => (n, n),
                };
                if let Some(cat) = classify_source(source) {
                    out.insert(bind.to_string(), (cat, source.to_string()));
                }
            }
        }
    }
}

/// Go: `import "net/http"` or `import http "net/http"` or import blocks.
fn parse_go_imports(src: &str, out: &mut HashMap<String, (Category, String)>) {
    let mut in_block = false;
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with("import (") {
            in_block = true;
            continue;
        }
        if in_block {
            if line == ")" {
                in_block = false;
                continue;
            }
            handle_go_import_line(line, out);
        } else if let Some(rest) = line.strip_prefix("import ") {
            handle_go_import_line(rest.trim(), out);
        }
    }
}

fn handle_go_import_line(s: &str, out: &mut HashMap<String, (Category, String)>) {
    let s = s.trim_end_matches(';').trim();
    let (alias, path) = if let Some(idx) = s.find('"') {
        let alias_part = s[..idx].trim();
        let path = s[idx..]
            .trim()
            .trim_start_matches('"')
            .split('"')
            .next()
            .unwrap_or("");
        (alias_part, path)
    } else {
        return;
    };
    let Some(cat) = classify_source(path) else {
        return;
    };
    let bind = if alias.is_empty() {
        path.rsplit('/').next().unwrap_or(path).to_string()
    } else {
        alias.to_string()
    };
    out.insert(bind, (cat, path.to_string()));
}

/// Rust:
/// - `use reqwest;`                    → reqwest
/// - `use reqwest::Client;`            → Client
/// - `use reqwest::Client as C;`       → C
/// - `use reqwest::{get, post};`       → get, post
fn parse_rust_imports(src: &str, out: &mut HashMap<String, (Category, String)>) {
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("use ") else {
            continue;
        };
        let body = rest.trim_end_matches(';').trim();
        let (crate_name, after) = match body.split_once("::") {
            Some((c, a)) => (c, a),
            None => (body, ""),
        };
        let Some(cat) = classify_source(crate_name) else {
            continue;
        };
        if after.is_empty() {
            out.insert(crate_name.to_string(), (cat, crate_name.to_string()));
            continue;
        }
        // `Client` / `Client as C` / `{a, b as bb, c}`
        if let Some(brace_start) = after.find('{') {
            if let Some(brace_end) = after.find('}') {
                for n in after[brace_start + 1..brace_end].split(',') {
                    let n = n.trim();
                    if n.is_empty() {
                        continue;
                    }
                    let bind = match n.rsplit_once(" as ") {
                        Some((_, alias)) => alias.trim(),
                        None => n.rsplit("::").next().unwrap_or(n).trim(),
                    };
                    out.insert(bind.to_string(), (cat, crate_name.to_string()));
                }
                continue;
            }
        }
        let bind = match after.rsplit_once(" as ") {
            Some((_, alias)) => alias.trim(),
            None => after.rsplit("::").next().unwrap_or(after).trim(),
        };
        out.insert(bind.to_string(), (cat, crate_name.to_string()));
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings(path: &str, src: &str) -> HashMap<String, (Category, String)> {
        collect_tool_bindings(path, src)
    }

    #[test]
    fn js_default_import_creates_binding() {
        let b = bindings("a.ts", "import axios from \"axios\";\n");
        assert_eq!(b.get("axios").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn js_default_alias_via_rename_imports() {
        let b = bindings("a.ts", "import req from \"axios\";\n");
        assert_eq!(
            b.get("req").map(|(c, p)| (*c, p.as_str())),
            Some((Category::Http, "axios"))
        );
    }

    #[test]
    fn js_named_import_with_alias() {
        let b = bindings("a.ts", "import { get as gg } from \"got\";\n");
        assert_eq!(b.get("gg").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn js_star_import_aliases_namespace() {
        let b = bindings("a.ts", "import * as ax from \"axios\";\n");
        assert_eq!(b.get("ax").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn js_require_default() {
        let b = bindings("a.js", "const axios = require(\"axios\");");
        assert_eq!(b.get("axios").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn js_require_destructure() {
        let b = bindings("a.js", "const { get } = require(\"axios\");");
        assert_eq!(b.get("get").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn js_subpath_import_classifies_via_prefix() {
        let b = bindings("a.ts", "import { defaults } from \"axios/lib\";");
        assert_eq!(
            b.get("defaults").map(|(c, p)| (*c, p.as_str())),
            Some((Category::Http, "axios/lib"))
        );
    }

    #[test]
    fn python_from_import_with_alias() {
        let b = bindings("a.py", "from requests import get as g\n");
        assert_eq!(b.get("g").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn python_import_module_with_alias() {
        let b = bindings("a.py", "import requests as r\n");
        assert_eq!(
            b.get("r").map(|(c, p)| (*c, p.as_str())),
            Some((Category::Http, "requests"))
        );
    }

    #[test]
    fn go_import_with_explicit_alias() {
        let b = bindings("a.go", "import h \"net/http\"\n");
        assert_eq!(b.get("h").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn rust_use_path_uses_tail_as_binding() {
        let b = bindings("a.rs", "use reqwest::Client;\n");
        assert_eq!(b.get("Client").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn rust_use_group_with_alias() {
        let b = bindings("a.rs", "use reqwest::{get, post as p};\n");
        assert_eq!(b.get("get").map(|(c, _)| *c), Some(Category::Http));
        assert_eq!(b.get("p").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn global_fetch_always_present() {
        let b = bindings("a.ts", "// no imports\n");
        assert_eq!(b.get("fetch").map(|(c, _)| *c), Some(Category::Http));
    }

    #[test]
    fn fetch_is_not_http_in_rust_files() {
        // Regression: `fetch` is a JS/browser/Deno global. In Rust it's
        // domain vocab (graph node fetch, HashMap accessor, …) and must
        // not be tagged HTTP without an explicit reqwest/surf/ureq import.
        let b = bindings("a.rs", "// no imports\n");
        assert!(
            b.get("fetch").is_none(),
            "fetch leaked into Rust bindings: {b:?}"
        );
        assert!(
            b.get("XMLHttpRequest").is_none(),
            "XMLHttpRequest leaked into Rust bindings: {b:?}",
        );
    }

    #[test]
    fn fetch_is_not_http_in_python_files() {
        let b = bindings("a.py", "# no imports\n");
        assert!(
            b.get("fetch").is_none(),
            "fetch leaked into Python bindings: {b:?}"
        );
    }

    #[test]
    fn fetch_is_not_http_in_go_files() {
        let b = bindings("a.go", "// no imports\n");
        assert!(
            b.get("fetch").is_none(),
            "fetch leaked into Go bindings: {b:?}"
        );
    }

    #[test]
    fn find_uses_detects_any_method_form() {
        let mut b = HashMap::new();
        b.insert("axios".to_string(), (Category::Http, "axios".to_string()));
        let hits = find_binding_uses("await axios.head(url)", &b);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].callee, "axios.head");
    }

    #[test]
    fn find_uses_detects_bare_call() {
        let mut b = HashMap::new();
        b.insert("axios".to_string(), (Category::Http, "axios".to_string()));
        let hits = find_binding_uses("return axios(opts)", &b);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].callee, "axios");
    }

    #[test]
    fn find_uses_word_boundary_blocks_prefix_match() {
        let mut b = HashMap::new();
        b.insert("axios".to_string(), (Category::Http, "axios".to_string()));
        let hits = find_binding_uses("const axiosFoo = doSomething()", &b);
        assert!(hits.is_empty());
    }

    #[test]
    fn classify_source_exact_and_subpath() {
        assert_eq!(classify_source("axios"), Some(Category::Http));
        assert_eq!(classify_source("axios/lib/foo"), Some(Category::Http));
        assert_eq!(classify_source("notaxios"), None);
        assert_eq!(classify_source("redis"), Some(Category::Redis));
    }
}
