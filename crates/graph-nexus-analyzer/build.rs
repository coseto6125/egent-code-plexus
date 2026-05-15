//! Build script for `graph-nexus-analyzer`.
//!
//! Computes a SHA256 fingerprint of all parser-related source files at
//! compile time and emits it as the `GRAPH_NEXUS_PARSER_FINGERPRINT`
//! environment variable. The runtime reads it via `env!()` to invalidate
//! the incremental-analysis cache whenever parser logic changes.
//!
//! Files included in the fingerprint (under `src/`):
//!   * every `parser.rs`
//!   * every `queries.scm`
//!   * top-level `calls.rs`, `framework_helpers.rs`, `route_detector.rs`

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

// Top-level helpers at `src/*.rs` whose content affects parse output.
// `framework_confidence.rs` carries the confidence constants written into
// `RawFrameworkRef.confidence` — a change there reshapes cached graphs even
// without touching parser.rs.
// `embeddings.rs`, `ast_test.rs`, `lib.rs` are excluded — embedding code is
// runtime-only, ast_test is a dev helper, and lib.rs is just re-exports.
const TOP_LEVEL_FILES: &[&str] = &[
    "calls.rs",
    "framework_helpers.rs",
    "framework_confidence.rs",
    "route_detector.rs",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_dir = manifest_dir.join("src");

    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(&src_dir, &src_dir, &mut files);

    // Deterministic order — without sorting, recursion order is filesystem-
    // dependent and the fingerprint would not be reproducible across machines.
    files.sort();

    let mut hasher = Sha256::new();
    for path in &files {
        // Hash a relative-path header so reordering/renaming changes the digest
        // even when content bytes are unchanged.
        let rel = path.strip_prefix(&manifest_dir).unwrap_or(path);
        let rel_str = rel.to_string_lossy();
        hasher.update(rel_str.as_bytes());
        hasher.update([0u8]);

        let bytes = fs::read(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        hasher.update(&bytes);
        hasher.update([0u8]);

        println!("cargo:rerun-if-changed={}", path.display());
    }

    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();

    println!("cargo:rustc-env=GRAPH_NEXUS_PARSER_FINGERPRINT={hex}");
    // Visible in `cargo build -vv` / when this script's stdout is shown.
    println!("cargo:warning=graph-nexus-analyzer parser fingerprint: {hex}");
}

/// Recursively collect files under `dir` whose names match the parser set.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            collect_files(root, &path, out);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        // Language subdir files (`src/<lang>/...`): any `.rs` plus
        // `queries.scm`. `parser.rs` alone misses helpers like
        // `python/receiver_types.rs` and `python/identifier_finder.rs`
        // which feed into the parser's output and so must invalidate the
        // cache on change. Including `mod.rs` is harmless — it usually
        // just declares submodules so its hash co-varies with theirs.
        let in_lang_subdir = path.parent() != Some(root);
        let is_lang_source = in_lang_subdir && name.ends_with(".rs");
        let is_queries = name == "queries.scm";
        let is_top_level_helper =
            path.parent() == Some(root) && TOP_LEVEL_FILES.contains(&name);

        if is_lang_source || is_queries || is_top_level_helper {
            out.push(path);
        }
    }
}
