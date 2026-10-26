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
//!   kinds against `RawImport.imported_name` (a PHP import: its alias, when
//!   it has one). Covers TS/JS/Python/Java/PHP where `imported_name` IS the
//!   symbol (`from a import foo`). A TS / JS / Python import with an alias
//!   (`import { render as paint }`) first takes the import tiers under its
//!   local name, which is what they bind, so a barrel hop is found.
//!   Confidence = tier-determined: `SameFile=1.0`, `ImportScoped=0.95`,
//!   `QualifierScoped=0.85`, `HeritageScoped=0.85`, `Global=0.7`. A hit
//!   through a re-export (a TS / JS barrel, a Python package's
//!   `__init__.py`) also emits the File → barrel File edge of Tier 2
//!   (0.9, `post_process:imports:module`).
//!
//! - **Step 2**: FQN last-segment retry — if `imported_name` contains `.`
//!   (Kotlin / Java `import com.x.Alpha` surfaces with `imported_name =
//!   "com.x.Alpha"`), retry with `"Alpha"`. Same confidence as Step 1.
//!
//! **Tier 2 — module-style path resolution** (target is `NodeKind::File`).
//! Triggered only when Tier 1 misses. Steps 3a and 3b resolve the
//! specifier and emit `confidence = 0.9`, `reason =
//! "post_process:imports:module"`. Steps 3c to 3f guess, below the 0.7
//! ceiling, with `reason = "post_process:imports:guess"`:
//!
//! - **Step 3a**: probe `import.source` as-is via `enumerate_candidates`
//!   (TS `./a`, Python `.foo` already encode relativity).
//! - **Step 3b**: retry with `./` prefix so the resolver's relative branch
//!   joins caller dir (Ruby `require_relative 'alpha'`, Go `import "x/pkg"`,
//!   C `#include "alpha.h"`).
//! - **Step 3c** (guess, 0.6): basename + suffix match across all indexed files via
//!   pre-built `basename_idx` (C/C++ `#include` where header sits under
//!   `-I include/`).
//! - **Step 3d** (guess, 0.5): caller-extension + last-segment basename match (Go
//!   `import "modulePath/pkg"` whose module prefix isn't a filesystem
//!   path → match `pkg.<caller_ext>`).
//!
//! **Tier 3 — language-specific path normalization** (target is
//! `NodeKind::File`):
//!
//! - **Step 3e** (guess, 0.5): Rust `use crate::a::b::Foo` — strip leading `crate::` /
//!   `self::`, take last `::` segment as module-file basename, suffix-match
//!   with caller extension.
//! - **Step 3f** (guess, 0.4): namespace / module-dir match for C# `using NS;` /
//!   Swift `import Module` where the specifier names a directory
//!   containing the implementation file. Uses `dir_component_idx`.
//!
//! Steps 3c to 3f match a name, not a resolved path: each edge they emit
//! carries its step's confidence, below `HEURISTIC_CEILING`, and
//! `post_process:imports:guess`, so a reader tells a guess from the resolved
//! `post_process:imports:module` edge of steps 3a and 3b.

use crate::resolution::index::{Language, ResolveTarget};
use crate::resolution::resolver::{NodeId, Resolver};
use ecp_core::analyzer::rust_paths::rust_module_path_base;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::cell::Cell;
use std::sync::Arc;

