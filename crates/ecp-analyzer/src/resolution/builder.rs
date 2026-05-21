use crate::fetch_shape::{consumer_keys, fetch_urls, format_reason, response_shapes};
use crate::framework_helpers::Span;
use crate::resolution::index::{ResolveTarget, SymbolTable};
use crate::resolution::path_aliases::PathAliases;
use crate::resolution::resolver::Resolver;
use aho_corasick::{AhoCorasick, MatchKind};
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{
    BlindSpotRecord, CallMeta, Edge, File, FileCategory, FunctionMeta, Node, NodeKind, RelType,
    RouteShape, ZeroCopyGraph,
};
use ecp_core::pool::{StrRef, StringPool};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::sync::OnceLock;

#[derive(Copy, Clone)]
enum PathPatternKind {
    Reference,
    Example,
    Test,
}

/// Substring patterns used by `determine_category`, scanned in one
/// Aho-Corasick pass instead of N independent `contains()` calls. The
/// 25k-file cold index used to spend ~150k substring scans here
/// (35 patterns × 4286 files-per-cpu-core); a single AC pass collapses
/// that to one scan per file with constant-time per-character work,
/// surfaced by the PR #149 simplify review.
///
/// The PascalCase test suffixes (`Test.java`, `Tests.kt`, `Spec.scala`)
/// stay on `ends_with` — they're suffix-anchored and need case-sensitive
/// comparison against the original-cased path (`Manifest.java`
/// lowercased ends with `test.java`, so a case-insensitive scan would
/// mis-classify it).
static PATH_PATTERN_AC: OnceLock<(AhoCorasick, Vec<PathPatternKind>)> = OnceLock::new();

fn path_pattern_ac() -> &'static (AhoCorasick, Vec<PathPatternKind>) {
    PATH_PATTERN_AC.get_or_init(|| {
        // (kind, pattern) — kept verbatim from the original cascade so
        // the diff against the pre-AC implementation stays mechanical.
        const PATTERNS: &[(PathPatternKind, &str)] = &[
            // Reference — vendored / installed deps, never user-authored source.
            (PathPatternKind::Reference, "/vendor/"),
            (PathPatternKind::Reference, "/node_modules/"),
            (PathPatternKind::Reference, "/.venv/"),
            (PathPatternKind::Reference, "/venv/"),
            (PathPatternKind::Reference, "/site-packages/"),
            (PathPatternKind::Reference, "/.tox/"),
            (PathPatternKind::Reference, "/.bundle/"),
            (PathPatternKind::Reference, "/gems/"),
            (PathPatternKind::Reference, "/.pub-cache/"),
            (PathPatternKind::Reference, "/.gradle/"),
            (PathPatternKind::Reference, "/.m2/"),
            (PathPatternKind::Reference, "/pods/"),
            (PathPatternKind::Reference, "/carthage/"),
            (PathPatternKind::Reference, "/.build/"),
            (PathPatternKind::Reference, "/third_party/"),
            (PathPatternKind::Reference, "/external/"),
            (PathPatternKind::Reference, "/deps/"),
            // Example / sample / demo — canonical "how to use this framework"
            // content. Surfaced separately from Test so routes / tools /
            // handlers under `/examples/` stay visible to LLM consumers
            // (Express's `examples/auth/`, NestJS `sample/`, Flask
            // `examples/tutorial/`). `/tests/` stays as Test because test
            // fixtures (`@app.route('/test_setup')`, helper test endpoints)
            // would pollute the production-route surface.
            (PathPatternKind::Example, "/examples/"),
            (PathPatternKind::Example, "/example/"),
            (PathPatternKind::Example, "/sample/"),
            (PathPatternKind::Example, "/samples/"),
            (PathPatternKind::Example, "/demo/"),
            (PathPatternKind::Example, "/demos/"),
            // Test — substring forms. Suffix forms (`_test.go`, etc.) are
            // handled separately because `ends_with` already runs in
            // constant time against the path tail.
            (PathPatternKind::Test, ".test."),
            (PathPatternKind::Test, ".spec."),
            // NestJS / Angular `.e2e-spec.ts` etc.
            (PathPatternKind::Test, "-spec."),
            (PathPatternKind::Test, "__tests__/"),
            (PathPatternKind::Test, "__mocks__/"),
            (PathPatternKind::Test, "/test/"),
            (PathPatternKind::Test, "/tests/"),
            (PathPatternKind::Test, "/testing/"),
            (PathPatternKind::Test, "/fixtures/"),
            // Cypress / NestJS / Playwright e2e dirs.
            (PathPatternKind::Test, "/e2e/"),
            (PathPatternKind::Test, "/spec/"),
            (PathPatternKind::Test, "/test_"),
            (PathPatternKind::Test, "/conftest."),
        ];
        let strings: Vec<&str> = PATTERNS.iter().map(|(_, s)| *s).collect();
        let kinds: Vec<PathPatternKind> = PATTERNS.iter().map(|(k, _)| *k).collect();
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(strings)
            .expect("path-pattern AC build");
        (ac, kinds)
    })
}

pub fn determine_category(path: &str) -> FileCategory {
    let normalized_path = path.replace('\\', "/");
    // Prefix with "/" so patterns like "/vendor/" match both embedded
    // segments AND top-level paths (e.g. `vendor/foo` → `/vendor/foo`).
    let lower_path = format!("/{}", normalized_path.to_lowercase());

    let (ac, kinds) = path_pattern_ac();
    let mut hit_reference = false;
    let mut hit_example = false;
    let mut hit_test_substring = false;
    for m in ac.find_iter(&lower_path) {
        match kinds[m.pattern().as_usize()] {
            PathPatternKind::Reference => {
                // Reference outranks Example and Test (vendored sample
                // dirs still classify as Reference). Bail early — no
                // later match can override.
                hit_reference = true;
                break;
            }
            PathPatternKind::Example => hit_example = true,
            PathPatternKind::Test => hit_test_substring = true,
        }
    }
    if hit_reference {
        return FileCategory::Reference;
    }
    if hit_example {
        return FileCategory::Example;
    }

    let is_test = hit_test_substring
        || lower_path.ends_with("_test.go")
        || lower_path.ends_with("_test.py")
        || lower_path.ends_with("_spec.rb")
        || lower_path.ends_with("_test.rb")
        // PascalCase test-class suffixes (Java/JUnit, Kotlin, Swift XCTest,
        // .NET MSTest/xUnit/NUnit, PHPUnit, ScalaTest/specs2). Case-sensitive
        // intentionally: `Manifest.java` lowercased ends with `test.java`, so
        // a case-insensitive check would mis-classify it. PascalCase `Test`
        // (capital T) is the language-mandated convention for these
        // ecosystems, so a literal `Test.ext` / `Tests.ext` / `Spec.ext`
        // suffix is a reliable signal.
        || normalized_path.ends_with("Test.java")
        || normalized_path.ends_with("Tests.java")
        || normalized_path.ends_with("Test.kt")
        || normalized_path.ends_with("Tests.kt")
        || normalized_path.ends_with("Tests.swift")
        || normalized_path.ends_with("Tests.cs")
        || normalized_path.ends_with("Test.cs")
        || normalized_path.ends_with("Test.php")
        || normalized_path.ends_with("Spec.scala")
        || normalized_path.ends_with("Test.scala");
    if is_test {
        return FileCategory::Test;
    }

    if lower_path.ends_with(".md") || lower_path.ends_with(".txt") || lower_path.ends_with(".rst") {
        return FileCategory::Document;
    }
    if lower_path.ends_with(".json")
        || lower_path.ends_with(".toml")
        || lower_path.ends_with(".yaml")
        || lower_path.ends_with(".yml")
        || lower_path.ends_with("dockerfile")
    {
        return FileCategory::Config;
    }
    FileCategory::Source
}

use std::collections::HashMap;

