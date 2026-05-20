//! `Imports` edge emission — `File` node → imported target.
//!
//! Walks each `LocalGraph.imports` (already populated by the 14 language
//! parsers), resolves each `RawImport` via the same `Resolver` used for
//! Calls/Accesses, and emits one `Imports` edge per
//! `(file_node, resolved_target)` pair.
//!
//! Spec: `docs/superpowers/specs/2026-05-17-imports-edge-emission.md` §2 —
//! resolver miss → don't emit (refuse to produce gitnexus-style cross-language
//! false positives like `.mjs → Path.java`).
//!
//! Resolution strategy — 8 sub-steps in 3 tiers, first-hit-wins per import.
//! Each `if target_file_idx.is_none()` gate keeps later sub-steps reachable
//! only when earlier ones miss; single-hit constraint in suffix lookups
//! defuses cross-language ambiguity.
//!
//! **Tier 1 — named-symbol lookup** (target is the imported symbol node):
//!
//! - **Step 1**: `Resolver::resolve_symbol` with both `Callable` + `Type`
//!   kinds against `RawImport.imported_name`. Covers TS/JS/Python/Java/PHP
//!   where `imported_name` IS the symbol (`from a import foo`).
//!   Confidence = tier-determined: `SameFile=1.0`, `ImportScoped=0.95`,
//!   `QualifierScoped=0.85`, `HeritageScoped=0.85`, `Global=0.7`.
//!
//! - **Step 2**: FQN last-segment retry — if `imported_name` contains `.`
//!   (Kotlin / Java `import com.x.Alpha` surfaces with `imported_name =
//!   "com.x.Alpha"`), retry with `"Alpha"`. Same confidence as Step 1.
//!
//! **Tier 2 — module-style path resolution** (target is `NodeKind::File`).
//! Triggered only when Tier 1 misses. All emit with `confidence = 0.9`
//! and `reason = "post_process:imports:module"`:
//!
//! - **Step 3a**: probe `import.source` as-is via `enumerate_candidates`
//!   (TS `./a`, Python `.foo` already encode relativity).
//! - **Step 3b**: retry with `./` prefix so the resolver's relative branch
//!   joins caller dir (Ruby `require_relative 'alpha'`, Go `import "x/pkg"`,
//!   C `#include "alpha.h"`).
//! - **Step 3c**: basename + suffix match across all indexed files via
//!   pre-built `basename_idx` (C/C++ `#include` where header sits under
//!   `-I include/`).
//! - **Step 3d**: caller-extension + last-segment basename match (Go
//!   `import "modulePath/pkg"` whose module prefix isn't a filesystem
//!   path → match `pkg.<caller_ext>`).
//!
//! **Tier 3 — language-specific path normalization** (target is
//! `NodeKind::File`). Same confidence + reason as Tier 2:
//!
//! - **Step 3e**: Rust `use crate::a::b::Foo` — strip leading `crate::` /
//!   `self::`, take last `::` segment as module-file basename, suffix-match
//!   with caller extension.
//! - **Step 3f**: namespace / module-dir match for C# `using NS;` /
//!   Swift `import Module` where the specifier names a directory
//!   containing the implementation file. Uses `dir_component_idx`.

use crate::resolution::index::ResolveTarget;
use crate::resolution::resolver::Resolver;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;

