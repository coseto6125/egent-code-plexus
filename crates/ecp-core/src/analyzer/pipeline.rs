use super::provider::LanguageProvider;
use super::types::LocalGraph;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Skip source files larger than this cap. A 100 MiB minified JS bundle or a
/// pathological generated source can otherwise materialise straight into
/// memory via `std::fs::read` and OOM the process. 16 MiB is well above any
/// hand-written source we've seen while still bounding worst-case RAM at
/// `num_threads * 16 MiB`. Override at runtime via `ECP_MAX_FILE_BYTES`.
const MAX_FILE_BYTES_DEFAULT: u64 = 16 * 1024 * 1024;

fn resolve_max_file_bytes() -> u64 {
    std::env::var("ECP_MAX_FILE_BYTES")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(MAX_FILE_BYTES_DEFAULT)
}

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

#[derive(Default)]
pub struct AnalyzerPipeline {
    providers: Vec<Box<dyn LanguageProvider>>,
}

impl AnalyzerPipeline {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
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
    /// The `rel_path` is used only for provider selection (extension lookup)
    /// and is stored verbatim in the returned `LocalGraph.file_path`.
    pub fn parse_file_raw(
        &self,
        rel_path: &std::path::Path,
        source: &[u8],
    ) -> anyhow::Result<LocalGraph> {
        let provider = self
            .find_provider(rel_path)
            .ok_or_else(|| anyhow::anyhow!("no provider for {:?}", rel_path))?;
        provider.parse_file(rel_path, source)
    }

    fn find_provider(&self, path: &std::path::Path) -> Option<&dyn LanguageProvider> {
        let name = Self::provider_name_for_path(path)?;
        self.providers
            .iter()
            .find(|p| p.name() == name)
            .map(|p| p.as_ref())
    }

    /// The single source of truth mapping a file path to the provider `name`
    /// that should parse it. `find_provider` resolves this name against the
    /// registered providers; the incremental path (`reanalyze`) uses it to
    /// build a pipeline holding ONLY the providers a dirty set needs, instead
    /// of paying the full 20-provider tree-sitter `Query` compile (~0.65s) to
    /// reparse one changed file (~8ms). Path-based special cases (Dockerfile,
    /// docker-compose, GitHub Actions workflows) are resolved before the
    /// extension table. `.h` routes to C++ (near-superset of C); see the
    /// indexing-pipeline note that made this the load-bearing dispatch.
    pub fn provider_name_for_path(path: &std::path::Path) -> Option<&'static str> {
        let file_name = path.file_name()?.to_str()?;
        if matches!(file_name, "Dockerfile" | "dockerfile") {
            return Some("dockerfile");
        }
        if matches!(
            file_name,
            "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
        ) {
            return Some("docker-compose");
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if matches!(ext, "yml" | "yaml") {
                let is_gha = path
                    .components()
                    .collect::<Vec<_>>()
                    .windows(2)
                    .any(|w| w[0].as_os_str() == ".github" && w[1].as_os_str() == "workflows");
                if is_gha {
                    return Some("github-actions");
                }
            }
        }

        let ext = path.extension()?.to_str()?;
        Some(match ext {
            "ts" | "tsx" => "typescript",
            "py" | "pyi" => "python",
            "go" => "go",
            "rs" => "rust",
            "java" => "java",
            "js" | "jsx" | "mjs" | "cjs" => "javascript",
            "php" => "php",
            "rb" => "ruby",
            "kt" | "kts" => "kotlin",
            "cs" => "c_sharp",
            "c" => "c",
            "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" | "h" => "cpp",
            "swift" => "swift",
            "dart" => "dart",
            "sh" | "bash" => "bash",
            "lua" | "luau" => "lua",
            "dockerfile" => "dockerfile",
            "cr" => "crystal",
            "move" => "move",
            "sol" => "solidity",
            "tf" | "tfvars" | "hcl" => "hcl",
            "nim" => "nim",
            "sql" => "sql",
            "vy" => "vyper",
            "cairo" => "cairo",
            "v" | "sv" | "vh" | "svh" => "verilog",
            "zig" => "zig",
            "vue" => "vue",
            "astro" => "astro",
            "svelte" => "svelte",
            "proto" => "protobuf",
            // `.json` routes to the OpenAPI provider; it applies a cheap
            // 200-byte prefix gate so non-OpenAPI JSON costs near-zero.
            "json" => "openapi",
            _ => return None,
        })
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
        let max_file_bytes = resolve_max_file_bytes();
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
                        // Single `find_provider` lookup, reused below — avoids
                        // the double-lookup the original `is_none()` gate +
                        // re-`match` pattern paid per file.
                        let Some(provider) = self.find_provider(&rel_path) else {
                            return;
                        };
                        // Skip oversized files before reading to keep the
                        // worker thread from materialising a multi-GiB blob
                        // into memory. metadata() is one fstat — cheap.
                        let file_len = match std::fs::metadata(&abs_path) {
                            Ok(meta) => {
                                if meta.len() > max_file_bytes {
                                    return;
                                }
                                meta.len()
                            }
                            Err(_) => return,
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
                                    Err(_) => return,
                                },
                            }
                        } else {
                            match std::fs::read(&abs_path) {
                                Ok(v) => SourceBuf::Owned(v),
                                Err(_) => return,
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
                        let result = provider.parse_file(&rel_path, source);
                        let parse_ns = t_parse.elapsed().as_nanos() as u64;
                        if let Some(t) = times {
                            let mut m = t.lock().unwrap();
                            let entry = m.entry(provider.name()).or_insert((0u64, 0u64));
                            entry.0 += 1;
                            entry.1 += parse_ns;
                        }
                        if let Ok(mut local_graph) = result {
                            local_graph.content_hash = content_hash;
                            let _ = sender.send(local_graph);
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

        let paths: Vec<_> = results
            .iter()
            .map(|g| g.file_path.to_str().unwrap().to_string())
            .collect();
        assert!(
            !paths.iter().any(|p| p == "big.ts"),
            "17-byte file must be skipped under 10-byte cap; got {:?}",
            paths
        );
        assert!(
            paths.iter().any(|p| p == "small.ts"),
            "1-byte file must still pass through; got {:?}",
            paths
        );

        unsafe { std::env::remove_var("ECP_MAX_FILE_BYTES") };
    }
}