/// Per-graph output from Pass 2 parallel map: `(edges, pending_call_metas)`.
/// `pending_call_metas` entries are `(pre_sort_edge_idx, flags, dispatch_type)`.
type PerGraphPass2 = (Vec<Edge>, Vec<(usize, u8, String)>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CallMetaKey {
    caller_span: (u32, u32, u32, u32),
    call_index: u32,
}

impl CallMetaKey {
    const fn new(caller_span: (u32, u32, u32, u32), call_index: u32) -> Self {
        Self {
            caller_span,
            call_index,
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(20)
        .collect()
}

pub struct GraphBuilder {
    local_graphs: Vec<LocalGraph>,
    old_file_hashes: HashMap<String, [u8; 8]>,
    /// When `Some`, the resolver pass 2 buffers every decision and writes a
    /// JSONL line per resolution attempt to this path. Used by the oracle
    /// verification harness (see specs/2026-05-15-resolver-oracle-harness.md).
    resolver_dump_path: Option<std::path::PathBuf>,
    /// Module-specifier aliases (TS `tsconfig.json` `compilerOptions.paths`,
    /// etc.) — forwarded to the resolver before Pass 2 starts.
    path_aliases: PathAliases,
    /// Repo root used to resolve `LocalGraph.file_path` (relative) to an
    /// absolute path when the fetch-shape pass needs to re-read the file
    /// content. Optional: when `None`, fetch-shape extraction is skipped
    /// (graph still builds; `route_shapes` is empty and no `Fetches`
    /// edges are emitted). Production callers (the `analyze` command)
    /// always pass this; in-process tests opt in via tempdirs.
    repo_root: Option<std::path::PathBuf>,
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self {
            local_graphs: Vec::new(),
            old_file_hashes: HashMap::new(),
            resolver_dump_path: None,
            path_aliases: PathAliases::new(),
            repo_root: None,
        }
    }

    pub fn with_path_aliases(mut self, aliases: PathAliases) -> Self {
        self.path_aliases = aliases;
        self
    }

    /// Provide the repo root so the fetch-shape pass can resolve each
    /// `LocalGraph.file_path` (relative) to an absolute path for re-read.
    /// Without this, `route_shapes` / `Fetches` edges are skipped.
    pub fn with_repo_root(mut self, root: std::path::PathBuf) -> Self {
        self.repo_root = Some(root);
        self
    }

    pub fn with_cache(mut self, hashes: HashMap<String, [u8; 8]>) -> Self {
        self.old_file_hashes = hashes;
        self
    }

    pub fn with_resolver_dump(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.resolver_dump_path = path;
        self
    }

    pub fn add_graph(&mut self, graph: LocalGraph) {
        self.local_graphs.push(graph);
    }

    pub fn build(mut self) -> ZeroCopyGraph {
        let prof = std::env::var("ECP_PROF").is_ok();
        let t_total = std::time::Instant::now();
        // Determinism: sort by file_path so node IDs and edge endpoints are
        // assigned in canonical order regardless of how the producer (scanner,
        // hook, manual add_graph) enumerated files. Without this, the same repo
        // indexed on two machines with different filesystem walk orders would
        // produce different graph.bin payloads — breaks the reproducibility
        // contract pinned by audit §4.2 (inv-003).
        // `file_path` is unique across LocalGraph entries (one ingest per file),
        // so unstable sort has no observable tie-breaking concern and avoids
        // the temporary allocation that stable sort needs.
        let t_sort = std::time::Instant::now();
        self.local_graphs
            .sort_unstable_by(|a, b| a.file_path.cmp(&b.file_path));
        if prof {
            eprintln!("prof build.sort: {:.3}s", t_sort.elapsed().as_secs_f32());
        }
        let _t_pass1 = std::time::Instant::now();

        // Pre-size accumulators from known input cardinalities. Each
        // LocalGraph contributes 1 File node + N symbol nodes. Plus a
        // small per-graph slack for Pass 1.5 Route nodes (one per
        // routing decorator). Vec growth from 0 → 297k goes through
        // ~17 reallocations + memcopies; sizing once avoids that.
        let total_symbol_nodes: usize = self.local_graphs.iter().map(|g| g.nodes.len()).sum();
        let total_files = self.local_graphs.len();
        // 10% slack for Pass 1.5 Route synthetics; over-shoot is cheap
        // (single tail growth), under-shoot just reverts to default.
        let mut symbol_table = SymbolTable::new();
        let mut string_pool = StringPool::new();
        let mut nodes = Vec::with_capacity(total_symbol_nodes + total_symbol_nodes / 10);
        let mut files = Vec::with_capacity(total_files);
        // Maps forward-slashed file_path → File node index. Populated in
        // Pass 1, consumed by `post_process::imports_edges` to wire
        // (File)-[:Imports]->(symbol) edges. File nodes are deliberately
        // NOT registered in SymbolTable — File is metadata, not a symbol,
        // and a SymbolTable hit on a File node would create spurious
        // Calls/Accesses edges in the resolver tiers.
        // Pre-size to one bucket per file — exact, since each file
        // contributes exactly one entry.
        let mut file_node_idx: FxHashMap<String, u32> =
            FxHashMap::with_capacity_and_hasher(total_files, Default::default());

        // Pass 1: Register all nodes into SymbolTable and StringPool
        let mut current_node_idx = 0;
        // Reusable UID buffer — avoids one allocation per `Node` (~297k
        // nodes on .sample_repo). `push_str` chain over `write!` skips
        // the fmt dispatch (see `NodeKind::as_str` doc) while reusing
        // the underlying capacity across clears.
        let mut uid_buf = String::with_capacity(128);

        for (file_idx, local_graph) in self.local_graphs.iter().enumerate() {
            let file_idx = file_idx as u32;
            // Path → string 一律走 forward-slash，讓 UID / lookup / 顯示在 Windows
            // 上與 Linux/macOS 一致（與 resolver.rs / registry/path.rs 既有 idiom 對齊）。
            // Cow: `to_string_lossy()` returns Cow; `.replace()` always allocates.
            // Skip the replace + alloc on Linux/macOS where paths use `/` already.
            let raw_path = local_graph.file_path.to_string_lossy();
            let path_str: std::borrow::Cow<'_, str> = if raw_path.contains('\\') {
                std::borrow::Cow::Owned(raw_path.replace('\\', "/"))
            } else {
                raw_path
            };
            let path_ref = string_pool.add(&path_str);
            // Hoisted once per file. `register_node` would otherwise call
            // `FileMeta::from_path` per node (~25× redundant on the
            // .sample_repo distribution), each allocating one `String`
            // for the `\\` → `/` normalisation. `path_str` is already
            // forward-slash, so use the `_normalized_path` fast path.
            let file_meta = crate::resolution::index::FileMeta::from_normalized_path(&path_str);

            files.push(File {
                path: path_ref,
                mtime: 0, // In a real implementation, fetch actual mtime
                content_hash: local_graph.content_hash,
                category: determine_category(&path_str),
            });

            for raw_node in &local_graph.nodes {
                symbol_table.register_node_with_meta(
                    &path_str,
                    file_meta,
                    &raw_node.name,
                    current_node_idx,
                    raw_node.kind,
                );

                uid_buf.clear();
                // push_str chain over `write!("{:?}:{}:{}")`: avoids fmt
                // dispatch (~300k calls × 3 segments). NodeKind::as_str()
                // returns the same byte-stable variant identifier that
                // Debug would, so existing UID strings stay binary-compat.
                uid_buf.push_str(raw_node.kind.as_str());
                uid_buf.push(':');
                uid_buf.push_str(&path_str);
                uid_buf.push(':');
                uid_buf.push_str(&raw_node.name);
                let uid_ref = string_pool.add(&uid_buf);
                let name_ref = string_pool.add(&raw_node.name);

                nodes.push(Node {
                    uid: uid_ref,
                    name: name_ref,
                    file_idx,
                    kind: raw_node.kind,
                    span: raw_node.span,
                    community_id: 0,
                });

                current_node_idx += 1;
            }

            // NOTE: documents (markdown/yaml section/doc nodes) are parsed into
            // `local_graph.documents` but the graph.bin DocumentBlock storage is
            // not wired up yet. Skipped here intentionally — re-enable when the
            // `DocumentBlock` type lands in `ecp_core::graph`.
        }

        // Finalize the basename-stem → file paths view consumed by the
        // resolver's Tier-4 module-file fallback. Pass 1 is the only writer
        // of `file_scoped`, so finalizing here gives every subsequent pass
        // (and the resolver) an O(1) `files_by_stem` lookup instead of an
        // O(N_files) scan per qualified call.
        symbol_table.build_stem_index();

        if prof {
            eprintln!(
                "prof build.pass1_register: {:.3}s",
                _t_pass1.elapsed().as_secs_f32()
            );
        }
        let _t_pass15 = std::time::Instant::now();
        // Pass 1.5: Extract Routes
        let mut route_edges = Vec::new();
        let mut current_handler_idx = 0;
        // (route_node_idx, file_idx, route_path) — drives Pass 1.6 below.
        let mut emitted_routes: Vec<(u32, u32, String)> = Vec::new();
        for (file_idx, local_graph) in self.local_graphs.iter().enumerate() {
            let file_idx = file_idx as u32;
            let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
            // Skip Route emission for genuinely non-production files
            // (Test/Reference). `Example` is INTENTIONALLY NOT skipped —
            // framework example apps (Express `examples/auth/`, Flask
            // `examples/tutorial/`) are canonical "how to wire routes"
            // content that LLM consumers explicitly want to navigate.
            // Tests (`/tests/`, `.spec.`, `Test.java` …) stay skipped
            // because their `@app.route('/test_setup')` fixture routes
            // would pollute the production-route surface. Reads the
            // category already computed in Pass 1 (line 229) — avoids
            // re-running `determine_category` (~36 string scans per file).
            // `current_handler_idx` still advances by the file's node count
            // so downstream alignment stays correct.
            let is_non_production = matches!(
                files[file_idx as usize].category,
                FileCategory::Test | FileCategory::Reference
            );
            if is_non_production {
                current_handler_idx += local_graph.nodes.len() as u32;
                continue;
            }

            for raw_node in &local_graph.nodes {
                let handler_idx = current_handler_idx;

                for dec in &raw_node.decorators {
                    if let Some(detected) = crate::route_detector::detect_from_decorator(dec) {
                        let route_name = format!("{} {}", detected.method, detected.path);
                        let uid_str = format!("Route:{}:{}", path_str, route_name);

                        let route_idx = nodes.len() as u32;
                        nodes.push(Node {
                            uid: string_pool.add(&uid_str),
                            name: string_pool.add(&route_name),
                            file_idx,
                            kind: ecp_core::graph::NodeKind::Route,
                            span: raw_node.span,
                            community_id: 0,
                        });

                        route_edges.push(Edge {
                            source: handler_idx,
                            target: route_idx,
                            rel_type: RelType::HandlesRoute,
                            confidence: 1.0,
                            reason: string_pool.add("decorator"),
                        });

                        emitted_routes.push((route_idx, file_idx, detected.path.clone()));
                    }
                }
                current_handler_idx += 1;
            }

            for raw_route in &local_graph.routes {
                if let Some(detected) = crate::route_detector::detect_from_call(raw_route) {
                    let route_name = format!("{} {}", detected.method, detected.path);
                    let uid_str = format!("Route:{}:{}", path_str, route_name);

                    let route_idx = nodes.len() as u32;
                    nodes.push(Node {
                        uid: string_pool.add(&uid_str),
                        name: string_pool.add(&route_name),
                        file_idx,
                        kind: ecp_core::graph::NodeKind::Route,
                        span: raw_route.span,
                        community_id: 0,
                    });

                    // Resolve the imperative-route handler, if the parser captured
                    // a named handler (e.g. `app.get("/x", loginHandler)`). The
                    // handler must be a function/method registered in the same
                    // file; inline arrow functions are not captured.
                    if let Some(handler_name) = raw_route.handler.as_deref() {
                        if let Some(handler_node_id) =
                            symbol_table.lookup_in_file(&path_str, handler_name)
                        {
                            route_edges.push(Edge {
                                source: handler_node_id,
                                target: route_idx,
                                rel_type: RelType::HandlesRoute,
                                confidence: 1.0,
                                reason: string_pool.add("call-arg"),
                            });
                        }
                    }

                    emitted_routes.push((route_idx, file_idx, detected.path.clone()));
                }
            }
        }

        if prof {
            eprintln!(
                "prof build.pass15_routes: {:.3}s",
                _t_pass15.elapsed().as_secs_f32()
            );
        }
        let _t_pass16 = std::time::Instant::now();
        // Pass 1.6: Fetch-shape extraction.
        //
        // Two sub-passes — both gated on `repo_root` being available (it
        // is the only way to resolve `LocalGraph.file_path` back to an
        // absolute path so we can re-read the source). Test harnesses that
        // don't set a repo root simply opt out of fetch-shape data.
        //
        // 1.6a — for each Route node, run `response_shapes::extract` on
        //        its handler file and stash a `RouteShape` if it produced
        //        any keys (sparse — empty payloads do not appear).
        // 1.6b — build a path→route_idx map, then for every non-handler
        //        file in `local_graphs` scan for `fetch(url) /
        //        axios.get(url)` literals and emit `RelType::Fetches`
        //        edges (file_node → route_idx) with the
        //        `format_reason(keys, fetch_count)` reason payload.
        //
        // Assumption (MVP): exact path match — `fetch('/users')` only
        // hits a route whose path is exactly `/users`. Dynamic-segment
        // normalisation (upstream `normalizeFetchURL` + `routeMatches`)
        // is intentionally NOT ported here; it can land later without
        // touching the wire format.
        let mut route_shapes_out: Vec<RouteShape> = Vec::new();
        let mut fetches_edges: Vec<Edge> = Vec::new();

        if let Some(repo_root) = self.repo_root.as_ref() {
            // Per-file content cache — multiple routes may share a handler file
            // (Express-style `router.get('/a')`/`router.get('/b')` in one file),
            // and consumer files re-read once even if they fetch many URLs.
            let mut content_cache: FxHashMap<u32, Option<String>> = FxHashMap::default();
            let read_content = |file_idx: u32,
                                cache: &mut FxHashMap<u32, Option<String>>,
                                local_graphs: &[LocalGraph]|
             -> Option<String> {
                if let Some(slot) = cache.get(&file_idx) {
                    return slot.clone();
                }
                let lg = &local_graphs[file_idx as usize];
                let abs = repo_root.join(&lg.file_path);
                let content = std::fs::read_to_string(&abs).ok();
                cache.insert(file_idx, content.clone());
                content
            };

            // 1.6a — RouteShape per Route.
            for (route_idx, file_idx, _route_path) in &emitted_routes {
                let lg = &self.local_graphs[*file_idx as usize];
                let Some(lang) = lang_for_path(&lg.file_path.to_string_lossy()) else {
                    continue;
                };
                let Some(content) = read_content(*file_idx, &mut content_cache, &self.local_graphs)
                else {
                    continue;
                };
                let shape = response_shapes::extract(&content, lang);
                if shape.response_keys.is_empty() && shape.error_keys.is_empty() {
                    continue;
                }
                let response_keys: Vec<StrRef> = shape
                    .response_keys
                    .iter()
                    .map(|k| string_pool.add(k))
                    .collect();
                let error_keys: Vec<StrRef> = shape
                    .error_keys
                    .iter()
                    .map(|k| string_pool.add(k))
                    .collect();
                route_shapes_out.push(RouteShape {
                    node_idx: *route_idx,
                    response_keys,
                    error_keys,
                });
            }

            // 1.6b — Fetches edges. Build path→Vec<route_idx> first
            // (multiple routes can share a path under different methods —
            // we link to all of them; downstream tooling already filters
            // by method when needed).
            let mut route_by_path: FxHashMap<String, Vec<u32>> = FxHashMap::default();
            for (route_idx, _file_idx, route_path) in &emitted_routes {
                route_by_path
                    .entry(route_path.clone())
                    .or_default()
                    .push(*route_idx);
            }

            // File-node index lookup: the source of a Fetches edge is the
            // *file* (per upstream `generateId('File', filePath)`), but
            // egent-code-plexus-rs doesn't currently create File nodes. Use the
            // first node in the file as a reasonable proxy (typically a
            // top-level function/class) so the edge has a real `source`.
            // When a file has no nodes we skip it — there's nothing to
            // attach the edge to.
            let file_first_node: Vec<Option<u32>> = {
                let mut v = Vec::with_capacity(self.local_graphs.len());
                let mut acc: u32 = 0;
                for lg in &self.local_graphs {
                    if lg.nodes.is_empty() {
                        v.push(None);
                    } else {
                        v.push(Some(acc));
                    }
                    acc += lg.nodes.len() as u32;
                }
                v
            };

            // Files that themselves emit a Route are handlers, not
            // consumers — skip them on the consumer pass to avoid
            // self-loops where a file `fetch()`es its own route.
            let handler_files: rustc_hash::FxHashSet<u32> =
                emitted_routes.iter().map(|(_, fi, _)| *fi).collect();

            for (file_idx, lg) in self.local_graphs.iter().enumerate() {
                let file_idx = file_idx as u32;
                if handler_files.contains(&file_idx) {
                    continue;
                }
                let Some(source_node) = file_first_node[file_idx as usize] else {
                    continue;
                };
                let path_str = lg.file_path.to_string_lossy();
                // URL extraction is JS/TS-only (upstream parity) — skip
                // other extensions cheaply by not loading them.
                if lang_for_path(&path_str)
                    .map(|l| matches!(l, response_shapes::Lang::Php))
                    .unwrap_or(true)
                {
                    // None or Php — neither yields a useful consumer scan.
                    continue;
                }
                let Some(content) = read_content(file_idx, &mut content_cache, &self.local_graphs)
                else {
                    continue;
                };
                let urls = fetch_urls::extract(&content);
                if urls.is_empty() {
                    continue;
                }
                let keys = consumer_keys::extract(&content);

                // fetch_count = how many distinct route paths this consumer
                // matches (upstream definition). Compute by intersecting
                // `urls` with `route_by_path` keys.
                let matched_paths: Vec<&String> = urls
                    .iter()
                    .filter(|u| route_by_path.contains_key(*u))
                    .collect();
                if matched_paths.is_empty() {
                    continue;
                }
                let fetch_count = matched_paths.len() as u32;
                let reason_str = format_reason(&keys, fetch_count);
                let reason_ref = string_pool.add(&reason_str);

                for url in &matched_paths {
                    if let Some(targets) = route_by_path.get(*url) {
                        for &route_idx in targets {
                            fetches_edges.push(Edge {
                                source: source_node,
                                target: route_idx,
                                rel_type: RelType::Fetches,
                                confidence: 0.9,
                                reason: reason_ref,
                            });
                        }
                    }
                }
            }
        }

        if prof {
            eprintln!(
                "prof build.pass16_fetch_shape: {:.3}s",
                _t_pass16.elapsed().as_secs_f32()
            );
        }
        let _t_pass17 = std::time::Instant::now();
        // Pass 1.7: Entry-point scoring (cross-language).
        //
        // Pure consumer of `RawRoute` + `RawFrameworkRef` + `main()`
        // detection — see `crate::entry_points` for the scoring matrix.
        // Closes the ⚠️ Entry column for Java / Kotlin / C# / Go / Rust /
        // Swift / C / C++ / Dart in the README Language Matrix.
        //
        // Emits one `NodeKind::EntryPoint` marker node per scored entry
        // point and a `References` edge from the marker to the underlying
        // handler (looked up by name in the same file's SymbolTable). The
        // edge's `reason` carries the scoring provenance so downstream
        // LLM tooling can render "this is an HTTP route handler at
        // confidence 1.0" without re-running the scorer.
        let mut entry_edges: Vec<Edge> = Vec::new();
        for (file_idx, local_graph) in self.local_graphs.iter().enumerate() {
            let file_idx = file_idx as u32;
            let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
            let entries = crate::entry_points::score_entry_points(
                &local_graph.routes,
                &local_graph.framework_refs,
                &local_graph.nodes,
            );
            for ep in entries {
                let handler_idx = symbol_table.lookup_in_file(&path_str, &ep.uid);
                let Some(handler_idx) = handler_idx else {
                    // Handler not found in this file — happens when a
                    // RawRoute's handler name is a string literal that
                    // doesn't match any parsed symbol (e.g. an external
                    // reference). Skip silently; the EntryPoint without
                    // a target would be a dangling marker.
                    continue;
                };
                let entry_uid = format!("EntryPoint:{}:{}:{}", path_str, ep.kind.tag(), ep.uid);
                let entry_name = format!("{}@{}", ep.kind.tag(), ep.uid);
                let entry_idx = nodes.len() as u32;
                nodes.push(Node {
                    uid: string_pool.add(&entry_uid),
                    name: string_pool.add(&entry_name),
                    file_idx,
                    kind: NodeKind::EntryPoint,
                    span: (0, 0, 0, 0),
                    community_id: 0,
                });

                // Encode score in the edge reason: "{tag}:{score}:{reason}".
                // Downstream parsing is trivial (split on first ':') and
                // the reason text is preserved as-is for LLM rendering.
                let edge_reason = format!("{}:{:.2}:{}", ep.kind.tag(), ep.score, ep.reason);
                entry_edges.push(Edge {
                    source: entry_idx,
                    target: handler_idx,
                    rel_type: RelType::References,
                    confidence: ep.score,
                    reason: string_pool.add(&edge_reason),
                });
            }
        }

        if prof {
            eprintln!(
                "prof build.pass17_entry_points: {:.3}s",
                _t_pass17.elapsed().as_secs_f32()
            );
        }
        let _t_pass18 = std::time::Instant::now();
        // Pass 1.8: FunctionMeta collection.
        //
        // For each LocalGraph that has populated `raw_function_metas`, pair each
        // entry with the corresponding graph node by span, then intern the strings
        // into the pool and produce a `FunctionMeta`. The result is sorted by
        // `node_idx` so `ZeroCopyGraph::function_meta()` binary-search works.
        let mut function_metas: Vec<FunctionMeta> = Vec::new();
        {
            let mut node_offset: u32 = 0;
            for local_graph in &self.local_graphs {
                if !local_graph.raw_function_metas.is_empty() {
                    let base = node_offset;
                    // Sorted index → binary_search per node, so the inner loop
                    // is O(N log F) instead of O(N*F).
                    let mut meta_idx: Vec<(Span, usize)> = local_graph
                        .raw_function_metas
                        .iter()
                        .enumerate()
                        .map(|(i, m)| (m.span, i))
                        .collect();
                    meta_idx.sort_by_key(|(s, _)| *s);
                    for (raw_idx, raw_node) in local_graph.nodes.iter().enumerate() {
                        if !matches!(
                            raw_node.kind,
                            NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                        ) {
                            continue;
                        }
                        let node_idx = base + raw_idx as u32;
                        let Ok(slot) = meta_idx.binary_search_by_key(&raw_node.span, |(s, _)| *s)
                        else {
                            continue;
                        };
                        let rfm = &local_graph.raw_function_metas[meta_idx[slot].1];
                        let params: Vec<StrRef> =
                            rfm.params.iter().map(|s| string_pool.add(s)).collect();
                        let return_type = string_pool.add(&rfm.return_type);
                        let decorators: Vec<StrRef> =
                            rfm.decorators.iter().map(|s| string_pool.add(s)).collect();
                        function_metas.push(FunctionMeta {
                            node_idx,
                            flags: rfm.flags,
                            params,
                            return_type,
                            decorators,
                        });
                    }
                }
                node_offset += local_graph.nodes.len() as u32;
            }
        }
        // Sort by node_idx so binary search in function_meta() is valid.
        function_metas.sort_unstable_by_key(|m| m.node_idx);
        if prof {
            eprintln!(
                "prof build.pass18_function_meta: {:.3}s  count={}",
                _t_pass18.elapsed().as_secs_f32(),
                function_metas.len()
            );
        }
        let _t_pass2 = std::time::Instant::now();
        // Pass 2: Resolve imports and build edges
        //
        // Pass 2 strategy: dump-disabled path (production hot path) runs in
        // parallel over `local_graphs` via rayon. Dump-enabled path (oracle
        // harness, off by default) stays serial so one resolver owns the
        // decision stream and preserves deterministic dump order.
        //
        // To enable parallelism we pre-compute two artifacts serially before
        // the par_iter so the inner closure only needs read-only access to
        // the resolver + symbol_table:
        //   1. `start_indices[graph_idx]` — base `current_node_idx` for each
        //      `local_graph` (prefix-sum of node counts). Replaces the
        //      `current_node_idx += 1` accumulator that previously coupled
        //      graphs sequentially.
        //   2. `reason_cache` — every unique `framework_refs.reason` /
        //      `fanout_refs.reason` interned into `string_pool` up front.
        //      `string_pool.add` is `&mut self` so the inner loop can't
        //      touch it; pre-interning + lookup-by-cache is `&StrRef`-only.

        let mut start_indices: Vec<u32> = Vec::with_capacity(self.local_graphs.len());
        {
            // Precompute as u64 so we detect overflow before the lossy cast
            // would corrupt indices. Hitting this means >4.29B total RawNodes
            // — not currently observed in any real repo, but a single int
            // truncation would silently misalign every downstream index.
            let total: u64 = self
                .local_graphs
                .iter()
                .map(|lg| lg.nodes.len() as u64)
                .sum();
            assert!(
                total <= u32::MAX as u64,
                "total RawNode count {} exceeds u32::MAX — graph node ID scheme would overflow",
                total
            );
            let mut acc: u32 = 0;
            for lg in &self.local_graphs {
                start_indices.push(acc);
                acc += lg.nodes.len() as u32;
            }
        }

        let reason_heritage = string_pool.add("heritage");
        let reason_type = string_pool.add("type_annotation");
        let reason_call = string_pool.add("call");

        let mut reason_cache: FxHashMap<String, StrRef> = FxHashMap::default();
        for lg in &self.local_graphs {
            for fw_ref in &lg.framework_refs {
                reason_cache
                    .entry(fw_ref.reason.clone())
                    .or_insert_with(|| string_pool.add(&fw_ref.reason));
            }
            for fanout_ref in &lg.fanout_refs {
                reason_cache
                    .entry(fanout_ref.reason.clone())
                    .or_insert_with(|| string_pool.add(&fanout_ref.reason));
            }
        }

        let dump_enabled = self.resolver_dump_path.is_some();
        let path_aliases = self.path_aliases.clone();

        // Build the Rust workspace module tree once before Pass 2. This is
        // Tier 3.5: resolves `crate::a::b::fn` FQN calls to concrete files
        // by walking the filesystem mod tree from each crate root. Gated on
        // `repo_root` being set — test harnesses that don't set a repo root
        // simply skip module-tree resolution.
        let mod_tree_opt: Option<crate::rust::module_tree::RustWorkspaceModTree> = self
            .repo_root
            .as_ref()
            .map(|root| crate::rust::module_tree::RustWorkspaceModTree::build(root));

        // When dumping is enabled we run the serial path so a single resolver
        // owns the decision stream. When disabled (the production case) we
        // create a fresh `Resolver` *inside* each par_iter worker so each
        // thread owns its own state.
        let mut resolver_for_dump = if dump_enabled {
            let mut r = Resolver::new(&symbol_table).with_path_aliases(path_aliases.clone());
            if let (Some(mt), Some(root)) = (mod_tree_opt.as_ref(), self.repo_root.as_ref()) {
                r = r.with_mod_tree(mt, root.clone());
            }
            r.enable_dump();
            Some(r)
        } else {
            None
        };

        let local_graphs = &self.local_graphs;
        let symbol_table_ref = &symbol_table;
        let reason_cache_ref = &reason_cache;

        // Pre-build per-graph indirect-call lookup keyed by caller span and
        // call index so same-name functions/methods in one file do not collide.
        let indirect_lookups: Vec<FxHashMap<CallMetaKey, (u8, String)>> = local_graphs
            .iter()
            .map(|lg| {
                lg.call_metas
                    .iter()
                    .map(|m| {
                        (
                            CallMetaKey::new(m.caller_span, m.call_index),
                            (m.flags, m.dispatch_type.clone()),
                        )
                    })
                    .collect()
            })
            .collect();

        // `pending_call_metas`: pairs of (pre-sort edge index, RawCallMeta ref).
        // Collected alongside edges and promoted to ZeroCopyGraph.call_metas after
        // the final edge sort remaps pre-sort indices to sorted positions.
        let mut pending_call_metas_global: Vec<(usize, u8, String)> = Vec::new();

        let edges: Vec<Edge> = if let Some(resolver) = resolver_for_dump.as_mut() {
            // Serial dump path — original loop, with reason lookups going
            // through `reason_cache` (filled above) instead of inline
            // `string_pool.add`.
            let mut edges = Vec::new();
            let mut current_node_idx = 0u32;
            for (graph_idx, local_graph) in local_graphs.iter().enumerate() {
                let lookup = &indirect_lookups[graph_idx];
                for raw_node in &local_graph.nodes {
                    pass2_emit_node_edges(
                        resolver,
                        local_graph,
                        raw_node,
                        current_node_idx,
                        reason_heritage,
                        reason_type,
                        reason_call,
                        &mut edges,
                        lookup,
                        &mut pending_call_metas_global,
                    );
                    current_node_idx += 1;
                }
                pass2_emit_framework_and_fanout(
                    resolver,
                    symbol_table_ref,
                    local_graph,
                    reason_cache_ref,
                    &mut edges,
                );
            }
            edges
        } else {
            // Parallel path. Each rayon worker drives a `flat_map` chunk;
            // we pay one Resolver construction per local_graph (cheap —
            // borrows symbol_table, clones path_aliases). For ~14k files
            // on .sample_repo that's ~14k path_aliases.clone() calls
            // totalling a few ms — far below the parallelism gain.
            //
            // The mod_tree borrow is `&RustWorkspaceModTree` (read-only,
            // `Sync`), so sharing it across rayon workers is safe.
            let mod_tree_ref = mod_tree_opt.as_ref();
            let workspace_root_ref = self.repo_root.as_ref();
            // Collect (local_edges, local_pending) per graph, then stitch.
            let per_graph: Vec<PerGraphPass2> = local_graphs
                .par_iter()
                .enumerate()
                .map(|(graph_idx, local_graph)| {
                    let mut resolver =
                        Resolver::new(symbol_table_ref).with_path_aliases(path_aliases.clone());
                    if let (Some(mt), Some(root)) = (mod_tree_ref, workspace_root_ref) {
                        resolver = resolver.with_mod_tree(mt, root.clone());
                    }
                    let start_idx = start_indices[graph_idx];
                    let lookup = &indirect_lookups[graph_idx];
                    let mut local_edges: Vec<Edge> = Vec::new();
                    let mut local_pending: Vec<(usize, u8, String)> = Vec::new();
                    for (node_offset, raw_node) in local_graph.nodes.iter().enumerate() {
                        let current_node_idx = start_idx + node_offset as u32;
                        pass2_emit_node_edges(
                            &resolver,
                            local_graph,
                            raw_node,
                            current_node_idx,
                            reason_heritage,
                            reason_type,
                            reason_call,
                            &mut local_edges,
                            lookup,
                            &mut local_pending,
                        );
                    }
                    pass2_emit_framework_and_fanout(
                        &resolver,
                        symbol_table_ref,
                        local_graph,
                        reason_cache_ref,
                        &mut local_edges,
                    );
                    (local_edges, local_pending)
                })
                .collect();

            // Stitch per-graph results: compute global edge offset for each graph's
            // local_pending indices, then merge into the global pending vec.
            let mut all_edges: Vec<Edge> = Vec::new();
            for (local_edges, local_pending) in per_graph {
                let edge_offset = all_edges.len();
                for (local_idx, flags, dispatch_type) in local_pending {
                    pending_call_metas_global.push((edge_offset + local_idx, flags, dispatch_type));
                }
                all_edges.extend(local_edges);
            }
            all_edges
        };
        let mut edges = edges;
        let resolver_dump_drain = resolver_for_dump.as_mut();

        edges.extend(route_edges);
        edges.extend(entry_edges);
        edges.extend(fetches_edges);

        if prof {
            eprintln!(
                "prof build.pass2_imports_resolve: {:.3}s",
                _t_pass2.elapsed().as_secs_f32()
            );
        }
        let _t_blind = std::time::Instant::now();
        // Pass: blind spots — pure metadata passthrough, no edges created.
        // Each local_graph's blind_spots are interned and stored in the graph's
        // file-level metadata for `ecp context` / `ecp index` to surface to
        // the LLM (truly unresolvable patterns like eval/dynamic-import).
        let mut all_blind_spots: Vec<BlindSpotRecord> = Vec::new();
        for local_graph in &self.local_graphs {
            for bs in &local_graph.blind_spots {
                all_blind_spots.push(BlindSpotRecord {
                    kind: string_pool.add(&bs.kind),
                    file_path: string_pool.add(&bs.file_path.to_string_lossy().replace('\\', "/")),
                    start_row: bs.span.0,
                    start_col: bs.span.1,
                    end_row: bs.span.2,
                    end_col: bs.span.3,
                    hint: string_pool.add(&bs.hint),
                });
            }
        }

        // Optional: flush the resolver decision dump now that pass 2 is done.
        // Spec: docs/specs/2026-05-15-resolver-oracle-harness.md
        // Only the serial dump-enabled path keeps a `Resolver` alive past
        // Pass 2; the parallel path constructs ephemeral per-graph resolvers
        // with `decisions: None`, so a dump-disabled run has nothing to flush.
        if let Some(dump_path) = self.resolver_dump_path.as_ref() {
            if let Some(resolver) = resolver_dump_drain {
                if let Some(decisions) = resolver.take_decisions() {
                    if let Err(e) = write_resolver_dump(dump_path, &decisions, &symbol_table) {
                        tracing::warn!("Failed to write resolver dump to {:?}: {}", dump_path, e);
                    }
                }
            }
        }

        if prof {
            eprintln!(
                "prof build.blind_spots: {:.3}s",
                _t_blind.elapsed().as_secs_f32()
            );
        }
        let _t_pass3 = std::time::Instant::now();
        // Pass 3: Community detection (Leiden) over Calls/Extends/Implements edges.
        // Leiden's refinement phase prevents the badly-connected-hub failure
        // mode where Louvain pins a hub to its first-touched chain.
        // Writes community_id back onto each Node in place.
        let assignments = ecp_core::algorithms::leiden::detect_communities(
            &nodes,
            &edges,
            &ecp_core::algorithms::leiden::LeidenConfig::default(),
        );
        for (node, &c) in nodes.iter_mut().zip(assignments.iter()) {
            node.community_id = c;
        }

        if prof {
            eprintln!(
                "prof build.pass3_community: {:.3}s",
                _t_pass3.elapsed().as_secs_f32()
            );
        }
        let _t_pass4 = std::time::Instant::now();
        // Pass 4: Process detection (BFS forward via CALLS).
        // Produces traces; each trace becomes a NodeKind::Process node + N
        // StepInProcess edges. Process nodes are appended to `nodes` so they
        // sit at the tail — `process_start` marks the boundary.
        let file_paths: Vec<String> = files
            .iter()
            .map(|f| {
                let start = f.path.offset as usize;
                let end = start + f.path.len as usize;
                std::str::from_utf8(&string_pool.bytes[start..end])
                    .unwrap_or("")
                    .to_string()
            })
            .collect();

        let traces = ecp_core::algorithms::process_trace::detect_processes(
            &nodes,
            &edges,
            &file_paths,
            &ecp_core::algorithms::process_trace::ProcessConfig::default(),
        );

        let process_start_idx = nodes.len() as u32;
        let mut traces_offsets: Vec<u32> = Vec::with_capacity(traces.len() + 1);
        let mut traces_data: Vec<u32> = Vec::new();
        traces_offsets.push(0);

        for (k, tr) in traces.iter().enumerate() {
            let entry_idx = tr.trace.first().copied().unwrap_or(0);
            let terminal_idx = tr.trace.last().copied().unwrap_or(0);
            let entry_name = nodes
                .get(entry_idx as usize)
                .map(|n| {
                    std::str::from_utf8(
                        &string_pool.bytes
                            [n.name.offset as usize..n.name.offset as usize + n.name.len as usize],
                    )
                    .unwrap_or("")
                    .to_string()
                })
                .unwrap_or_default();
            let terminal_name = nodes
                .get(terminal_idx as usize)
                .map(|n| {
                    std::str::from_utf8(
                        &string_pool.bytes
                            [n.name.offset as usize..n.name.offset as usize + n.name.len as usize],
                    )
                    .unwrap_or("")
                    .to_string()
                })
                .unwrap_or_default();

            let label = format!(
                "{} → {}",
                capitalize(&entry_name),
                capitalize(&terminal_name)
            );
            let uid_str = format!(
                "proc_{}_{}_{}",
                k,
                sanitize_id(&entry_name),
                sanitize_id(&terminal_name)
            );

            let process_node_idx = nodes.len() as u32;
            let process_node_community = nodes
                .get(entry_idx as usize)
                .map(|n| n.community_id)
                .unwrap_or(0);

            nodes.push(Node {
                uid: string_pool.add(&uid_str),
                name: string_pool.add(&label),
                file_idx: nodes
                    .get(entry_idx as usize)
                    .map(|n| n.file_idx)
                    .unwrap_or(0),
                kind: NodeKind::Process,
                span: nodes
                    .get(entry_idx as usize)
                    .map(|n| n.span)
                    .unwrap_or((0, 0, 0, 0)),
                community_id: process_node_community,
            });

            for (step_idx, &member_idx) in tr.trace.iter().enumerate() {
                let reason_str = format!("step:{}", step_idx + 1);
                edges.push(Edge {
                    source: member_idx,
                    target: process_node_idx,
                    rel_type: RelType::StepInProcess,
                    confidence: 1.0,
                    reason: string_pool.add(&reason_str),
                });
                traces_data.push(member_idx);
            }
            traces_offsets.push(traces_data.len() as u32);
        }

        // Cross-language class membership post-process. Emits HasMethod /
        // HasProperty edges (Class → Function|Method|Property) for every
        // language. Pass 1 uses span containment; Pass 2 uses RawNode.owner_class
        // (set directly by each parser) as primary and the legacy __impl_target__:
        // heritage sentinel as a cache-compat fallback. Must run BEFORE the CSR
        // construction below so new edges land in `out_offsets` / `in_offsets`.
        if prof {
            eprintln!(
                "prof build.pass4_processes: {:.3}s",
                _t_pass4.elapsed().as_secs_f32()
            );
        }
        let _t_class_mem = std::time::Instant::now();
        crate::post_process::class_membership::emit_edges(
            &self.local_graphs,
            &symbol_table,
            &mut string_pool,
            &mut edges,
        );

        // Override resolution — emits `RelType::Overrides` edges (concrete
        // method → overridden supertype method). Runs after class_membership
        // so HasMethod edges are already in place; before CSR construction so
        // the new edges land in out_offsets / in_offsets.
        crate::post_process::overrides::emit_edges(
            &self.local_graphs,
            &symbol_table,
            &mut string_pool,
            &mut edges,
        );

        // Append one `NodeKind::File` node per LocalGraph at the tail of
        // `nodes` (idx >= raw-node count). Doing it here — AFTER all passes
        // that index symbols by SymbolTable + use raw node idx ranges —
        // keeps raw node ids dense [0..N) for the SymbolTable's monotonic-
        // dense invariant (`index.rs::register_node` debug_assert). File
        // nodes don't enter SymbolTable; they're metadata reachable only
        // via `file_node_idx` for module-level edges (Imports today).
        for (i, local_graph) in self.local_graphs.iter().enumerate() {
            let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
            let basename = std::path::Path::new(&path_str)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_str.clone());
            let file_uid = format!("File:{}", path_str);
            let uid_ref = string_pool.add(&file_uid);
            let name_ref = string_pool.add(&basename);
            // `Node.file_idx` points into `files: Vec<File>`. Pass 1 pushed
            // `files` in the same `self.local_graphs` enumeration order, so
            // the i-th LocalGraph's File node references `files[i]`.
            //
            // Use the iteration index directly — an earlier `file_node_idx.len()`
            // formulation silently lagged behind `i` when paths duplicated
            // (HashMap insert overwrites instead of growing), pointing later
            // File nodes at the wrong files[] entry.
            let node_file_idx = i as u32;
            // `nodes.len()` is the authoritative index because Passes 1.5
            // (Routes), 1.6/2 (EntryPoint), and Pass 4 (Process) all push
            // into `nodes` AFTER Pass 1 without keeping `current_node_idx`
            // in sync — using `current_node_idx` here pointed `file_node_idx`
            // at stale raw-node indices, which would silently make
            // `(File)-[:Imports]->()` edges originate from random Route /
            // EntryPoint / Process nodes instead of File.
            let file_node_id = nodes.len() as u32;
            nodes.push(Node {
                uid: uid_ref,
                name: name_ref,
                file_idx: node_file_idx,
                kind: NodeKind::File,
                span: (0, 0, 0, 0),
                community_id: 0,
            });
            file_node_idx.insert(path_str, file_node_id);
        }

        // Cross-language Imports post-process. Emits (File)-[:Imports]->(symbol)
        // edges by resolving each `RawImport.imported_name` against the same
        // SymbolTable + path_aliases used in Pass 2. Resolver misses don't
        // emit — refuse gitnexus-style cross-language false positives
        // (`.mjs → Path.java`). Must run BEFORE CSR construction below so
        // new edges land in `out_offsets` / `in_offsets`.
        let imports_resolver =
            Resolver::new(&symbol_table).with_path_aliases(self.path_aliases.clone());
        if prof {
            eprintln!(
                "prof build.class_membership: {:.3}s",
                _t_class_mem.elapsed().as_secs_f32()
            );
        }
        let _t_imports_edges = std::time::Instant::now();
        let _track_imports_call = ();
        crate::post_process::imports_edges::emit_edges(
            &self.local_graphs,
            &imports_resolver,
            &file_node_idx,
            &mut string_pool,
            &mut edges,
        );

        if prof {
            eprintln!(
                "prof build.imports_edges: {:.3}s",
                _t_imports_edges.elapsed().as_secs_f32()
            );
        }
        let _t_csr = std::time::Instant::now();
        // Final pass: Construct CSR (out_offsets and in_offsets)
        // Sort edges by source to build out_offsets easily.
        // We need to track where pre-sort indices land after sorting so
        // `pending_call_metas_global` pre-sort edge indices can be remapped
        // to the final sorted positions for `ZeroCopyGraph.call_metas`.
        let n_edges = edges.len();
        let mut pre_sort_to_sorted: Vec<u32> = vec![0; n_edges];
        {
            // Build a permutation vector: sorted_positions[i] = pre-sort index that
            // lands at sorted position i. Stable sort preserves relative order of
            // equal keys, mirroring what `edges.sort_by_key` will do.
            let mut perm: Vec<usize> = (0..n_edges).collect();
            perm.sort_by_key(|&i| edges[i].source);
            // Invert: pre_sort_to_sorted[pre_sort_idx] = sorted_idx.
            for (sorted_idx, &pre_idx) in perm.iter().enumerate() {
                pre_sort_to_sorted[pre_idx] = sorted_idx as u32;
            }
        }
        edges.sort_by_key(|e| e.source);

        let num_nodes = nodes.len();
        let mut out_offsets = vec![0; num_nodes + 1];
        for edge in &edges {
            out_offsets[edge.source as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            out_offsets[i + 1] += out_offsets[i];
        }

        // Build in_edge_idx (indices of edges sorted by target).
        // Same overflow guard as the node accumulator: precompute total in
        // u64 and assert before the lossy cast would corrupt the index range.
        assert!(
            edges.len() as u64 <= u32::MAX as u64,
            "total edge count {} exceeds u32::MAX — edge index scheme would overflow",
            edges.len()
        );
        let mut in_edge_idx: Vec<u32> = (0..edges.len() as u32).collect();
        in_edge_idx.sort_by_key(|&idx| edges[idx as usize].target);

        let mut in_offsets = vec![0; num_nodes + 1];
        for &idx in &in_edge_idx {
            let edge = &edges[idx as usize];
            in_offsets[edge.target as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            in_offsets[i + 1] += in_offsets[i];
        }

        if prof {
            eprintln!(
                "prof build.csr_assembly: {:.3}s  total_build: {:.3}s",
                _t_csr.elapsed().as_secs_f32(),
                t_total.elapsed().as_secs_f32()
            );
        }

        // Promote pending_call_metas_global to ZeroCopyGraph.call_metas.
        // Remap pre-sort edge indices to sorted positions via `pre_sort_to_sorted`,
        // intern dispatch_type strings, then sort by edge_idx for binary-search
        // lookup in graph_query.rs hot paths (ZeroCopyGraph.call_meta() contract).
        let mut call_metas: Vec<CallMeta> = pending_call_metas_global
            .into_iter()
            .filter_map(|(pre_idx, flags, dispatch_type)| {
                pre_sort_to_sorted.get(pre_idx).map(|&sorted_idx| CallMeta {
                    edge_idx: sorted_idx,
                    flags,
                    dispatch_type: string_pool.add(&dispatch_type),
                })
            })
            .collect();
        call_metas.sort_by_key(|m| m.edge_idx);
        // Deduplicate: if two RawCallMeta entries map to the same edge (shouldn't
        // happen in practice, but defensive), keep the first (most specific).
        call_metas.dedup_by_key(|m| m.edge_idx);

        ZeroCopyGraph {
            magic: ecp_core::graph::GRAPH_MAGIC,
            version: ecp_core::graph::GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: string_pool.bytes,
            nodes,
            edges,
            out_offsets,
            in_offsets,
            in_edge_idx,
            name_index: Vec::new(), // To be implemented if name indexing is needed
            process_start: process_start_idx,
            traces_offsets,
            traces_data,
            files,
            blind_spots: all_blind_spots,
            route_shapes: route_shapes_out,
            call_metas,
            function_metas,
        }
    }
}

/// Map a file path's extension to the language hint accepted by
/// `response_shapes::extract` / `consumer_keys::extract`. Returns `None`
/// for extensions outside the supported set so callers can skip the
/// fetch-shape pass for those files cheaply.
fn lang_for_path(path: &str) -> Option<response_shapes::Lang> {
    let dot = path.rfind('.')?;
    let ext = &path[dot + 1..];
    match ext {
        "ts" | "tsx" => Some(response_shapes::Lang::TypeScript),
        "js" | "jsx" | "mjs" | "cjs" => Some(response_shapes::Lang::JavaScript),
        "php" => Some(response_shapes::Lang::Php),
        _ => None,
    }
}

/// Serialize captured resolver decisions to a JSONL file. Schema matches
/// the oracle harness contract: one decision per line, fields ordered for
/// readable diffs. Each line is a flattened `ResolverDecision` plus the
/// resolved `target_file` (looked up from `target_id` via the symbol
/// table). Delegating to `serde_json` keeps escaping (Unicode controls,
/// surrogates, line separators) compliant with RFC 8259.
/// Locate the smallest `Class`-kind raw node whose span fully contains
/// `raw_node`'s span, returning its heritage list. Used by Pass-2 call-edge
/// emission to power Tier 2.75 (HeritageScoped) lookups: an unqualified
/// method call inside `class Bar; include Foo; end` carries Bar's heritage
/// (`["Foo"]`) so the resolver can probe Foo's file when Tier 1/2/2.5 miss.
/// Returns an empty slice when the node has no enclosing class (top-level
/// function, etc.). For Ruby `module Foo` and `class Foo` both emit as
/// `NodeKind::Class`, so this single check covers Ruby module mixins too.
fn enclosing_class_heritage<'a>(
    raw_node: &'a RawNode,
    local_graph: &'a LocalGraph,
) -> &'a [String] {
    if raw_node.kind == NodeKind::Class {
        return &raw_node.heritage;
    }
    let (s_row, s_col, e_row, e_col) = raw_node.span;
    let mut best: Option<&RawNode> = None;
    let mut best_span: (u32, u32) = (u32::MAX, u32::MAX);
    for candidate in &local_graph.nodes {
        if candidate.kind != NodeKind::Class {
            continue;
        }
        let (cs_row, cs_col, ce_row, ce_col) = candidate.span;
        let starts_before_or_at = (cs_row, cs_col) <= (s_row, s_col);
        let ends_after_or_at = (ce_row, ce_col) >= (e_row, e_col);
        if !(starts_before_or_at && ends_after_or_at) {
            continue;
        }
        let size = (ce_row.saturating_sub(cs_row), ce_col.saturating_sub(cs_col));
        if size < best_span {
            best_span = size;
            best = Some(candidate);
        }
    }
    best.map(|n| n.heritage.as_slice()).unwrap_or(&[])
}

