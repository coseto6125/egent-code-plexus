use super::provider::LanguageProvider;
use super::types::LocalGraph;
use crate::contracts::routing::{FileRoute, FileRouter, Language, RouteOutcome, SizeCap};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Holds the bytes for one parsed file. `Mmap` is the zero-copy fast path;
/// `Owned` is the `fs::read` fallback for small files or filesystems lacking
/// mmap. Both deref to `&[u8]` for the parser + hasher.
enum SourceBuf {
    Mmap(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl SourceBuf {
    #[inline]
    fn as_slice(&self) -> &[u8] {
        match self {
            SourceBuf::Mmap(m) => m,
            SourceBuf::Owned(v) => v,
        }
    }
}

pub struct AnalyzerPipeline {
    providers: Vec<Box<dyn LanguageProvider>>,
    /// Routes every path and holds the one per-file byte cap. A file over the
    /// cap would otherwise materialise straight into memory via `fs::read`
    /// (a 100 MiB minified bundle), bounding worst-case RAM at
    /// `num_threads * cap`.
    router: FileRouter,
}

impl Default for AnalyzerPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalyzerPipeline {
    /// The cap is `ECP_MAX_FILE_BYTES`, else the built-in default. A caller
    /// that knows the repo config passes it with [`Self::with_size_cap`].
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            router: FileRouter::new(crate::config::env_size_cap(None)),
        }
    }

    /// Replace the cap, so the parse step and the walk that fed it read the
    /// same one.
    pub fn with_size_cap(mut self, size_cap: SizeCap) -> Self {
        self.router = FileRouter::new(size_cap);
        self
    }

    pub fn register_provider(&mut self, provider: Box<dyn LanguageProvider>) {
        self.providers.push(provider);
    }

    /// Number of registered providers. Lets callers assert a subset pipeline
    /// holds the expected language count without exposing the provider list.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Parse a single file from in-memory bytes without touching disk.
    /// The `rel_path` is used only for routing and is stored verbatim in the
    /// returned `LocalGraph.file_path`.
    pub fn parse_file_raw(
        &self,
        rel_path: &std::path::Path,
        source: &[u8],
    ) -> anyhow::Result<LocalGraph> {
        let chain = self
            .route(rel_path)
            .map(|route| self.provider_chain(&route))
            .unwrap_or_default();
        Self::parse_chain(&chain, rel_path, source)
            .map(|(_, result)| result)
            .unwrap_or_else(|| Err(anyhow::anyhow!("no provider for {:?}", rel_path)))
    }

    // A non-UTF-8 path is skipped, never routed through a lossy spelling:
    // the lossy name names no file on disk (C3 `SkipReason::NotUtf8`).
    fn route(&self, rel_path: &std::path::Path) -> Option<FileRoute> {
        match self.router.route(rel_path.to_str()?) {
            RouteOutcome::Routed(route) => Some(route),
            RouteOutcome::Excluded(_) | RouteOutcome::Unrouted => None,
        }
    }

    /// The registered providers to try for `route`, in order. A sniffed route
    /// ends its chain before `Yaml`: a generic YAML gets no graph from the
    /// cold index (C5 Open point 6 waits for a node-count measurement), but
    /// an OpenAPI YAML reaches the OpenAPI provider.
    fn provider_chain(&self, route: &FileRoute) -> Vec<&dyn LanguageProvider> {
        route
            .candidates()
            .iter()
            .filter(|language| **language != Language::Yaml)
            .filter_map(|language| {
                self.providers
                    .iter()
                    .find(|provider| provider.name() == language.provider())
                    .map(|provider| provider.as_ref())
            })
            .collect()
    }

    /// Run the chain until a provider accepts: its graph holds a node or a
    /// blind spot. Every provider content-gates by returning an empty graph,
    /// so an empty result hands the file to the next candidate, and the first
    /// result stands when none accepts (today's single-provider answer). An
    /// error ends the chain. `None` when the chain is empty.
    fn parse_chain(
        chain: &[&dyn LanguageProvider],
        rel_path: &std::path::Path,
        source: &[u8],
    ) -> Option<(&'static str, anyhow::Result<LocalGraph>)> {
        let mut fallback = None;
        for provider in chain {
            let result = provider.parse_file(rel_path, source);
            let accepted = !matches!(
                &result,
                Ok(graph) if graph.nodes.is_empty() && graph.blind_spots.is_empty()
            );
            if accepted {
                return Some((provider.name(), result));
            }
            fallback.get_or_insert((provider.name(), result));
        }
        fallback
    }