/// Confidence of a basename + suffix match (step 3c): the specifier's whole
/// path tail agrees.
const SUFFIX_GUESS_CONFIDENCE: f32 = 0.6;
/// Confidence of a last-segment + caller-extension match (step 3d) and of a
/// Rust module-name match (step 3e): one name agrees.
const NAME_GUESS_CONFIDENCE: f32 = 0.5;
/// Confidence of a namespace / module-directory match (step 3f).
const DIRECTORY_GUESS_CONFIDENCE: f32 = 0.4;

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
    let reason_guess = string_pool.add("post_process:imports:guess");
    let mut emitted = 0usize;

    // Pre-pass: build a basename → [(full_path, idx)] index so the
    // Step 3c/3d/3e/3f fallbacks can do O(1) hash lookup + bucket-local
    // suffix filter instead of an O(N) linear scan across `file_node_idx`
    // per import. On `.sample_repo` (14k files) this shrinks Step 3 cost
    // from O(imports × files) ≈ 200M comparisons to O(imports × bucket)
    // where bucket size is typically < 10.
    // Both indexes built in one pass through file_node_idx — saves one
    // 14k-entry iteration vs the two-loop version.
    let mut basename_idx: FxHashMap<&str, Vec<(&str, u32)>> = FxHashMap::default();
    let mut dir_component_idx: FxHashMap<&str, Vec<(&str, u32)>> = FxHashMap::default();
    for (path, &idx) in file_node_idx.iter() {
        let path_str = path.as_str();
        let basename = std::path::Path::new(path_str)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path_str);
        basename_idx
            .entry(basename)
            .or_default()
            .push((path_str, idx));
        for component in path_str.split('/') {
            if !component.is_empty() && !component.contains('.') {
                dir_component_idx
                    .entry(component)
                    .or_default()
                    .push((path_str, idx));
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
            // Forward-slash normalize without allocating on Linux/macOS
            // (raw path is already `/`-only there). file_node_idx.get() takes
            // &str so Cow → &str → HashMap lookup is alloc-free for the
            // common branch.
            let raw_path = local_graph.file_path.to_string_lossy();
            let path_str: Cow<'_, str> = if raw_path.contains('\\') {
                Cow::Owned(raw_path.replace('\\', "/"))
            } else {
                raw_path
            };
            let Some(&source_file_idx) = file_node_idx.get(path_str.as_ref()) else {
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

            // A PHP import binds its alias (`use A\B as C`, and every group
            // member `use A\{B}`), not the declared name, which may be a
            // same-file declaration's name.
            let language = resolver.file_language(&path_str);
            let php = language == Language::Php;
            // The import tiers of these languages bind an import by its
            // local name. The general tiers keep the declared name: an alias
            // names no declaration.
            let binds_local = matches!(
                language,
                Language::TypeScript | Language::JavaScript | Language::Python
            );
            let file_path = &local_graph.file_path;
            let imports = &local_graph.imports;
            for import in imports {
                let before = *emitted;
                let local_name = match &import.alias {
                    Some(alias) if php => alias,
                    _ => &import.imported_name,
                };
                // Step 1: named-symbol lookup.
                let aliased = import.alias.as_deref().filter(|alias| {
                    binds_local && import.imported_name != "*" && *alias != import.imported_name
                });
                // Whether the local name bound at all: an edge the dedupe
                // drops is still that import's answer.
                let bound = Cell::new(false);
                let mut found = aliased.map_or((0, None), |alias| {
                    try_named(
                        |target| {
                            let hit =
                                resolver.resolve_import_binding(file_path, alias, imports, target);
                            bound.set(bound.get() || !hit.0.is_empty());
                            hit
                        },
                        source_file_idx,
                        reason_named,
                        &mut dedupe,
                        edges_out,
                    )
                });
                if !bound.get() {
                    found = try_named(
                        |target| resolver.resolve_import(file_path, local_name, imports, target),
                        source_file_idx,
                        reason_named,
                        &mut dedupe,
                        edges_out,
                    );
                }
                let (named, barrel) = found;
                *emitted += named;
                // A hit through a re-export lands outside the module the
                // import names. The importer still depends on that barrel,
                // and the edit hook's `importers()` finds a barrel's
                // consumers only through this edge.
                if let Some(&barrel_file) =
                    barrel.as_deref().and_then(|path| file_node_idx.get(path))
                {
                    if barrel_file != source_file_idx
                        && dedupe.insert((source_file_idx, barrel_file))
                    {
                        edges_out.push(Edge {
                            source: source_file_idx,
                            target: barrel_file,
                            rel_type: RelType::Imports,
                            confidence: 0.9,
                            reason: reason_module,
                        });
                        *emitted += 1;
                    }
                }

                // Step 2: FQN last-segment retry (Kotlin / Java / PHP qualified imports).
                if *emitted == before && import.imported_name.contains('.') {
                    if let Some(last) = import.imported_name.rsplit('.').next() {
                        if !last.is_empty() && last != import.imported_name {
                            *emitted += try_named(
                                |target| resolver.resolve_import(file_path, last, imports, target),
                                source_file_idx,
                                reason_named,
                                &mut dedupe,
                                edges_out,
                            )
                            .0;
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
                    // Set by the first guess step that hits.
                    let mut guess_confidence: Option<f32> = None;

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
                        if target_file_idx.is_some() {
                            guess_confidence = Some(SUFFIX_GUESS_CONFIDENCE);
                        }
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
                                if target_file_idx.is_some() {
                                    guess_confidence = Some(NAME_GUESS_CONFIDENCE);
                                }
                            }
                        }
                    }

                    // Step 3e: Rust `use` path guess. The Rust parser stamps
                    // `use crate::a::b::Foo` as `source = "crate::a::b"` +
                    // `imported_name = "Foo"`. Steps 3a and 3b resolve the
                    // module through the module tree; when they miss, the
                    // module's file name is matched anywhere in the repo.
                    //
                    //   `use crate::a;`         -> source="crate", name="a"      -> `a.rs`
                    //   `use crate::a::b::Foo;` -> source="crate::a::b"          -> `b.rs`
                    //   `use std::io;`          -> external, no match
                    //   `use super::Foo;`       -> skipped (the guess has no anchor)
                    if target_file_idx.is_none() {
                        if let Some(last) = rust_module_file_stem(
                            &local_graph.file_path,
                            &import.source,
                            &import.imported_name,
                        ) {
                            if let Some(caller_ext) = local_graph.file_path.extension() {
                                let candidate =
                                    format!("{}.{}", last, caller_ext.to_string_lossy());
                                target_file_idx = suffix_match_single(&candidate, &basename_idx);
                                if target_file_idx.is_some() {
                                    guess_confidence = Some(NAME_GUESS_CONFIDENCE);
                                }
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
                                    if hit.is_some() {
                                        guess_confidence = Some(DIRECTORY_GUESS_CONFIDENCE);
                                    }
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
                                confidence: guess_confidence.unwrap_or(0.9),
                                reason: if guess_confidence.is_some() {
                                    reason_guess
                                } else {
                                    reason_module
                                },
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

/// The module-file stem a Rust `use` names, for step 3e: the last module
/// segment of a `crate::` / `self::` path, the item name for `use crate::x;`
/// (the parser leaves `crate` / `self` as the source then). `super::` and
/// external crates give `None`: probing them matched unrelated internal files
/// (2092 emitted against 137 valid on `.sample_repo`). The layout rule is
/// [`rust_module_path_base`]; a crate without a `src/` ancestor (a `tests/`
/// file) has no base, and keeps the plain last segment.
fn rust_module_file_stem(
    caller: &std::path::Path,
    source: &str,
    imported_name: &str,
) -> Option<String> {
    let joined;
    let specifier = if matches!(source, "crate" | "self") {
        if imported_name.contains("::") {
            return None;
        }
        joined = format!("{source}::{imported_name}");
        joined.as_str()
    } else if source.starts_with("crate::") || source.starts_with("self::") {
        source
    } else {
        return None;
    };
    let last = rust_module_path_base(caller, specifier)
        .and_then(|base| base.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| specifier.rsplit("::").next().map(str::to_string))?;
    Some(last)
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

/// Step 1 through `resolve` ([`Resolver::resolve_import`] or
/// [`Resolver::resolve_import_binding`]): the edges it emitted, and the
/// barrel a re-export hop of the import tier went through.
fn try_named(
    resolve: impl Fn(ResolveTarget) -> (Vec<(NodeId, f32)>, Option<Arc<str>>),
    source_file_idx: u32,
    reason: ecp_core::pool::StrRef,
    dedupe: &mut FxHashSet<(u32, u32)>,
    edges_out: &mut Vec<Edge>,
) -> (usize, Option<Arc<str>>) {
    let mut emitted = 0usize;
    let mut barrel = None;
    for target_kind in [ResolveTarget::Callable, ResolveTarget::Type] {
        let (targets, via) = resolve(target_kind);
        barrel = barrel.or(via);
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
    (emitted, barrel)
}