/// Emit Pass-2 edges for a single `raw_node`'s heritage / calls / type
/// annotation. Factored out so the serial dump path and the parallel
/// hot path can share the same per-node logic.
///
/// `indirect_lookup` maps `(caller_span, call_index)` to
/// `(flags, dispatch_type_string)` for calls that are non-direct. When a Calls
/// edge is emitted from such a call site, a corresponding entry is pushed to
/// `pending_call_metas` using the pre-sort edge index. The caller is responsible
/// for converting pre-sort indices to final sorted indices before placing
/// entries in `ZeroCopyGraph.call_metas`.
#[allow(clippy::too_many_arguments)]
fn pass2_emit_node_edges(
    resolver: &Resolver<'_>,
    local_graph: &LocalGraph,
    raw_node: &RawNode,
    current_node_idx: u32,
    reason_heritage: StrRef,
    reason_type: StrRef,
    reason_call: StrRef,
    edges: &mut Vec<Edge>,
    indirect_lookup: &FxHashMap<CallMetaKey, (u8, String)>,
    pending_call_metas: &mut Vec<(usize, u8, String)>,
) {
    for base in &raw_node.heritage {
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            base,
            &local_graph.imports,
            ResolveTarget::Type,
        );
        for (target_id, confidence) in targets {
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::Extends,
                confidence,
                reason: reason_heritage,
            });
        }
    }

    let call_heritage = enclosing_class_heritage(raw_node, local_graph);
    for (call_idx, callee) in raw_node.calls.iter().enumerate() {
        let lookup_key = CallMetaKey::new(raw_node.span, call_idx as u32);
        let meta = indirect_lookup.get(&lookup_key);
        let targets = resolver.resolve_symbol_with_heritage(
            &local_graph.file_path,
            callee,
            &local_graph.imports,
            ResolveTarget::Callable,
            call_heritage,
        );
        for (target_id, confidence) in targets {
            if target_id == current_node_idx {
                continue; // self-recursion edges are Louvain / process noise
            }
            let edge_pre_sort_idx = edges.len();
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::Calls,
                confidence,
                reason: reason_call,
            });
            if let Some((flags, dispatch_type)) = meta {
                pending_call_metas.push((edge_pre_sort_idx, *flags, dispatch_type.clone()));
            }
        }
    }

    if let Some(type_ann) = &raw_node.type_annotation {
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            type_ann,
            &local_graph.imports,
            ResolveTarget::Type,
        );
        for (target_id, confidence) in targets {
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::Accesses,
                confidence,
                reason: reason_type,
            });
        }
    }
}