/// Emit `Imports` edges from each `File` node to the resolved targets of
/// every `RawImport` in that file. Returns the number of edges appended.
pub fn emit_edges(
    local_graphs: &[LocalGraph],
    resolver: &Resolver<'_>,
    file_node_idx: &FxHashMap<String, u32>,
    string_pool: &mut StringPool,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let reason_named = string_pool.add("post_process:imports");
    let reason_module = string_pool.add("post_process:imports:module");
    let mut emitted = 0usize;

    // Pre-pass: build a basename → [(full_path, idx)] index so the
    // Step 3c/3d/3e/3f fallbacks can do O(1) hash lookup + bucket-local
    // suffix filter instead of an O(N) linear scan across `file_node_idx`
    // per import. On `.sample_repo` (14k files) this shrinks Step 3 cost
    // from O(imports × files) ≈ 200M comparisons to O(imports × bucket)
    // where bucket size is typically < 10.
    let mut basename_idx: FxHashMap<&str, Vec<(&str, u32)>> = FxHashMap::default();
    for (path, &idx) in file_node_idx.iter() {
        let basename = std::path::Path::new(path.as_str())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path.as_str());
        basename_idx
            .entry(basename)
            .or_default()
            .push((path.as_str(), idx));
    }
    // Also index path components for Step 3f namespace/module-dir match.
    // Key = directory component name (e.g. "X" from `src/X/Alpha.cs`).
    // Value = Vec<(full_path, idx)>. Same O(1) lookup benefit.
    let mut dir_component_idx: FxHashMap<&str, Vec<(&str, u32)>> = FxHashMap::default();
    for (path, &idx) in file_node_idx.iter() {
        for component in path.split('/') {
            if !component.is_empty() && !component.contains('.') {
                dir_component_idx
                    .entry(component)
                    .or_default()
                    .push((path.as_str(), idx));
            }
        }
    }

    // Parallel per-file: each thread accumulates its own (emitted_count,
    // local_edges) — dedupe key is (source_file_idx, target_file_idx), and
    // source_file_idx is unique per file → no cross-file dedupe collisions,
    // so per-file dedupe is semantically equivalent to the prior global
    // HashSet. Rayon picks num_cpus workers; no hardcoded thread count.
    use rayon::prelude::*;
    let chunk_results: Vec<(usize, Vec<Edge>)> = local_graphs
        .par_iter()
        .map(|local_graph| {
            let mut local_emitted = 0usize;
            let mut local_edges: Vec<Edge> = Vec::new();
            let mut dedupe: FxHashSet<(u32, u32)> = FxHashSet::default();
            let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
            let Some(&source_file_idx) = file_node_idx.get(&path_str) else {
                return (local_emitted, local_edges);
            };
            if local_graph.imports.is_empty() {
                return (local_emitted, local_edges);
            }

            // Reusable buffer for Step 3b's `./` prefix retry; cleared and
            // re-filled per import-miss so we don't allocate on every miss.
            let mut dot_prefix_buf = String::new();
            let emitted = &mut local_emitted;
            let edges_out = &mut local_edges;

            for import in &local_graph.imports {
                let before = *emitted;
                // Step 1: named-symbol lookup.
                *emitted += try_named(
                    resolver,
                    local_graph,
                    &import.imported_name,
                    source_file_idx,
                    reason_named,
                    &mut dedupe,
                    edges_out,
                );

                // Step 2: FQN last-segment retry (Kotlin / Java / PHP qualified imports).
                if *emitted == before && import.imported_name.contains('.') {
                    if let Some(last) = import.imported_name.rsplit('.').next() {
                        if !last.is_empty() && last != import.imported_name {
                            *emitted += try_named(
                                resolver,
                                local_graph,
                                last,
                                source_file_idx,
                                reason_named,
                                &mut dedupe,
                                edges_out,
                            );
                        }
                    }
                }

                // Step 3: module-style fallback (File → File). Strip leading
                // surrounding quotes / angle brackets common in C-family
                // `#include "alpha.h"` / `#include <alpha.h>` source strings.
                if *emitted == before {
                    let cleaned = import
                        .source
                        .trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>');

                    let probe = |spec: &str,
                                 file_node_idx: &FxHashMap<String, u32>|
                     -> Option<u32> {
                        let mut hit: Option<u32> = None;
                        resolver.enumerate_candidates(&local_graph.file_path, spec, |candidate| {
                            // Cow avoids the no-op allocation on Linux/macOS where
                            // candidate paths already use forward slashes (probe
                            // callback fires ~10× per import-miss × 14k files,
                            // so unconditional `String::replace` was ~140k wasted
                            // heap allocations per build).
                            let normalized: Cow<'_, str> = if candidate.contains('\\') {
                                Cow::Owned(candidate.replace('\\', "/"))
                            } else {
                                Cow::Borrowed(candidate)
                            };
                            if let Some(&idx) = file_node_idx.get(normalized.as_ref()) {
                                hit = Some(idx);
                                return false;
                            }
                            true
                        });
                        hit
                    };

                    // Step 3a: probe as-is (TypeScript `./a`, Python `.foo`, etc.
                    // already encode relativity in the specifier).
                    let mut target_file_idx = probe(cleaned, file_node_idx);

                    // Step 3b: if still miss, retry with `./` prefix so the
                    // resolver's relative-resolution branch joins caller dir
                    // (Ruby `require_relative 'alpha'`, Go `import "x/pkg"`,
                    // C `#include "alpha.h"` all surface here — none of them
                    // prepend a `./` but they're all caller-dir-relative in
                    // practice when the target lives in the same indexed tree).
                    if target_file_idx.is_none() && !cleaned.starts_with('.') && !cleaned.is_empty()
                    {
                        dot_prefix_buf.clear();
                        dot_prefix_buf.push_str("./");
                        dot_prefix_buf.push_str(cleaned);
                        target_file_idx = probe(&dot_prefix_buf, file_node_idx);
                    }

                    // Step 3c: basename + suffix match. Handles C/C++
                    // `#include "alpha.hpp"` where the header sits under a
                    // search-path dir. O(1) hash lookup via basename_idx,
                    // then suffix-filter within the small same-basename bucket.
                    if target_file_idx.is_none() && !cleaned.is_empty() {
                        target_file_idx = suffix_match_single(cleaned, &basename_idx);
                    }

                    // Step 3d: caller-extension + last-segment match. Handles
                    // Go `import "modulePath/pkg"` where the specifier carries
                    // a `go.mod` module-name prefix that isn't a filesystem
                    // path; the actual file is `pkg/<anything>.go`. Falls
                    // back to `<last-segment>.<caller-ext>` basename lookup.
                    if target_file_idx.is_none() && !cleaned.is_empty() {
                        if let Some(last) = cleaned.rsplit('/').next() {
                            if let Some(caller_ext) = local_graph.file_path.extension() {
                                let ext = caller_ext.to_string_lossy();
                                let candidate = format!("{}.{}", last, ext);
                                target_file_idx = suffix_match_single(&candidate, &basename_idx);
                            }
                        }
                    }

                    // Step 3e: Rust `use` path resolution. The Rust parser
                    // stamps `use crate::a::b::Foo` as `source = "crate::a::b"`
                    // (parent module path) + `imported_name = "Foo"` (already
                    // split off). The resolver doesn't grok `::` as a path
                    // separator nor `crate::` as a crate-root anchor, so this
                    // step resolves manually.
                    //
                    // Cases handled:
                    //   `use crate::a;`         → source="crate", name="a"      → suffix `a.rs`
                    //   `use crate::a::Foo;`    → source="crate::a", name="Foo" → suffix `a.rs`
                    //   `use crate::a::b::Foo;` → source="crate::a::b", name="Foo" → suffix `b.rs`
                    //   `use std::io;`          → source="std", name="io"       → external, no match
                    //   `use super::Foo;`       → source="super", name="Foo"    → skipped (needs caller dir walk)
                    if target_file_idx.is_none() {
                        let raw = import.source.as_str();
                        let module_last: Option<String> = if raw == "crate" || raw == "self" {
                            // `use crate::Foo` / `use self::Foo` — imported_name IS
                            // the module-file basename.
                            Some(import.imported_name.clone())
                        } else if let Some(rest) = raw
                            .strip_prefix("crate::")
                            .or_else(|| raw.strip_prefix("self::"))
                        {
                            // Strip leading qualifier, take last `::` segment.
                            rest.rsplit("::").next().map(str::to_string)
                        } else if raw.starts_with("super") {
                            // super:: requires caller_dir walk; defer.
                            None
                        } else {
                            // Don't probe generic `a::b::c` forms — these are
                            // external crate imports (`use std::io::Read`,
                            // `use tokio::io::Interest`) whose last segment is
                            // a module name that coincidentally matches an
                            // unrelated internal file. Probing them caused a
                            // 15× over-extraction in the Rust corner of
                            // .sample_repo (2092 emit vs 137 valid).
                            None
                        };
                        if let Some(last) = module_last {
                            if let Some(caller_ext) = local_graph.file_path.extension() {
                                let candidate =
                                    format!("{}.{}", last, caller_ext.to_string_lossy());
                                target_file_idx = suffix_match_single(&candidate, &basename_idx);
                            }
                        }
                    }

                    // Step 3f: namespace/module-dir match. C# `using NS;`
                    // names a namespace whose source lives under a `/NS/`
                    // directory; Swift `import Module` similarly names a
                    // module-dir. O(1) lookup via dir_component_idx, then
                    // filter same-extension single-hit within the bucket.
                    if target_file_idx.is_none() && !cleaned.is_empty() && !cleaned.contains('/') {
                        if let Some(caller_ext) = local_graph.file_path.extension() {
                            let ext_dot = format!(".{}", caller_ext.to_string_lossy());
                            if let Some(bucket) = dir_component_idx.get(cleaned) {
                                let mut hit: Option<u32> = None;
                                let mut multi = false;
                                for &(path, idx) in bucket {
                                    if path.ends_with(&ext_dot) {
                                        if hit.is_some() {
                                            multi = true;
                                            break;
                                        }
                                        hit = Some(idx);
                                    }
                                }
                                if !multi {
                                    target_file_idx = hit;
                                }
                            }
                        }
                    }

                    if let Some(target) = target_file_idx {
                        if dedupe.insert((source_file_idx, target)) {
                            edges_out.push(Edge {
                                source: source_file_idx,
                                target,
                                rel_type: RelType::Imports,
                                confidence: 0.9,
                                reason: reason_module,
                            });
                            *emitted += 1;
                        }
                    }
                }
            }
            (local_emitted, local_edges)
        })
        .collect();

    // Serial merge: extend the caller's edges_out with each worker's
    // emissions. Sum the per-worker emitted counts for the return value.
    let total_new_edges: usize = chunk_results.iter().map(|(_, v)| v.len()).sum();
    edges_out.reserve(total_new_edges);
    for (n, edges) in chunk_results {
        emitted += n;
        edges_out.extend(edges);
    }

    emitted
}