    /// The provider `name` that parses `path`: the first candidate of its
    /// route. The incremental path (`reanalyze`) uses it to build a pipeline
    /// holding ONLY the providers a dirty set needs, instead of paying the
    /// full tree-sitter `Query` compile (~0.65s) to reparse one changed file
    /// (~8ms). A thin view of [`FileRouter::route`]; it goes when its callers
    /// read the route.
    pub fn provider_name_for_path(path: &std::path::Path) -> Option<&'static str> {
        const ROUTER: FileRouter = FileRouter::new(SizeCap::BUILT_IN);
        let RouteOutcome::Routed(route) = ROUTER.route(path.to_str()?) else {
            return None;
        };
        route
            .candidates()
            .first()
            .map(|language| language.provider())
    }

    /// Analyze files concurrently using a Multi-Producer Single-Consumer architecture
    pub fn analyze(&self, files: Vec<(PathBuf, PathBuf)>) -> Vec<LocalGraph> {
        // No-cache fast path — the closure short-circuits in the inner
        // hot loop so callers without a cache pay zero overhead vs the
        // pre-cache implementation.
        self.analyze_with_cache(files, |_, _| None)
    }

    /// Same as [`analyze`] but each file's `(rel_path, content_hash)` is
    /// first offered to `cache_lookup`; on `Some(local_graph)` the
    /// tree-sitter parse is skipped entirely and the cached result is
    /// emitted verbatim.
    ///
    /// `cache_lookup` runs on every rayon worker thread, so it must be
    /// `Send + Sync`. A typical implementation captures a
    /// `&CacheIndex` (`FxHashMap` lookup) by reference.
    pub fn analyze_with_cache<F>(
        &self,
        files: Vec<(PathBuf, PathBuf)>,
        cache_lookup: F,
    ) -> Vec<LocalGraph>
    where
        F: Fn(&std::path::Path, &[u8; 8]) -> Option<LocalGraph> + Send + Sync,
    {
        let (tx, rx) = crossbeam_channel::unbounded::<LocalGraph>();
        let cache_lookup = &cache_lookup;
        let prof = std::env::var("ECP_PROF").is_ok();
        // Per-provider (count, total_ns) when ECP_PROF=1. Mutex is fine —
        // critical section is just a HashMap update, negligible vs the
        // parse_file work it brackets.
        let times_owned: Option<Mutex<FxHashMap<&'static str, (u64, u64)>>> = if prof {
            Some(Mutex::new(FxHashMap::default()))
        } else {
            None
        };
        let times = times_owned.as_ref();

        // Producer (A): parse files concurrently
        rayon::scope(|s| {
            s.spawn(|_| {
                files
                    .into_par_iter()
                    .for_each_with(tx, |sender, (abs_path, rel_path)| {
                        let Some(route) = self.route(&rel_path) else {
                            return;
                        };
                        let chain = self.provider_chain(&route);
                        if chain.is_empty() {
                            return;
                        }
                        // Skip oversized files before reading to keep the
                        // worker thread from materialising a multi-GiB blob
                        // into memory. metadata() is one fstat — cheap.
                        // Every give-up below reports itself. A silent `return`
                        // left the file with no graph and no record, the index
                        // published as complete, and `ecp find` answered
                        // `found: false` for a symbol that is in the tree.
                        let file_len = match std::fs::metadata(&abs_path) {
                            Ok(meta) => {
                                if !route.size_cap().admits(meta.len()) {
                                    let _ = sender.send(LocalGraph::omitted(
                                        &rel_path,
                                        "file-too-large",
                                        format!(
                                            "{} bytes exceeds the {}-byte per-file cap, \
                                             so nothing in this file is in the graph. Raise \
                                             `[index] max_file_bytes` or `ECP_MAX_FILE_BYTES` to include it.",
                                            meta.len(),
                                            route.size_cap().bytes()
                                        ),
                                    ));
                                    return;
                                }
                                meta.len()
                            }
                            Err(e) => {
                                let _ = sender.send(LocalGraph::omitted(
                                    &rel_path,
                                    "file-unreadable",
                                    format!("could not stat the file: {e}"),
                                ));
                                return;
                            }
                        };

                        // CI-E: mmap source bytes instead of `fs::read`. The
                        // tree-sitter parsers + content hasher consume `&[u8]`,
                        // so mmap drops the user-space copy. Fall back to
                        // `fs::read` on:
                        //   - small files (<4KB ≈ one page) where mmap setup
                        //     cost outweighs the saved copy
                        //   - mmap failure (filesystems lacking mmap, e.g.
                        //     some FUSE mounts, /proc, /sys)
                        //   - `ECP_NO_MMAP=1` env kill-switch
                        const MMAP_MIN_BYTES: u64 = 4096;
                        let use_mmap =
                            file_len >= MMAP_MIN_BYTES && std::env::var_os("ECP_NO_MMAP").is_none();
                        let source_buf: SourceBuf = if use_mmap {
                            match std::fs::File::open(&abs_path)
                                .and_then(|f| unsafe { memmap2::Mmap::map(&f) })
                            {
                                Ok(m) => SourceBuf::Mmap(m),
                                Err(_) => match std::fs::read(&abs_path) {
                                    Ok(v) => SourceBuf::Owned(v),
                                    Err(e) => {
                                        let _ = sender.send(LocalGraph::omitted(
                                            &rel_path,
                                            "file-unreadable",
                                            format!("could not read the file: {e}"),
                                        ));
                                        return;
                                    }
                                },
                            }
                        } else {
                            match std::fs::read(&abs_path) {
                                Ok(v) => SourceBuf::Owned(v),
                                Err(e) => {
                                    let _ = sender.send(LocalGraph::omitted(
                                        &rel_path,
                                        "file-unreadable",
                                        format!("could not read the file: {e}"),
                                    ));
                                    return;
                                }
                            }
                        };
                        let source: &[u8] = source_buf.as_slice();

                        let content_hash: [u8; 8] =
                            xxhash_rust::xxh3::xxh3_64(source).to_le_bytes();

                        // Cache fast-path: skip parse if a hit comes back
                        // with the exact same content hash. Path is the
                        // rel_path (matches what gets stored on save).
                        if let Some(cached) = cache_lookup(&rel_path, &content_hash) {
                            let _ = sender.send(cached);
                            return;
                        }

                        // Cache miss / no cache: regular parse path.
                        let t_parse = std::time::Instant::now();
                        let Some((provider_name, result)) =
                            Self::parse_chain(&chain, &rel_path, source)
                        else {
                            return;
                        };
                        let parse_ns = t_parse.elapsed().as_nanos() as u64;
                        if let Some(t) = times {
                            let mut m = t.lock().unwrap();
                            let entry = m.entry(provider_name).or_insert((0u64, 0u64));
                            entry.0 += 1;
                            entry.1 += parse_ns;
                        }
                        match result {
                            Ok(mut local_graph) => {
                                local_graph.content_hash = content_hash;
                                let _ = sender.send(local_graph);
                            }
                            Err(e) => {
                                let _ = sender.send(LocalGraph::omitted(
                                    &rel_path,
                                    "parse-failed",
                                    format!("the {provider_name} parser returned an error: {e}"),
                                ));
                            }
                        }
                    });
            });
        });

        // Consumer (B): collect and build (Builder thread)
        let mut all_graphs = Vec::new();
        while let Ok(graph) = rx.recv() {
            all_graphs.push(graph);
        }

        if let Some(t) = times_owned {
            let m = t.into_inner().unwrap();
            let mut rows: Vec<_> = m.into_iter().collect();
            // Sort by total ns descending — surface the hot providers first.
            rows.sort_by_key(|(_, (_, ns))| std::cmp::Reverse(*ns));
            eprintln!("prof per-provider parse_file:");
            for (name, (n, ns)) in rows {
                let per_file_us = ns.checked_div(n).unwrap_or(0) / 1000;
                eprintln!(
                    "  {:<16} n={:>6}  total={:>7.2}s  per-file={}µs",
                    name,
                    n,
                    ns as f64 / 1e9,
                    per_file_us
                );
            }
        }

        all_graphs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::typescript::TypeScriptProvider;

    #[test]
    fn test_pipeline_execution() {
        // Pipeline calls std::fs::read on each abs_path, so the test must
        // materialise real files. Use a tempdir + emit empty `.ts`/`.txt`.
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.ts");
        let b = tmp.path().join("b.ts");
        let c = tmp.path().join("c.txt");
        std::fs::write(&a, b"").unwrap();
        std::fs::write(&b, b"").unwrap();
        std::fs::write(&c, b"").unwrap();

        let mut pipeline = AnalyzerPipeline::new();
        pipeline.register_provider(Box::new(TypeScriptProvider));

        let files = vec![
            (a.clone(), PathBuf::from("a.ts")),
            (b.clone(), PathBuf::from("b.ts")),
            (c.clone(), PathBuf::from("c.txt")), // Should be ignored based on extension
        ];

        let results = pipeline.analyze(files);

        // We expect only 2 .ts files to be processed
        assert_eq!(results.len(), 2);

        let paths: Vec<_> = results
            .iter()
            .map(|g| g.file_path.to_str().unwrap())
            .collect();
        assert!(paths.contains(&"a.ts"));
        assert!(paths.contains(&"b.ts"));
    }

    /// Files exceeding `ECP_MAX_FILE_BYTES` must be skipped silently so a
    /// rogue multi-GiB source can't OOM the process. Verify with a tiny
    /// cap (10 bytes) that an 11-byte file is excluded from results.
    #[test]
    fn oversize_file_is_skipped() {
        // SAFETY: set_var is `unsafe` in 2024 ed. The pipeline reads the env
        // var once before spawning rayon workers, so racing with other tests
        // (which don't touch this var) is not a concern here.
        unsafe { std::env::set_var("ECP_MAX_FILE_BYTES", "10") };

        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("big.ts");
        let small = tmp.path().join("small.ts");
        std::fs::write(&big, b"AAAAAAAAAAAAAAAAA").unwrap(); // 17 bytes > 10
        std::fs::write(&small, b"x").unwrap(); // 1 byte ≤ 10

        let mut pipeline = AnalyzerPipeline::new();
        pipeline.register_provider(Box::new(TypeScriptProvider));

        let files = vec![
            (big.clone(), PathBuf::from("big.ts")),
            (small.clone(), PathBuf::from("small.ts")),
        ];
        let results = pipeline.analyze(files);

        // The contract changed from "the oversized file is absent" to "it is
        // present carrying only the reason it was not parsed". Absence was the
        // defect: it left the caller unable to tell a file that has no symbols
        // from one nobody looked at.
        let big = results
            .iter()
            .find(|g| g.file_path.to_str() == Some("big.ts"))
            .expect("the skipped file must still be reported");
        assert!(
            big.nodes.is_empty(),
            "a file over the cap must not be parsed; got {} nodes",
            big.nodes.len()
        );
        assert_eq!(
            big.blind_spots
                .iter()
                .map(|b| b.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["file-too-large"],
            "the reason has to travel with it"
        );
        assert!(
            big.content_hash == [0u8; 8],
            "content that was never read must not get a hash the parse cache trusts"
        );

        let small = results
            .iter()
            .find(|g| g.file_path.to_str() == Some("small.ts"))
            .expect("1-byte file must still pass through");
        assert!(small.blind_spots.is_empty());

        unsafe { std::env::remove_var("ECP_MAX_FILE_BYTES") };
    }

    #[test]
    fn yaml_routes_to_kubernetes_provider() {
        use std::path::Path;
        let pn = AnalyzerPipeline::provider_name_for_path;
        // Plain `.yaml`/`.yml` → kubernetes (content-sniffed inside the provider).
        assert_eq!(pn(Path::new("k8s/deploy.yaml")), Some("kubernetes"));
        assert_eq!(pn(Path::new("manifest.yml")), Some("kubernetes"));
        // Filename/path specials still win over the kubernetes fallback.
        assert_eq!(pn(Path::new("docker-compose.yml")), Some("docker-compose"));
        assert_eq!(
            pn(Path::new(".github/workflows/ci.yml")),
            Some("github-actions")
        );
    }
}