/// Emit Pass-2 framework-ref + fanout-ref edges for one `local_graph`.
/// Reason interning is pre-baked into `reason_cache` (see Pass 2 setup);
/// every entry that reaches this function is guaranteed to be in the map.
fn pass2_emit_framework_and_fanout(
    resolver: &Resolver<'_>,
    symbol_table: &SymbolTable,
    local_graph: &LocalGraph,
    reason_cache: &FxHashMap<String, StrRef>,
    edges: &mut Vec<Edge>,
) {
    let file_path_lossy = local_graph.file_path.to_string_lossy().replace('\\', "/");

    for fw_ref in &local_graph.framework_refs {
        let source_id = symbol_table.lookup_in_file(&file_path_lossy, &fw_ref.source_name);
        let Some(source_id) = source_id else { continue };
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            &fw_ref.target_name,
            &local_graph.imports,
            ResolveTarget::Callable,
        );
        // `reason_cache` is filled by the same caller that walked
        // `self.local_graphs` to seed it, so every reason should be
        // present. Use `.get(...)` rather than `[]` indexing so a
        // future caller that forgets to pre-seed degrades to "skip
        // this batch of edges" instead of panicking mid-analyze on
        // a rayon worker (consistent with the best-effort policy of
        // every other Pass 2 error path).
        let Some(&reason_ref) = reason_cache.get(&fw_ref.reason) else {
            continue;
        };
        for (target_id, _) in targets {
            edges.push(Edge {
                source: source_id,
                target: target_id,
                rel_type: RelType::References,
                confidence: fw_ref.confidence,
                reason: reason_ref,
            });
        }
    }

    for fanout_ref in &local_graph.fanout_refs {
        let source_id = symbol_table.lookup_in_file(&file_path_lossy, &fanout_ref.source_name);
        let Some(source_id) = source_id else { continue };
        let n = fanout_ref.candidates.len() as f32;
        if n < 1.0 {
            continue;
        }
        let confidence = (fanout_ref.base_confidence / n.sqrt()).max(0.1);
        let Some(&reason_ref) = reason_cache.get(&fanout_ref.reason) else {
            continue;
        };
        for candidate_name in &fanout_ref.candidates {
            let targets = resolver.resolve_symbol(
                &local_graph.file_path,
                candidate_name,
                &local_graph.imports,
                ResolveTarget::Callable,
            );
            for (target_id, _) in targets {
                edges.push(Edge {
                    source: source_id,
                    target: target_id,
                    rel_type: RelType::References,
                    confidence,
                    reason: reason_ref,
                });
            }
        }
    }
}

