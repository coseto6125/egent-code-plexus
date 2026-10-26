use super::provider::LanguageProvider;
use super::types::LocalGraph;
use crate::contracts::routing::{
    FileRoute, FileRouter, Language, ParsedLanguage, RouteOutcome, SizeCap,
};
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
    /// returned `LocalGraph.file_path`. Source over the cap returns the same
    /// `file-too-large` omission that [`Self::analyze`] records.
    pub fn parse_file_raw(
        &self,
        rel_path: &std::path::Path,
        source: &[u8],
    ) -> anyhow::Result<LocalGraph> {
        let Some(route) = self.route(rel_path) else {
            return Err(anyhow::anyhow!("no provider for {:?}", rel_path));
        };
        let chain = self.provider_chain(&route);
        if !chain.is_empty() && !route.size_cap().admits(source.len() as u64) {
            return Ok(Self::too_large(
                rel_path,
                source.len() as u64,
                route.size_cap(),
            ));
        }
        Self::parse_chain(&route, &chain, rel_path, source)
            .map(|(_, result)| result)
            .unwrap_or_else(|| Err(anyhow::anyhow!("no provider for {:?}", rel_path)))
    }

    fn too_large(rel_path: &std::path::Path, len: u64, size_cap: SizeCap) -> LocalGraph {
        LocalGraph::omitted(
            rel_path,
            "file-too-large",
            format!(
                "{len} bytes exceeds the {}-byte per-file cap, so nothing in this file is in \
                 the graph. Raise `[index] max_file_bytes` or `ECP_MAX_FILE_BYTES` to include it.",
                size_cap.bytes()
            ),
        )
    }

    // A non-UTF-8 path is skipped, never routed through a lossy spelling:
    // the lossy name names no file on disk (C3 `SkipReason::NotUtf8`).
    fn route(&self, rel_path: &std::path::Path) -> Option<FileRoute> {
        match self.router.route(rel_path.to_str()?) {
            RouteOutcome::Routed(route) => Some(route),
            RouteOutcome::Excluded(_) | RouteOutcome::Unrouted => None,
        }
    }

    /// The registered providers to try for `route`, in the order of
    /// [`FileRoute::providers`].
    fn provider_chain(&self, route: &FileRoute) -> Vec<&dyn LanguageProvider> {
        route
            .providers()
            .filter_map(|name| {
                self.providers
                    .iter()
                    .find(|provider| provider.name() == name)
                    .map(|provider| provider.as_ref())
            })
            .collect()
    }

    /// Run the chain until a provider accepts: its graph holds a node or a
    /// blind spot. Every provider content-gates by returning an empty graph,
    /// so an empty result hands the file to the next candidate, and the first
    /// result stands when none accepts (today's single-provider answer). An
    /// error ends the chain. `None` when the chain is empty.
    ///
    /// Both parse entries, [`Self::parse_file_raw`] (incremental) and
    /// [`Self::analyze_with_cache`] (cold), go through here, so the graph
    /// they return has its records bound and its language after parse set
    /// ([`finish`]).
    fn parse_chain(
        route: &FileRoute,
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
                let language = route
                    .candidates()
                    .iter()
                    .copied()
                    .find(|language| language.provider() == provider.name());
                return Some((provider.name(), finish(route, language, result)));
            }
            fallback.get_or_insert((provider.name(), result));
        }
        // No provider accepted: a fixed route keeps its language, and a YAML
        // no content gate accepted is `Yaml`. A JSON file no gate accepted
        // has none.
        let unaccepted = route.fixed_language().or_else(|| {
            route
                .candidates()
                .last()
                .copied()
                .filter(|language| *language == Language::Yaml)
        });
        fallback.map(|(name, result)| (name, finish(route, unaccepted, result)))
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
                                    let _ = sender.send(Self::too_large(
                                        &rel_path,
                                        meta.len(),
                                        route.size_cap(),
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
                            Self::parse_chain(&route, &chain, &rel_path, source)
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

/// What the pipeline does with one provider's graph once the provider has
/// returned, so its node list is final: C4 `ParsePhase::BindRecords`, then
/// the C5 language after parse. `accepted` is the candidate whose provider
/// accepted the content; the script the provider reported
/// ([`LocalGraph::reported_script`]) decides the resolution language. No
/// accepted language, or a report the language cannot embed, stores
/// [`ParsedLanguage::ABSENT`].
fn finish(
    route: &FileRoute,
    accepted: Option<Language>,
    result: anyhow::Result<LocalGraph>,
) -> anyhow::Result<LocalGraph> {
    result.map(|mut graph| {
        graph.bind_records();
        let parsed =
            accepted.and_then(|language| route.after_parse(language, graph.reported_script()).ok());
        graph.parsed_language = ParsedLanguage::encode(parsed);
        graph
    })
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

    /// The raw path (overlay writer, `impact --baseline`) and the walk path
    /// agree on the cap: over it, both return the `file-too-large` omission.
    /// Failed before this change: `parse_file_raw` parsed any length.
    #[test]
    fn test_parse_file_raw_over_cap_returns_file_too_large_omission() {
        let mut pipeline =
            AnalyzerPipeline::new().with_size_cap(SizeCap::resolve(Some("10"), None));
        pipeline.register_provider(Box::new(TypeScriptProvider));

        let big = pipeline
            .parse_file_raw(std::path::Path::new("big.ts"), b"AAAAAAAAAAAAAAAAA")
            .expect("an oversized routed file is an omission, not an error");
        assert!(big.nodes.is_empty());
        assert_eq!(
            big.blind_spots
                .iter()
                .map(|b| b.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["file-too-large"]
        );
        assert_eq!(big.content_hash, [0u8; 8]);

        let at_cap = pipeline
            .parse_file_raw(std::path::Path::new("at_cap.ts"), b"let a = 1;")
            .expect("a file of exactly the cap is parsed");
        assert!(at_cap.blind_spots.is_empty());

        assert!(pipeline
            .parse_file_raw(std::path::Path::new("notes.txt"), b"AAAAAAAAAAAAAAAAA")
            .is_err());
    }

    /// A provider that returns a fixed graph, so a test plants what the parse
    /// reports.
    struct Planted {
        name: &'static str,
        script: Option<bool>,
    }

    impl LanguageProvider for Planted {
        fn name(&self) -> &'static str {
            self.name
        }

        fn parse_file(&self, path: &std::path::Path, _source: &[u8]) -> anyhow::Result<LocalGraph> {
            let mut graph = LocalGraph {
                file_path: path.to_path_buf(),
                ..LocalGraph::default()
            };
            if let Some(is_ts) = self.script {
                graph.note_script(is_ts, (0, 0, 0, 0));
            }
            Ok(graph)
        }
    }

    fn parsed_of(provider: Planted, path: &str) -> [u8; 2] {
        let mut pipeline = AnalyzerPipeline::new();
        pipeline.register_provider(Box::new(provider));
        pipeline
            .parse_file_raw(std::path::Path::new(path), b"x")
            .expect("parse")
            .parsed_language
    }

    /// A fixed route stores the language its provider parsed, also for an
    /// empty graph (no provider accepted).
    #[test]
    fn test_parse_file_raw_fixed_route_stores_path_language() {
        let stored = parsed_of(
            Planted {
                name: "typescript",
                script: None,
            },
            "src/a.ts",
        );
        let parsed = ParsedLanguage::from_bytes(stored).expect("a language is stored");
        assert_eq!(
            (parsed.file(), parsed.resolution()),
            (Language::TypeScript, Language::TypeScript)
        );
    }

    /// A single-file component's reported script becomes the resolution
    /// language; the file language stays the component.
    #[test]
    fn test_parse_file_raw_component_script_stores_resolution_language() {
        for (is_ts, expected) in [(true, Language::TypeScript), (false, Language::JavaScript)] {
            let stored = parsed_of(
                Planted {
                    name: "vue",
                    script: Some(is_ts),
                },
                "src/App.vue",
            );
            let parsed = ParsedLanguage::from_bytes(stored).expect("a language is stored");
            assert_eq!(
                (parsed.file(), parsed.resolution()),
                (Language::Vue, expected)
            );
        }
    }

    /// `after_parse` rejects a script on a language that embeds none: the
    /// graph stores no language rather than an invented one, and the report
    /// byte does not leak into the stored field.
    #[test]
    fn test_parse_file_raw_script_on_plain_language_stores_absent() {
        let stored = parsed_of(
            Planted {
                name: "typescript",
                script: Some(true),
            },
            "src/a.ts",
        );
        assert_eq!(stored, ParsedLanguage::ABSENT);
    }

    /// No content gate of a YAML route accepted the file: it is `Yaml`. A JSON
    /// file no gate accepted has no language.
    #[test]
    fn test_parse_file_raw_unaccepted_sniffed_route_stores_yaml_or_absent() {
        let mut yaml = AnalyzerPipeline::new();
        yaml.register_provider(Box::new(Planted {
            name: "kubernetes",
            script: None,
        }));
        yaml.register_provider(Box::new(Planted {
            name: "openapi",
            script: None,
        }));
        let graph = yaml
            .parse_file_raw(std::path::Path::new("config/app.yaml"), b"name: app\n")
            .expect("parse");
        let parsed = ParsedLanguage::from_bytes(graph.parsed_language).expect("Yaml is stored");
        assert_eq!(
            (parsed.file(), parsed.resolution()),
            (Language::Yaml, Language::Yaml)
        );

        let mut json = AnalyzerPipeline::new();
        json.register_provider(Box::new(Planted {
            name: "openapi",
            script: None,
        }));
        let graph = json
            .parse_file_raw(std::path::Path::new("api/data.json"), b"{}")
            .expect("parse");
        assert_eq!(graph.parsed_language, ParsedLanguage::ABSENT);
    }

    #[test]
    fn test_route_yaml_candidates_put_kubernetes_first() {
        let first = |path: &str| {
            let RouteOutcome::Routed(route) = FileRouter::PATH_ONLY.route(path) else {
                panic!("{path} is routed");
            };
            route
                .candidates()
                .first()
                .map(|language| language.provider())
        };
        // Plain `.yaml`/`.yml` → kubernetes (content-sniffed inside the provider).
        assert_eq!(first("k8s/deploy.yaml"), Some("kubernetes"));
        assert_eq!(first("manifest.yml"), Some("kubernetes"));
        // Filename/path specials still win over the kubernetes fallback.
        assert_eq!(first("docker-compose.yml"), Some("docker-compose"));
        assert_eq!(first(".github/workflows/ci.yml"), Some("github-actions"));
    }
}