/// Suffix-match `candidate` against the basename index. Lookup is O(1)
/// on the bucket (typically < 10 entries per basename); only paths that
/// share the candidate's basename get a full-suffix `ends_with` check.
/// Returns `Some(idx)` iff exactly one path equals `candidate` or ends
/// with `"/<candidate>"`; multi-hit returns `None` (single-hit constraint
/// keeps cross-language ambiguity defused — same rule as resolver Tier-3).
fn suffix_match_single(
    candidate: &str,
    basename_idx: &FxHashMap<&str, Vec<(&str, u32)>>,
) -> Option<u32> {
    let cand_basename = std::path::Path::new(candidate)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(candidate);
    let bucket = basename_idx.get(cand_basename)?;
    let needle = format!("/{}", candidate);
    let mut hit: Option<u32> = None;
    for &(path, idx) in bucket {
        if path == candidate || path.ends_with(&needle) {
            if hit.is_some() {
                return None;
            }
            hit = Some(idx);
        }
    }
    hit
}

fn try_named(
    resolver: &Resolver<'_>,
    local_graph: &LocalGraph,
    name: &str,
    source_file_idx: u32,
    reason: ecp_core::pool::StrRef,
    dedupe: &mut FxHashSet<(u32, u32)>,
    edges_out: &mut Vec<Edge>,
) -> usize {
    let mut emitted = 0usize;
    for target_kind in [ResolveTarget::Callable, ResolveTarget::Type] {
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            name,
            &local_graph.imports,
            target_kind,
        );
        for (target_id, confidence) in targets {
            if !dedupe.insert((source_file_idx, target_id)) {
                continue;
            }
            edges_out.push(Edge {
                source: source_file_idx,
                target: target_id,
                rel_type: RelType::Imports,
                confidence,
                reason,
            });
            emitted += 1;
        }
    }
    emitted
}