fn write_resolver_dump(
    path: &std::path::Path,
    decisions: &[crate::resolution::resolver::ResolverDecision],
    symbol_table: &SymbolTable,
) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for d in decisions {
        let line = DumpLine {
            src_file: &d.src_file,
            name: &d.name,
            specifier: d.specifier.as_deref(),
            tier: d.tier,
            target_file: d.target_id.and_then(|id| symbol_table.file_of(id)),
            alt_count: d.alt_count,
            confidence: d.confidence,
        };
        // serde_json into a Vec<u8> keeps the return type io::Result. The
        // alloc cost is per-decision; for dumps this only fires when the
        // user passed `--dump-resolver`, so production traffic is unaffected.
        let buf = serde_json::to_vec(&line).map_err(std::io::Error::other)?;
        f.write_all(&buf)?;
        f.write_all(b"\n")?;
    }
    f.flush()?;
    Ok(())
}

#[derive(serde::Serialize)]
struct DumpLine<'a> {
    src_file: &'a str,
    name: &'a str,
    specifier: Option<&'a str>,
    tier: crate::resolution::resolver::DecisionTier,
    target_file: Option<&'a str>,
    alt_count: u32,
    confidence: Option<f32>,
}

#[cfg(test)]
mod determine_category_tests {
    use super::{determine_category, FileCategory};

