//! `gnx tool_map` — scan source files for calls to known external clients
//! (HTTP / DB / Redis / queue) and group them by category.
//!
//! Implementation note: external-client identifiers (`axios.get`,
//! `requests.post`, …) are not graph nodes because their definitions live
//! in third-party packages outside the analyzed source tree. Walking the
//! graph's CALLS edges therefore misses them entirely. Instead this
//! command iterates the source files indexed in `graph.bin` and scans
//! each for catalog-matching identifier substrings with word-boundary
//! checks. Test files (`graph.files` `category == Test`) are skipped.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::graph::ArchivedFileCategory;
use graph_nexus_core::GnxError;
use std::collections::HashMap;
use std::path::PathBuf;

const HTTP_CLIENTS: &[&str] = &[
    // JS / TS
    "fetch",
    "axios.get",
    "axios.post",
    "axios.put",
    "axios.delete",
    "axios",
    "got",
    "ky",
    "node-fetch",
    // Python
    "requests.get",
    "requests.post",
    "requests.put",
    "requests.delete",
    "httpx.get",
    "httpx.post",
    "aiohttp.get",
    "aiohttp.post",
    "urllib.request.urlopen",
    // Go
    "http.Get",
    "http.Post",
    "http.Do",
    // Rust
    "reqwest::get",
    "reqwest::post",
    "Client::get",
    "Client::post",
];

const DB_CLIENTS: &[&str] = &[
    // SQL
    "psycopg.connect",
    "psycopg2.connect",
    "sqlalchemy.create_engine",
    "pg.Client",
    "mysql.createConnection",
    "sqlx::query",
    // ORM
    "mongoose.connect",
    "prisma",
    "drizzle",
    "typeorm",
    // KV / Doc
    "MongoClient",
];

const REDIS_CLIENTS: &[&str] = &[
    "redis.Redis",
    "redis.createClient",
    "ioredis.Redis",
    "redis::Client",
    "Redis::open",
];

const QUEUE_CLIENTS: &[&str] = &[
    "celery.Task",
    "bull.Queue",
    "amqplib.connect",
    "kafka.Producer",
    "sqs.SendMessage",
    "boto3.client",
];

/// Category tag for a matched callee. Kept stable as the JSON key under
/// `totals` / `calls` in the output.
#[derive(Debug, Clone, Copy)]
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

#[derive(Args, Debug, Clone)]
pub struct ToolMapArgs {
    /// Filter to a specific category. Comma-separated. Valid:
    /// `http`, `db`, `redis`, `queue`. Empty = all.
    #[arg(long, alias = "categories")]
    pub category: Option<String>,

    #[arg(long)]
    pub repo: Option<String>,

    #[arg(long, default_value = "toon")]
    pub format: Option<String>,
}

pub fn run(args: ToolMapArgs, engine: &Engine) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let format = OutputFormat::parse(args.format.as_deref());

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
            Some(list) => list.iter().any(|f| f.as_key() == c.as_key()),
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

    // Source-tree scan: external-client identifiers like `axios.get` aren't
    // graph nodes (no definition in this repo) so walking CALLS edges would
    // miss every match. Instead read each non-test file the analyzer
    // indexed, scan line-by-line with word-boundary checks against the
    // catalog. `args.repo` lets the user override the project root for
    // cases where the binary runs outside the indexed worktree.
    let repo_root: PathBuf = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    for file in graph.files.iter() {
        if matches!(file.category, ArchivedFileCategory::Test) {
            continue;
        }
        let rel = file.path.resolve(&graph.string_pool);
        let abs = repo_root.join(rel);
        let Ok(src) = std::fs::read_to_string(&abs) else {
            continue;
        };
        scan_file(rel, &src, |needle, cat, line, col| {
            if !category_allowed(cat) {
                return;
            }
            let entry = serde_json::json!({
                "callee": needle,
                "filePath": rel,
                "line": line,
                "col": col,
            });
            let key = cat.as_key();
            calls.get_mut(key).expect("bucket pre-seeded").push(entry);
            *totals.get_mut(key).expect("bucket pre-seeded") += 1;
        });
    }

    let result = serde_json::json!({
        "status": "success",
        "totals": totals,
        "calls": calls,
    });

    emit(&result, format)
}

/// Scan one file's source text against every catalog entry. For each
/// match the closure is invoked with `(needle, category, line_idx_1based,
/// col_idx_1based)`. Word-boundary check: char immediately before/after
/// the match must NOT be `[A-Za-z0-9_]` so `axios.getRoute` doesn't match
/// `axios.get`. Catalog needles themselves may contain `.` / `::` — those
/// are allowed within the match window, the boundary check applies only
/// to the surrounding chars.
fn scan_file<F: FnMut(&'static str, Category, usize, usize)>(_path: &str, src: &str, mut visit: F) {
    let catalogs: &[(&[&'static str], Category)] = &[
        (HTTP_CLIENTS, Category::Http),
        (DB_CLIENTS, Category::Db),
        (REDIS_CLIENTS, Category::Redis),
        (QUEUE_CLIENTS, Category::Queue),
    ];

    for (line_idx, line) in src.lines().enumerate() {
        let bytes = line.as_bytes();
        for (catalog, cat) in catalogs {
            for &needle in *catalog {
                let needle_bytes = needle.as_bytes();
                if needle_bytes.is_empty() {
                    continue;
                }
                let mut start = 0usize;
                while let Some(pos_rel) = find_subslice(&bytes[start..], needle_bytes) {
                    let abs = start + pos_rel;
                    let before_ok = abs == 0 || !is_word_char(bytes[abs - 1]);
                    let after = abs + needle_bytes.len();
                    let after_ok = after == bytes.len() || !is_word_char(bytes[after]);
                    if before_ok && after_ok {
                        visit(needle, *cat, line_idx + 1, abs + 1);
                    }
                    start = abs + 1;
                }
            }
        }
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
