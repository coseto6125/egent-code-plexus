//! Source-based value dependency queries. No graph load or reindex is required.

mod sources;
pub use sources::{load_sources, load_sources_at_ref, relative_path, supported_path};

use crate::output::{emit, OutputFormat};
use clap::{Args, ValueEnum};
use ecp_analyzer::flow::{analyze, Budgets, Direction, FlowRequest, Subject};
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FlowSubject {
    Binding,
    Value,
    Return,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FlowDirection {
    Forward,
    Backward,
}

#[derive(Args, Debug, Clone)]
pub struct FlowArgs {
    /// Source path relative to --repo, or an absolute path inside that directory.
    #[arg(long)]
    pub file: String,
    /// One-based source line.
    #[arg(long, value_parser = positive)]
    pub line: usize,
    /// One-based UTF-8 byte column. Use the exact expression or binding position.
    #[arg(long, default_value_t = 1, value_parser = positive)]
    pub column: usize,
    /// Select a variable binding, expression value, or enclosing function return.
    #[arg(long, value_enum, default_value = "value")]
    pub subject: FlowSubject,
    /// Follow consumers forward or value origins backward.
    #[arg(long, value_enum, default_value = "forward")]
    pub direction: FlowDirection,
    /// Repository directory. No ecp index is required.
    #[arg(long)]
    pub repo: Option<String>,
    /// JSON object mapping repository-relative paths to unsaved source text.
    #[arg(long)]
    pub overlay: Option<PathBuf>,
    /// Maximum dependency nodes. Exhaustion is reported as truncated.
    #[arg(long, default_value_t = 50_000, value_parser = positive)]
    pub max_nodes: usize,
    /// Maximum call expansion depth. Remaining calls become explicit boundaries.
    #[arg(long, default_value_t = 16, value_parser = positive)]
    pub max_call_depth: usize,
    /// Maximum analysis steps. Exhaustion is reported as truncated.
    #[arg(long, default_value_t = 200_000, value_parser = positive)]
    pub max_steps: usize,
    /// Bypass the latest-result cache. Source hashes always invalidate changed results.
    #[arg(long)]
    pub no_cache: bool,
    #[arg(long)]
    pub format: Option<String>,
}

fn positive(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "expected a positive integer".into())
}

pub fn run(args: FlowArgs) -> Result<(), EcpError> {
    let payload = build_payload(&args)?;
    emit(&payload, OutputFormat::parse(args.format.as_deref()))
}

pub fn build_payload(args: &FlowArgs) -> Result<Value, EcpError> {
    let repo = dunce::canonicalize(args.repo.as_deref().unwrap_or("."))?;
    if !repo.is_dir() {
        return Err(EcpError::InvalidArgument(
            "--repo must be a directory".into(),
        ));
    }
    let file = sources::relative_path(&repo, Path::new(&args.file))?;
    let sources = load_sources(&repo, args.overlay.as_deref())?;
    let request = FlowRequest {
        file: file.clone(),
        line: args.line,
        column: args.column,
        subject: match args.subject {
            FlowSubject::Binding => Subject::Binding,
            FlowSubject::Value => Subject::Value,
            FlowSubject::Return => Subject::Return,
        },
        direction: match args.direction {
            FlowDirection::Forward => Direction::Forward,
            FlowDirection::Backward => Direction::Backward,
        },
        budgets: Budgets {
            max_nodes: args.max_nodes,
            max_call_depth: args.max_call_depth,
            max_steps: args.max_steps,
        },
    };
    // One cache entry per repository bounds disk growth. Include all snapshots,
    // including dependencies and overlays, plus parser and query semantics.
    let mut hash = xxhash_rust::xxh3::Xxh3::new();
    hash.update(ecp_analyzer::PARSER_FINGERPRINT.as_bytes());
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    hash.update(include_bytes!("../../../../Cargo.lock"));
    hash.update(include_bytes!("flow.rs"));
    hash.update(include_bytes!("flow/sources.rs"));
    hash.update(&[u8::from(args.overlay.is_some())]);
    hash.update(
        format!(
            "flow-v1:{file}:{}:{}:{:?}:{:?}:{}:{}:{}",
            args.line,
            args.column,
            args.subject,
            args.direction,
            args.max_nodes,
            args.max_call_depth,
            args.max_steps
        )
        .as_bytes(),
    );
    for source in &sources {
        hash.update(&(source.path.len() as u64).to_le_bytes());
        hash.update(source.path.as_bytes());
        hash.update(&(source.source.len() as u64).to_le_bytes());
        hash.update(source.source.as_bytes());
    }
    let key = format!("{:016x}", hash.digest());
    let cache_dir = (!args.no_cache).then(|| ecp_core::registry::resolve_home_ecp().join("flow"));
    let cache_file = cache_dir.as_ref().map(|dir| {
        dir.join(format!(
            "{:016x}.json",
            ecp_core::uid::xxh3_64_bytes(repo.to_string_lossy().as_bytes())
        ))
    });
    if let Some(cached) = cache_file
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    {
        if cached["key"].as_str() == Some(key.as_str()) && cached["report"].is_object() {
            let mut report = cached["report"].clone();
            report["cache_hit"] = json!(true);
            return Ok(report);
        }
    }
    let result = analyze(&sources, &request).map_err(EcpError::InvalidArgument)?;
    let mut report =
        serde_json::to_value(result).map_err(|e| EcpError::InvalidArgument(e.to_string()))?;
    report["cache_hit"] = json!(false);
    report["source_kind"] = json!(if args.overlay.is_some() {
        "overlay"
    } else {
        "working_tree"
    });
    report["requires_verification"] = json!(true);
    report["source_scope"] = json!({
        "files": sources.len(),
        "ignored_files": "untracked files excluded by ignore rules; tracked sources included",
        "excluded_directories": [".git", ".ecp", ".claude", "node_modules", "target", ".venv", "vendor", "__pycache__"],
        "index_required": false,
    });
    if let (Some(dir), Some(path)) = (cache_dir, cache_file) {
        // Cache failures never change query correctness. Persist atomically so
        // simultaneous invocations cannot expose a partial JSON document.
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(mut temp) = tempfile::NamedTempFile::new_in(&dir) {
                if serde_json::to_writer(temp.as_file_mut(), &json!({"key":key,"report":report}))
                    .is_ok()
                {
                    let _ = temp.persist(path);
                }
            }
        }
    }
    Ok(report)
}