    fn assert_test(path: &str) {
        assert_eq!(
            determine_category(path),
            FileCategory::Test,
            "expected Test for {path}",
        );
    }

    fn assert_source(path: &str) {
        assert_eq!(
            determine_category(path),
            FileCategory::Source,
            "expected Source for {path}",
        );
    }

    fn assert_example(path: &str) {
        assert_eq!(
            determine_category(path),
            FileCategory::Example,
            "expected Example for {path}",
        );
    }

    #[test]
    fn java_kotlin_swift_csharp_php_scala_test_suffixes_classify_as_test() {
        // Per-language test-file conventions added in PR #51.
        assert_test("src/main/java/com/foo/BarTest.java"); // JUnit
        assert_test("src/main/java/com/foo/BarTests.java"); // JUnit alt
        assert_test("app/src/main/kotlin/FooTest.kt"); // Kotlin
        assert_test("app/src/main/kotlin/FooTests.kt"); // Kotlin alt
        assert_test("MyAppTests/AuthFlowTests.swift"); // Swift XCTest
        assert_test("src/Auth.Tests/LoginTests.cs"); // .NET xUnit
        assert_test("src/MyApp/UserTest.cs"); // .NET MSTest alt
        assert_test("app/Http/Controllers/UserControllerTest.php"); // PHPUnit
        assert_test("src/main/scala/com/foo/BarSpec.scala"); // ScalaTest
        assert_test("src/main/scala/com/foo/BarTest.scala"); // ScalaTest alt
    }

    #[test]
    fn example_sample_demo_dirs_classify_as_example() {
        // Round 80 split: framework example/sample/demo dirs are canonical
        // "how to wire routes" content that LLM consumers want to navigate
        // (Express's `examples/auth/`, NestJS's `sample/`, Flask's
        // `examples/tutorial/`). Previously these collapsed into `Test`,
        // which the builder skipped — ecp emitted zero Routes for the
        // 82-row JS examples corpus. Now they classify as `Example` and
        // routes flow through normally; `/tests/` / `.spec.` / Cypress
        // `/e2e/` stay as Test (test fixtures still must not pollute the
        // production-route surface).
        assert_example("JavaScript/examples/auth/index.js");
        assert_example("Ruby/examples/chat.rb");
        assert_example("Python/examples/flask_basic.py");
        assert_example("TypeScript/sample/01-cats-app/src/cats.controller.ts");
        assert_example("packages/demo/index.html");
        // E2E spec files still classify as Test — they're tests against
        // routes, not example apps demonstrating routes.
        assert_test("apps/foo/e2e/login.spec.ts");
        assert_test("apps/foo/src/auth.e2e-spec.ts");
        assert_test("frontend/cypress/e2e/login.cy.ts");
    }

    #[test]
    fn ambiguous_substrings_do_not_classify_as_test() {
        // Guard against the new patterns becoming false-positive magnets.
        // `sample`, `example`, `demo`, `e2e` are common nouns and the
        // path filter must require the literal `/<token>/` segment form,
        // not bare substring matches.
        assert_source("src/sampleRate.ts"); // var name, not /sample/
        assert_source("src/Examples.kt"); // exported public type
        assert_source("src/demographics/service.py");
        assert_source("src/e2encoder/utils.go"); // no /e2e/ segment
        assert_source("src/lib/spec_loader.py"); // _spec at start of basename
    }

