//! T7-7 — parity gate: incremental `reanalyze_files` must produce per-file
//! node sets identical to a direct fresh parse of the same file content.
//!
//! The invariant: both `reanalyze_files(repo, DiffScope::All, &[path])`
//! and `make_pipeline().analyze([(abs, rel)])` call the same underlying
//! `LanguageProvider::parse_file` — so their outputs must be set-equal.
//! Any divergence means the incremental path is applying some filter or
//! transformation that the full-build path does not, which is a real bug.
//!
//! Property-based strategy: pick a random file from the 14-lang polyglot
//! fixture, apply one of 5 edit types, then assert parity.
//! Smoke variant: 20 cases, <1 s. Heavy variant: 200 cases, `#[ignore]`.

use ecp_cli::git::DiffScope;
use ecp_cli::reanalyze::{pipeline, reanalyze_files};
use ecp_core::graph::NodeKind;
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/incremental_parity"
);

// ── fixture discovery ────────────────────────────────────────────────────────

fn fixture_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for lang_entry in std::fs::read_dir(FIXTURE_DIR).expect("fixture dir must exist") {
        let lang_dir = lang_entry.unwrap().path();
        if !lang_dir.is_dir() {
            continue;
        }
        for file_entry in std::fs::read_dir(&lang_dir).unwrap() {
            let p = file_entry.unwrap().path();
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

// ── edit types ───────────────────────────────────────────────────────────────

/// Five edit types from the T7-7 roadmap.
#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
enum Edit {
    /// Replace the first function/method body with a trivial alternative.
    BodyEdit,
    /// Append a new top-level symbol at the end of the file.
    AddSymbol,
    /// Remove the first recognized function/method definition.
    DeleteSymbol,
    /// Prepend a harmless comment line that looks like an import/use.
    AddImport,
    /// Write identical bytes back (mtime-only bump; content unchanged).
    NoopTouch,
}

fn edit_idx_to_variant(i: usize) -> Edit {
    match i % 5 {
        0 => Edit::BodyEdit,
        1 => Edit::AddSymbol,
        2 => Edit::DeleteSymbol,
        3 => Edit::AddImport,
        _ => Edit::NoopTouch,
    }
}

/// Extension → comment prefix for AddImport / AddSymbol stubs.
fn comment_prefix(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("py" | "rb") => "#",
        _ => "//",
    }
}

/// Apply `edit` to `target` (absolute path) in a temp repo copy.
fn apply_edit(target: &Path, edit: &Edit) {
    let content = std::fs::read_to_string(target).unwrap_or_default();
    let pfx = comment_prefix(target);
    let new_content = match edit {
        Edit::BodyEdit => {
            // Find the first `{` after a function-keyword line and replace the
            // body up to the matching `}`. For languages without braces, just
            // append a comment — the parse result changes trivially.
            if let Some(open) = content.find('{') {
                // Count nesting depth to find the matching close.
                let bytes = content.as_bytes();
                let mut depth = 0usize;
                let mut close = open;
                for (i, &b) in bytes[open..].iter().enumerate() {
                    match b {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                close = open + i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                format!(
                    "{} {pfx} body-edit\n{}",
                    &content[..open + 1],
                    &content[close..]
                )
            } else {
                format!("{content}\n{pfx} body-edit\n")
            }
        }
        Edit::AddSymbol => {
            let stub = match target.extension().and_then(|e| e.to_str()) {
                Some("py") => "\ndef _parity_added(): pass\n",
                Some("rb") => "\ndef _parity_added\nend\n",
                Some("go") => "\nfunc _parityAdded() {}\n",
                Some("java" | "kt") => "\nvoid _parityAdded() {}\n",
                _ => "\nfn _parity_added() {}\n",
            };
            format!("{content}{stub}")
        }
        Edit::DeleteSymbol => {
            // Remove the first non-empty line that starts a function / method.
            let keywords: &[&str] = &["fn ", "func ", "def ", "fun ", "void ", "public "];
            let mut lines: Vec<&str> = content.lines().collect();
            if let Some(idx) = lines.iter().position(|l| {
                let t = l.trim();
                !t.is_empty() && keywords.iter().any(|kw| t.contains(kw))
            }) {
                lines.remove(idx);
            }
            lines.join("\n") + "\n"
        }
        Edit::AddImport => {
            let stub = match target.extension().and_then(|e| e.to_str()) {
                Some("py") => "# added import\n",
                Some("rb") => "# added require\n",
                Some("go") => "// added import\n",
                Some("java" | "kt") => "// added import\n",
                _ => "// added use\n",
            };
            format!("{stub}{content}")
        }
        Edit::NoopTouch => {
            // Identical bytes — only mtime changes.
            content
        }
    };
    std::fs::write(target, new_content).expect("write edit");
}

// ── snapshot ─────────────────────────────────────────────────────────────────

/// Stable fingerprint of a single file's parse output.
///
/// Nodes are keyed by `(name, kind_str, owner_class)` — the minimal tuple
/// that distinguishes sibling symbols. We do NOT include span because edits
/// intentionally shift line numbers; the invariant is symbol identity, not
/// position stability.
#[derive(Debug, PartialEq, Eq)]
struct FileSnapshot {
    nodes: BTreeSet<(String, String, String)>,
    /// xxh3_64 over sorted `kind_str \0 name` pairs — T7-6 guard (c).
    bucket_fingerprint: u64,
}

fn snapshot_from_local_graph(nodes: &[ecp_core::analyzer::types::RawNode]) -> FileSnapshot {
    let node_set: BTreeSet<(String, String, String)> = nodes
        .iter()
        .filter(|n| !matches!(n.kind, NodeKind::Import | NodeKind::File))
        .map(|n| {
            (
                n.name.clone(),
                n.kind.as_str().to_owned(),
                n.owner_class.clone().unwrap_or_default(),
            )
        })
        .collect();

    // Bucket fingerprint: xxh3_64 over sorted (kind \0 name) pairs.
    let mut sorted: Vec<String> = node_set
        .iter()
        .map(|(name, kind, _)| format!("{kind}\0{name}"))
        .collect();
    sorted.sort();
    let joined = sorted.join("\x01");
    let fp = xxhash_rust::xxh3::xxh3_64(joined.as_bytes());

    FileSnapshot {
        nodes: node_set,
        bucket_fingerprint: fp,
    }
}

// ── parse helpers ────────────────────────────────────────────────────────────

/// Fresh parse via the direct pipeline path (equivalent to full-build).
///
/// Uses the process-wide cached `pipeline()` accessor (same one
/// `reanalyze_files` consumes) instead of `make_pipeline()` — rebuilding
/// all 21 tree-sitter providers per parse cost ~0.6 s/call, which at
/// ~60 parses per smoke run dominated wall time (~37 s of pure rebuild
/// work). Sharing the cached pipeline is parity-safe because
/// `analyze` is stateless w.r.t. the pipeline instance (no cache is
/// populated in this code path).
fn parse_direct(abs: &Path, rel: &Path) -> FileSnapshot {
    let graphs = pipeline().analyze(vec![(abs.to_path_buf(), rel.to_path_buf())]);
    let nodes = graphs
        .into_iter()
        .next()
        .map(|g| g.nodes)
        .unwrap_or_default();
    snapshot_from_local_graph(&nodes)
}

/// Parse via the incremental `reanalyze_files` path.
fn parse_incremental(repo: &Path, rel: &str) -> FileSnapshot {
    let graphs = reanalyze_files(repo, &DiffScope::All, &[rel.to_owned()]);
    let nodes = graphs
        .into_iter()
        .next()
        .map(|g| g.nodes)
        .unwrap_or_default();
    snapshot_from_local_graph(&nodes)
}

// ── proptest strategy ────────────────────────────────────────────────────────

fn edit_strategy() -> impl Strategy<Value = Vec<(usize, usize)>> {
    // Each element: (file_idx, edit_type_idx)
    prop::collection::vec((any::<usize>(), 0..5usize), 1..=5)
}

// ── parity core ──────────────────────────────────────────────────────────────

fn run_parity_cases(cases: u32) {
    let files = fixture_files();
    assert!(!files.is_empty(), "fixture dir must contain files");
    let n = files.len();

    let tmp = tempfile::tempdir().expect("tempdir");

    // Mirror the fixture tree into the tempdir so edits are isolated.
    for src in &files {
        // Reconstruct relative path from FIXTURE_DIR.
        let rel = src.strip_prefix(FIXTURE_DIR).unwrap();
        let dst = tmp.path().join(rel);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(src, &dst).unwrap();
    }

    let config = ProptestConfig {
        cases,
        ..ProptestConfig::default()
    };

    proptest!(config, |(edit_seq in edit_strategy())| {
        // Reset the tempdir to a clean fixture copy for each proptest case.
        for src in &files {
            let rel = src.strip_prefix(FIXTURE_DIR).unwrap();
            let dst = tmp.path().join(rel);
            std::fs::copy(src, &dst).unwrap();
        }

        for (file_selector, edit_type_idx) in &edit_seq {
            let abs = tmp.path().join(
                files[file_selector % n]
                    .strip_prefix(FIXTURE_DIR)
                    .unwrap(),
            );
            let rel = files[file_selector % n]
                .strip_prefix(FIXTURE_DIR)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let edit = edit_idx_to_variant(*edit_type_idx);

            apply_edit(&abs, &edit);

            let snap_direct = parse_direct(&abs, Path::new(&rel));
            let snap_inc = parse_incremental(tmp.path(), &rel);

            prop_assert_eq!(
                &snap_inc.nodes,
                &snap_direct.nodes,
                "node mismatch after {:?} on {}: incremental had {:?}, direct had {:?}",
                edit,
                rel,
                &snap_inc.nodes,
                &snap_direct.nodes
            );
            prop_assert_eq!(
                snap_inc.bucket_fingerprint,
                snap_direct.bucket_fingerprint,
                "bucket fingerprint mismatch after {:?} on {}",
                edit,
                rel
            );
        }
    });
}

// ── tests ────────────────────────────────────────────────────────────────────

/// Smoke: 20 proptest cases, <1 s on dev machine. Gates every PR.
#[test]
fn parity_gate_smoke() {
    run_parity_cases(20);
}

/// Heavy: 200 proptest cases. Opt-in for nightly via `--ignored`.
#[test]
#[ignore]
fn parity_gate_heavy() {
    run_parity_cases(200);
}