    #[test]
    fn non_test_files_classify_as_source() {
        // Files whose names happen to contain the substring "test" but aren't
        // test files — these would mis-classify if the suffix check were
        // case-insensitive, since after lowercasing `Manifest.java` ends with
        // `test.java`. Case-sensitive PascalCase suffix matching keeps them
        // as Source.
        assert_source("src/main/java/com/foo/Manifest.java"); // ends with test.java when lowercased
        assert_source("src/main/java/com/foo/Tester.java");
        assert_source("src/main/java/com/foo/Contestant.java");
        assert_source("app/src/main/kotlin/StressTester.kt");
        assert_source("src/Trading/Backtest.cs"); // lowercased ends with `test.cs`
        assert_source("app/src/main/scala/com/foo/Latest.scala"); // lowercased ends with `test.scala`
                                                                  // Also confirm production code with PascalCase test-like names but
                                                                  // not the literal `Test.ext` suffix stays Source.
        assert_source("src/Auth/AttestationService.cs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
    use ecp_core::graph::NodeKind;

    /// L0 end-to-end: caller imports `./b`, defining file lives at
    /// `src/b.ts`. Tier 2 ImportScoped must fire and emit a `Calls` edge
    /// at confidence 0.95. Locks in the 173-hit win measured on NestJS so
    /// it can't silently regress.
    #[test]
    fn l0_relative_import_produces_import_scoped_edge() {
        let caller = LocalGraph {
            file_path: "src/a.ts".into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: "useThing".into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec!["thing".into()],
                owner_class: None,
            }],
            documents: vec![],
            imports: vec![RawImport {
                source: "./b".into(),
                imported_name: "thing".into(),
                alias: None,
                binding_kind: None,
            }],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        };
        let target = LocalGraph {
            file_path: "src/b.ts".into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: "thing".into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                owner_class: None,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        };

        let mut builder = GraphBuilder::new();
        builder.add_graph(caller);
        builder.add_graph(target);
        let graph = builder.build();

        let calls: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Calls)
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "exactly one Calls edge expected (./b → thing), got {}",
            calls.len()
        );
        // ImportScoped confidence = 0.95 — locks in that L0 promoted the
        // resolution past Tier 3 Global (0.7).
        assert!(
            (calls[0].confidence - 0.95).abs() < 1e-6,
            "Calls edge should be ImportScoped (0.95), got {}",
            calls[0].confidence
        );
    }

    /// `write_resolver_dump` round-trip: produce a dump containing
    /// boundary characters (quote, backslash, newline, control byte,
    /// non-ASCII), parse it back as JSON, assert fidelity. Locks in the
    /// serde-based serializer against silent escape regressions if we
    /// ever revert to a hand-rolled writer.
    #[test]
    fn resolver_dump_round_trips_through_serde_json() {
        use crate::resolution::resolver::{DecisionTier, ResolverDecision};

        let symbol_table = SymbolTable::new();
        let decisions = vec![
            ResolverDecision {
                src_file: "weird \"name\".ts".into(),
                name: "fn\\with\nbreak".into(),
                specifier: Some("./bar".into()),
                tier: DecisionTier::ImportScoped,
                target_id: None,
                alt_count: 0,
                confidence: Some(0.95),
            },
            ResolverDecision {
                src_file: "中文/檔名.py".into(),
                name: "你好".into(),
                specifier: None,
                tier: DecisionTier::Unresolved,
                target_id: None,
                alt_count: 0,
                confidence: None,
            },
        ];

        let tmp = std::env::temp_dir().join(format!("ecp-dump-test-{}.jsonl", std::process::id()));
        write_resolver_dump(&tmp, &decisions, &symbol_table).expect("write dump");
        let text = std::fs::read_to_string(&tmp).expect("read dump");
        let _ = std::fs::remove_file(&tmp);

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for (line, original) in lines.iter().zip(decisions.iter()) {
            let v: serde_json::Value = serde_json::from_str(line).expect("valid JSONL");
            assert_eq!(v["src_file"], original.src_file);
            assert_eq!(v["name"], original.name);
            assert_eq!(v["alt_count"], 0);
            match original.tier {
                DecisionTier::ImportScoped => assert_eq!(v["tier"], "ImportScoped"),
                DecisionTier::Unresolved => assert_eq!(v["tier"], "Unresolved"),
                DecisionTier::SameFile
                | DecisionTier::QualifierScoped
                | DecisionTier::HeritageScoped
                | DecisionTier::Global
                | DecisionTier::AmbiguousGlobal
                | DecisionTier::ModuleTree => {
                    panic!(
                        "fixture should only produce ImportScoped/Unresolved, got {:?}",
                        original.tier
                    )
                }
            }
        }
    }

    #[test]
    fn fanout_ref_emits_n_edges_with_confidence_decay() {
        use ecp_core::analyzer::types::RawFanoutRef;

        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 8],
            nodes: vec![
                RawNode {
                    name: "dispatch".into(),
                    kind: NodeKind::Method,
                    span: (0, 0, 5, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                    owner_class: None,
                },
                RawNode {
                    name: "handle_a".into(),
                    kind: NodeKind::Method,
                    span: (10, 0, 12, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                    owner_class: None,
                },
                RawNode {
                    name: "handle_b".into(),
                    kind: NodeKind::Method,
                    span: (14, 0, 16, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                    owner_class: None,
                },
                RawNode {
                    name: "handle_c".into(),
                    kind: NodeKind::Method,
                    span: (18, 0, 20, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                    owner_class: None,
                },
            ],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![RawFanoutRef {
                source_name: "dispatch".into(),
                candidates: vec!["handle_a".into(), "handle_b".into(), "handle_c".into()],
                base_confidence: 0.5,
                reason: "reflection-getattr-fanout".into(),
                span: (0, 0, 0, 0),
            }],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        };

        let mut builder = GraphBuilder::new();
        builder.add_graph(g);
        let graph = builder.build();

        let fanout_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::References)
            .collect();

        // Expect 3 edges (one per candidate), each with confidence ≈ 0.5 / sqrt(3) ≈ 0.29.
        assert_eq!(
            fanout_edges.len(),
            3,
            "expected 3 fan-out edges, got {}",
            fanout_edges.len()
        );

        let expected_conf = 0.5_f32 / (3.0_f32).sqrt();
        for e in &fanout_edges {
            assert!(
                (e.confidence - expected_conf).abs() < 0.01,
                "expected conf ≈ {}, got {}",
                expected_conf,
                e.confidence
            );
            let reason_start = e.reason.offset as usize;
            let reason_end = reason_start + e.reason.len as usize;
            let reason_str = std::str::from_utf8(&graph.string_pool[reason_start..reason_end])
                .expect("reason utf-8");
            assert_eq!(reason_str, "reflection-getattr-fanout");
        }
    }

    #[test]
    fn fanout_ref_minimum_confidence_cap() {
        use ecp_core::analyzer::types::RawFanoutRef;

        // 60 candidates → 0.5/sqrt(60) ≈ 0.0645，應 cap 到 0.1
        let mut nodes = vec![RawNode {
            name: "dispatch".into(),
            kind: NodeKind::Method,
            span: (0, 0, 5, 0),
            is_exported: false,
            heritage: vec![],
            type_annotation: None,
            decorators: vec![],
            calls: vec![],
            owner_class: None,
        }];
        let mut candidates = vec![];
        for i in 0..60u32 {
            let name = format!("h{}", i);
            candidates.push(name.clone());
            nodes.push(RawNode {
                name,
                kind: NodeKind::Method,
                span: (10 + i, 0, 10 + i + 1, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                owner_class: None,
            });
        }
        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 8],
            nodes,
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![RawFanoutRef {
                source_name: "dispatch".into(),
                candidates,
                base_confidence: 0.5,
                reason: "reflection-getattr-fanout".into(),
                span: (0, 0, 0, 0),
            }],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        };
        let mut builder = GraphBuilder::new();
        builder.add_graph(g);
        let graph = builder.build();

        let fanout_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::References)
            .collect();

        assert_eq!(fanout_edges.len(), 60);
        for e in &fanout_edges {
            assert!(
                (e.confidence - 0.1).abs() < 1e-5,
                "expected cap 0.1, got {}",
                e.confidence
            );
        }
    }

    #[test]
    fn framework_ref_produces_edge_with_confidence_and_reason() {
        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 8],
            nodes: vec![
                RawNode {
                    name: "handler".into(),
                    kind: NodeKind::Function,
                    span: (0, 0, 0, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                    owner_class: None,
                },
                RawNode {
                    name: "get_db".into(),
                    kind: NodeKind::Function,
                    span: (0, 0, 0, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                    owner_class: None,
                },
            ],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            fanout_refs: vec![],
            framework_refs: vec![RawFrameworkRef {
                source_name: "handler".into(),
                target_name: "get_db".into(),
                confidence: 0.6,
                reason: "fastapi-depends".into(),
                span: (0, 0, 0, 0),
            }],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        };

        let mut builder = GraphBuilder::new();
        builder.add_graph(g);
        let graph = builder.build();

        let fw_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::References)
            .collect();
        assert_eq!(
            fw_edges.len(),
            1,
            "expected 1 References edge, got {}",
            fw_edges.len()
        );
        assert!((fw_edges[0].confidence - 0.6).abs() < 1e-6);
    }

    /// Build a single-node `LocalGraph` for end-to-end resolver tests.
    fn mk_file(path: &str, name: &str, kind: NodeKind, calls: Vec<String>) -> LocalGraph {
        LocalGraph {
            file_path: path.into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: name.into(),
                kind,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls,
                owner_class: None,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        }
    }

    /// Two same-named callables in different files must NOT both receive a
    /// CALLS edge from an ambiguous bare call site. Pin against fan-out
    /// regression for common names (`new` / `format` / `default` / ...).
    #[test]
    fn ambiguous_bare_callee_emits_no_calls_edge() {
        let mut builder = GraphBuilder::new();
        builder.add_graph(mk_file(
            "caller.rs",
            "caller_fn",
            NodeKind::Function,
            vec!["new".into()],
        ));
        builder.add_graph(mk_file("a.rs", "new", NodeKind::Method, vec![]));
        builder.add_graph(mk_file("b.rs", "new", NodeKind::Method, vec![]));
        let graph = builder.build();

        let calls_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Calls)
            .collect();
        assert_eq!(
            calls_edges.len(),
            0,
            "ambiguous bare callee must produce zero CALLS edges, got {}: {:?}",
            calls_edges.len(),
            calls_edges
        );
    }

    /// Sibling: a uniquely-named callable still resolves via Tier 3 — the cap
    /// suppresses fan-out, not all cross-file resolution.
    #[test]
    fn unique_global_callable_still_emits_calls_edge() {
        let mut builder = GraphBuilder::new();
        builder.add_graph(mk_file(
            "caller.rs",
            "caller_fn",
            NodeKind::Function,
            vec!["uniquely_named_helper".into()],
        ));
        builder.add_graph(mk_file(
            "lib.rs",
            "uniquely_named_helper",
            NodeKind::Function,
            vec![],
        ));
        let graph = builder.build();

        let calls_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Calls)
            .collect();
        assert_eq!(
            calls_edges.len(),
            1,
            "unique callable must emit exactly one CALLS edge"
        );
    }

    /// Task A acceptance: `LocalGraph.blind_spots` survive the builder pass
    /// and land in `ZeroCopyGraph.blind_spots` with all fields (kind /
    /// file_path / span / hint) preserved via the string pool. Locks in
    /// the contract that Task B (Python detector) and Task C (CLI
    /// surface) rely on.
    #[test]
    fn blind_spots_pass_through_to_graph() {
        use ecp_core::analyzer::types::BlindSpot;

        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 8],
            nodes: vec![],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![
                BlindSpot {
                    kind: "python-eval".into(),
                    file_path: "test.py".into(),
                    span: (10, 4, 10, 25),
                    hint: "eval(arg) — runtime code execution".into(),
                },
                BlindSpot {
                    kind: "python-dynamic-import".into(),
                    file_path: "test.py".into(),
                    span: (15, 0, 15, 40),
                    hint: "importlib.import_module(...) — dynamic loading".into(),
                },
            ],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        };

        let mut builder = GraphBuilder::new();
        builder.add_graph(g);
        let graph = builder.build();

        assert_eq!(
            graph.blind_spots.len(),
            2,
            "expected 2 blind spots in graph, got {}",
            graph.blind_spots.len()
        );

        let resolve = |sref: &ecp_core::pool::StrRef| -> &str {
            let start = sref.offset as usize;
            let end = start + sref.len as usize;
            std::str::from_utf8(&graph.string_pool[start..end]).expect("utf-8")
        };

        let kinds: Vec<&str> = graph
            .blind_spots
            .iter()
            .map(|bs| resolve(&bs.kind))
            .collect();
        assert!(kinds.contains(&"python-eval"));
        assert!(kinds.contains(&"python-dynamic-import"));

        // Spot-check the first record's span + file_path + hint round-trip.
        let bs0 = &graph.blind_spots[0];
        assert_eq!(resolve(&bs0.file_path), "test.py");
        assert_eq!(bs0.start_row, 10);
        assert_eq!(bs0.start_col, 4);
        assert_eq!(bs0.end_row, 10);
        assert_eq!(bs0.end_col, 25);
        assert_eq!(resolve(&bs0.hint), "eval(arg) — runtime code execution");
    }

    /// Pins the contract that Pass-2 emits the same edge set whether the
    /// dump-enabled serial path or the dump-disabled parallel path runs.
    ///
    /// Extended from the original aggregated-set assertion to:
    ///   (a) stratify per `RelType` so a divergence on one type doesn't hide
    ///       behind equality in another;
    ///   (b) include the resolved `reason` string in the equality predicate;
    ///   (c) add a `HandlesRoute` fixture to fire that emit branch;
    ///   (d) pin emit-zero invariant for Sub-projects 1/5 types
    ///       (Imports, Defines, Implements, Fetches).
    ///
    /// The fixtures cover these edge-emission categories:
    ///   * heritage (`Extends`) — Class with base
    ///   * calls (`Calls`) — Function with callee
    ///   * type_annotation (`Accesses`)
    ///   * framework_refs (`References` via Spring fixture)
    ///   * fanout_refs (`References` via reflection fixture)
    ///   * routes (`HandlesRoute`) — bar.rs exposes GET /users → other_fn
    #[test]
    fn pass2_parallel_serial_identical_per_reltype() {
        use ecp_core::analyzer::types::{RawFanoutRef, RawFrameworkRef, RawRoute};
        use std::collections::{BTreeMap, BTreeSet};

        fn build_fixtures() -> Vec<LocalGraph> {
            vec![
                LocalGraph {
                    file_path: "src/foo.rs".into(),
                    content_hash: [0; 8],
                    nodes: vec![RawNode {
                        name: "Foo".into(),
                        kind: NodeKind::Class,
                        span: (0, 0, 10, 0),
                        is_exported: true,
                        heritage: vec!["Bar".into()],
                        type_annotation: Some("Other".into()),
                        decorators: vec![],
                        calls: vec!["other_fn".into()],
                        owner_class: None,
                    }],
                    documents: vec![],
                    imports: vec![],
                    routes: vec![],
                    framework_refs: vec![RawFrameworkRef {
                        source_name: "Foo".into(),
                        target_name: "other_fn".into(),
                        confidence: 0.9,
                        reason: "spring-autowired".into(),
                        span: (1, 0, 1, 10),
                    }],
                    fanout_refs: vec![RawFanoutRef {
                        source_name: "Foo".into(),
                        candidates: vec!["other_fn".into(), "Bar".into()],
                        base_confidence: 0.6,
                        reason: "python-getattr".into(),
                        span: (2, 0, 2, 5),
                    }],
                    blind_spots: vec![],
                    schema_fields: None,
                    event_topics: None,
                    tx_scopes: None,
                    call_metas: vec![],
                    raw_function_metas: vec![],
                },
                LocalGraph {
                    file_path: "src/bar.rs".into(),
                    content_hash: [0; 8],
                    nodes: vec![
                        RawNode {
                            name: "Bar".into(),
                            kind: NodeKind::Class,
                            span: (0, 0, 5, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                            owner_class: None,
                        },
                        RawNode {
                            name: "Other".into(),
                            kind: NodeKind::Class,
                            span: (6, 0, 10, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                            owner_class: None,
                        },
                        RawNode {
                            name: "other_fn".into(),
                            kind: NodeKind::Function,
                            span: (11, 0, 12, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                            owner_class: None,
                        },
                    ],
                    documents: vec![],
                    imports: vec![],
                    routes: vec![RawRoute {
                        method: "GET".into(),
                        path: "/users".into(),
                        handler: Some("other_fn".into()),
                        span: (20, 0, 20, 30),
                    }],
                    framework_refs: vec![],
                    fanout_refs: vec![],
                    blind_spots: vec![],
                    schema_fields: None,
                    event_topics: None,
                    tx_scopes: None,
                    call_metas: vec![],
                    raw_function_metas: vec![],
                },
            ]
        }

        // Parallel path (production): no dump enabled
        let mut parallel_builder = GraphBuilder::new();
        for lg in build_fixtures() {
            parallel_builder.add_graph(lg);
        }
        let parallel_graph = parallel_builder.build();

        // Serial path: dump enabled forces the serial branch
        let tmp = tempfile::TempDir::new().unwrap();
        let dump_path = tmp.path().join("dump.jsonl");
        let mut serial_builder = GraphBuilder::new().with_resolver_dump(Some(dump_path.clone()));
        for lg in build_fixtures() {
            serial_builder.add_graph(lg);
        }
        let serial_graph = serial_builder.build();

        // Bucketize edges per RelType; include resolved reason in the key so a
        // diverging reason on the same (source, target) pair is caught.
        // `RelType` doesn't derive `Ord`, so `format!("{:?}", …)` is used as a
        // stable string key for the BTreeMap — the Debug repr of each variant
        // is its identifier name and is not subject to drift.
        let bucketize =
            |g: &ecp_core::graph::ZeroCopyGraph| -> BTreeMap<String, BTreeSet<(u32, u32, String)>> {
                let mut buckets: BTreeMap<String, BTreeSet<(u32, u32, String)>> = BTreeMap::new();
                for e in &g.edges {
                    let key = format!("{:?}", e.rel_type);
                    let reason = e.reason.resolve(&g.string_pool).to_string();
                    buckets
                        .entry(key)
                        .or_default()
                        .insert((e.source, e.target, reason));
                }
                buckets
            };

        let parallel_buckets = bucketize(&parallel_graph);
        let serial_buckets = bucketize(&serial_graph);

        // RelType key sets must match before per-bucket comparison.
        let p_keys: Vec<_> = parallel_buckets.keys().cloned().collect();
        let s_keys: Vec<_> = serial_buckets.keys().cloned().collect();
        assert_eq!(
            p_keys, s_keys,
            "parallel vs serial produced different RelType sets"
        );

        // Per-RelType equality — divergence is localised to the failing bucket.
        for (rel, p_edges) in &parallel_buckets {
            let s_edges = serial_buckets.get(rel).expect("rel exists in both");
            assert_eq!(
                p_edges, s_edges,
                "parallel vs serial diverged on RelType {rel}",
            );
        }

        // Emit-zero invariant: Sub-projects 1/5 types must not appear yet.
        // Update these assertions when those sub-projects ship.
        for unimplemented in &["Imports", "Defines", "Implements", "Fetches"] {
            assert!(
                !parallel_buckets.contains_key(*unimplemented),
                "RelType {unimplemented} unexpectedly emitted (parallel) — \
                 Sub-projects 1/5 will lift this; update this assertion when they ship",
            );
        }

        // Node counts identical (both paths build identical SymbolTable + StringPool)
        assert_eq!(parallel_graph.nodes.len(), serial_graph.nodes.len());

        // Sanity: dump file actually exists for the serial run (proves the
        // serial branch was the one taken).
        assert!(dump_path.exists(), "serial dump path was not taken");

        // Fixture coverage: assert each expected category fired at least once.
        for required in &["Calls", "Extends", "Accesses", "References", "HandlesRoute"] {
            assert!(
                parallel_buckets.contains_key(*required),
                "fixture failed to trigger {required} emit",
            );
        }
    }

    // ─── Pass 1.6: fetch-shape extraction ──────────────────────────────────

    /// Helper: materialise a file under `repo` and return a `LocalGraph` whose
    /// `file_path` is the relative form (matches the production
    /// `analyze.rs` flow). The graph carries an imperative-style
    /// `RawRoute` so Pass 1.5 emits a Route node we can attach a shape to.
    fn route_local_graph(
        rel_path: &str,
        method: &str,
        route_path: &str,
        handler: &str,
    ) -> LocalGraph {
        LocalGraph {
            file_path: rel_path.into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: handler.into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                owner_class: None,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![ecp_core::analyzer::types::RawRoute {
                method: method.into(),
                path: route_path.into(),
                handler: Some(handler.into()),
                span: (0, 0, 0, 0),
            }],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        }
    }

    fn consumer_local_graph(rel_path: &str) -> LocalGraph {
        LocalGraph {
            file_path: rel_path.into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: "loadUsers".into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                owner_class: None,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        }
    }

    /// Resolve a `StrRef` against the graph's archived string pool to a
    /// `String` so test assertions don't need to juggle byte offsets.
    fn s(graph: &ZeroCopyGraph, sref: StrRef) -> String {
        let start = sref.offset as usize;
        let end = start + sref.len as usize;
        std::str::from_utf8(&graph.string_pool[start..end])
            .expect("utf-8 in pool")
            .to_string()
    }

    /// TS route emitting `res.json({ id, name })` → RouteShape with
    /// response_keys `["id", "name"]` (sorted). Locks in that Pass 1.6a
    /// reads the source via `repo_root` and runs `response_shapes::extract`.
    #[test]
    fn ts_route_handler_emits_route_shape_response_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let route_file = "api/users.ts";
        std::fs::create_dir_all(tmp.path().join("api")).unwrap();
        std::fs::write(
            tmp.path().join(route_file),
            "function getUsers(req, res) { res.json({ id, name }); }",
        )
        .unwrap();

        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        builder.add_graph(route_local_graph(route_file, "get", "/users", "getUsers"));
        let graph = builder.build();

        assert_eq!(
            graph.route_shapes.len(),
            1,
            "expected exactly one RouteShape; got {}",
            graph.route_shapes.len()
        );
        let shape = &graph.route_shapes[0];
        // The Route node must be the one this shape points at.
        let route_node = &graph.nodes[shape.node_idx as usize];
        assert_eq!(route_node.kind, NodeKind::Route);

        let response_keys: Vec<String> =
            shape.response_keys.iter().map(|r| s(&graph, *r)).collect();
        assert_eq!(response_keys, vec!["id".to_string(), "name".to_string()]);
        assert!(
            shape.error_keys.is_empty(),
            "no error keys on 2xx handler; got {:?}",
            shape
                .error_keys
                .iter()
                .map(|r| s(&graph, *r))
                .collect::<Vec<_>>()
        );
    }

    /// Consumer file with `fetch('/users')` + `data.id` access → a
    /// `RelType::Fetches` edge whose reason is `fetch-url-match|keys:id`.
    /// Covers the full 1.6b flow: URL extraction, route-table match,
    /// consumer-key extraction, reason formatting.
    #[test]
    fn ts_consumer_emits_fetches_edge_with_keyed_reason() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("api.ts"),
            "function getUsers(req, res) { res.json({ id, name }); }",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("client.ts"),
            "async function loadUsers() { const { id } = await fetch('/users').json(); }",
        )
        .unwrap();

        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        builder.add_graph(route_local_graph("api.ts", "get", "/users", "getUsers"));
        builder.add_graph(consumer_local_graph("client.ts"));
        let graph = builder.build();

        let fetches: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Fetches)
            .collect();
        assert_eq!(
            fetches.len(),
            1,
            "expected exactly one Fetches edge; got {} (edges: {:?})",
            fetches.len(),
            graph
                .edges
                .iter()
                .map(|e| (e.source, e.target, format!("{:?}", e.rel_type)))
                .collect::<Vec<_>>()
        );
        let edge = fetches[0];
        assert!(
            (edge.confidence - 0.9).abs() < 1e-6,
            "Fetches confidence must be 0.9 for exact-path matches; got {}",
            edge.confidence
        );
        // Target must be the Route node.
        assert_eq!(graph.nodes[edge.target as usize].kind, NodeKind::Route);

        let reason = s(&graph, edge.reason);
        assert_eq!(reason, "fetch-url-match|keys:id");
    }

    /// PHP route emitting `json_encode(['id' => $x])` → RouteShape
    /// with response_keys `["id"]`. Covers the Lang::Php branch of
    /// `response_shapes::extract` end-to-end through the builder.
    #[test]
    fn php_route_handler_emits_route_shape_response_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let route_file = "api/show.php";
        std::fs::create_dir_all(tmp.path().join("api")).unwrap();
        std::fs::write(
            tmp.path().join(route_file),
            "<?php $x = 1; echo json_encode(['id' => $x]); ?>",
        )
        .unwrap();

        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        builder.add_graph(route_local_graph(route_file, "get", "/show", "showHandler"));
        let graph = builder.build();

        assert_eq!(
            graph.route_shapes.len(),
            1,
            "expected exactly one RouteShape; got {}",
            graph.route_shapes.len()
        );
        let shape = &graph.route_shapes[0];
        let response_keys: Vec<String> =
            shape.response_keys.iter().map(|r| s(&graph, *r)).collect();
        assert_eq!(response_keys, vec!["id".to_string()]);
    }
}
