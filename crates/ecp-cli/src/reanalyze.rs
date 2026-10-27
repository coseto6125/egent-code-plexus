//! Re-parse a specific subset of repository files on demand.
//!
//! `detect_changes` uses this to materialise a fresh AST view of only the
//! files git-diff says changed, so it can compare per-symbol content hashes
//! against the stale `graph.bin` snapshot. Doing a full repo re-analyze just
//! to answer "which symbols actually changed?" would defeat the point of the
//! mmap-backed graph.

use crate::git::{safe_exec, DiffScope};
use crate::provider_registry::{make_provider, PROVIDER_CONSTRUCTORS};
use ecp_analyzer::incremental::shadow_candidates::detect_shadow_candidates;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::routing::{FileRouter, RouteOutcome, SizeCap};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Process-wide pipeline holding every registered provider. Test and bench
/// callers use it; the production overlay and reanalyze paths build only the
/// providers their paths route to ([`pipeline_for_path`],
/// [`make_pipeline_for_names`]), because each provider costs a tree-sitter
/// `Query` compile (~100-300ms for the full set).
pub fn pipeline() -> &'static AnalyzerPipeline {
    static PIPELINE: std::sync::OnceLock<AnalyzerPipeline> = std::sync::OnceLock::new();
    PIPELINE.get_or_init(make_pipeline)
}

/// The provider names every parse path constructs for `paths` (the cold index,
/// the incremental reanalyze, the overlay writer, `impact --baseline`): every
/// provider of each path's route. An unrouted, excluded or empty path adds
/// nothing.
pub(crate) fn provider_names_for_paths<'a>(
    paths: impl IntoIterator<Item = &'a str>,
) -> FxHashSet<&'static str> {
    let mut names = FxHashSet::default();
    for path in paths {
        if let RouteOutcome::Routed(route) = FileRouter::PATH_ONLY.route(path) {
            names.extend(route.providers());
        }
    }
    names
}

/// A pipeline holding only the providers `rel_path` routes to and `size_cap`,
/// built once per distinct (provider set, cap bytes) and kept for the process.
/// The overlay writer parses one file per call, so a per-call build would pay
/// the `Query` compile each time. `None` for a path no rule routes.
pub fn pipeline_for_path(rel_path: &Path, size_cap: SizeCap) -> Option<&'static AnalyzerPipeline> {
    type Cache = Mutex<FxHashMap<(Vec<&'static str>, u64), &'static AnalyzerPipeline>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let mut names: Vec<&'static str> = provider_names_for_paths([rel_path.to_str()?])
        .into_iter()
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort_unstable();
    let mut cache = CACHE
        .get_or_init(Cache::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Some(
        *cache
            .entry((names, size_cap.bytes()))
            .or_insert_with_key(|(names, _)| {
                &*Box::leak(Box::new(make_pipeline_for_names(
                    names.iter().copied(),
                    size_cap,
                )))
            }),
    )
}

/// Build the production analyzer pipeline with every registered provider, in
/// `PROVIDER_CONSTRUCTORS` order.
///
/// Most callers should use `pipeline()` instead; this constructor stays for
/// the `OnceLock::get_or_init` initializer and for tests that need an
/// isolated pipeline.
pub fn make_pipeline() -> AnalyzerPipeline {
    make_pipeline_for_names(
        PROVIDER_CONSTRUCTORS.iter().map(|(name, _)| *name),
        ecp_core::config::env_size_cap(None),
    )
}

/// Build a pipeline holding only the named providers (dedup'd) and the per-file
/// cap `size_cap`. A name with no constructor is skipped.
///
/// The incremental reanalyze path and `impact --baseline` use this to construct
/// a pipeline for exactly the providers a path set routes to
/// ([`provider_names_for_paths`]), avoiding the full tree-sitter `Query`
/// compile (~0.65s) when reparsing a handful of changed files (~8ms of actual
/// parse).
pub(crate) fn make_pipeline_for_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
    size_cap: SizeCap,
) -> AnalyzerPipeline {
    let mut pipeline = AnalyzerPipeline::new().with_size_cap(size_cap);
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    for name in names {
        if seen.insert(name) {
            // A malformed tree-sitter query must fail fast: dropping the
            // provider silently would give an incomplete incremental
            // reanalyze / `impact --baseline` result with no failure signal.
            // This function has no `Result` in its signature (its callers mix
            // `Result`-free and `Result`-returning shapes), so `panic!` keeps
            // the pre-registry behaviour.
            if let Some(result) = make_provider(name) {
                let provider = result
                    .unwrap_or_else(|e| panic!("provider constructor for {name:?} failed: {e}"));
                pipeline.register_provider(provider);
            }
        }
    }
    pipeline
}

/// Re-parse the "new" side of `scope` for the given relative paths and
/// return their fresh `LocalGraph` views. The materialised file location
/// depends on which scope is requested:
///
/// | scope         | source of "new" content              |
/// |---------------|--------------------------------------|
/// | `Unstaged`    | working tree directly                |
/// | `All`         | working tree directly                |
/// | `Compare(_)`  | working tree directly                |
/// | `Staged`      | git index (`git show :path`) → tempdir |
///
/// For `Staged` we MUST extract from the index because working tree may
/// have additional unstaged edits on top of staged content; analysing the
/// working tree would report symbols that aren't actually in the diff.
///
/// Files that don't exist on disk (deletions) are silently skipped — the
/// caller reconciles those via the old-graph side of the symbol diff.
///
/// Returns `LocalGraph` entries with `file_path` set to the original
/// relative-to-repo path (NOT the tempdir path), so call sites can match
/// against `graph.files[].path` directly.
pub fn reanalyze_files(repo: &Path, scope: &DiffScope, rel_paths: &[String]) -> Vec<LocalGraph> {
    reanalyze_files_under(
        repo,
        scope,
        rel_paths,
        ecp_core::config::resolve_size_cap(repo),
    )
}

/// `reanalyze_files` under a cap the caller resolved once, so the caller can
/// stamp that same cap on whatever it stores from the parse.
pub fn reanalyze_files_under(
    repo: &Path,
    scope: &DiffScope,
    rel_paths: &[String],
    size_cap: SizeCap,
) -> Vec<LocalGraph> {
    if rel_paths.is_empty() {
        return Vec::new();
    }
    reanalyze_expanded_under(repo, scope, &expand_shadow_paths(repo, rel_paths), size_cap)
}

/// `rel_paths` plus the tracked files whose import resolution the changed
/// files can steal (ref-gitnexus PR #1479 stale-Calls fix). A caller that
/// must stat every parsed file before the parse calls this first, then
/// `reanalyze_expanded_under`, so the parse cascade and the stat set agree.
pub fn expand_shadow_paths(repo: &Path, rel_paths: &[String]) -> Vec<String> {
    if let Some(all_tracked) = tracked_files(repo) {
        let changed_pb: Vec<PathBuf> = rel_paths.iter().map(PathBuf::from).collect();
        let shadows = detect_shadow_candidates(&changed_pb, &all_tracked);
        if shadows.is_empty() {
            rel_paths.to_vec()
        } else {
            // Use FxHashSet for O(1) dedup instead of Vec::contains O(N).
            let mut seen: FxHashSet<String> = rel_paths.iter().map(|s| s.to_string()).collect();
            let mut v = rel_paths.to_vec();
            for s in shadows {
                let as_str = s.to_string_lossy().into_owned();
                if seen.insert(as_str.clone()) {
                    v.push(as_str);
                }
            }
            v
        }
    } else {
        rel_paths.to_vec()
    }
}

/// Parse exactly `rel_paths` (already shadow-expanded, no further cascade).
pub fn reanalyze_expanded_under(
    repo: &Path,
    scope: &DiffScope,
    rel_paths: &[String],
    size_cap: SizeCap,
) -> Vec<LocalGraph> {
    if rel_paths.is_empty() {
        return Vec::new();
    }

    // Build a pipeline holding only the providers this dirty set's routes
    // list, the set the cold index constructs for the same paths. The full
    // `pipeline()` singleton stays reserved for callers that touch every
    // language at once.
    let needed = provider_names_for_paths(rel_paths.iter().map(String::as_str));
    if needed.is_empty() {
        return Vec::new();
    }
    // The repo's `[index] max_file_bytes`, as the cold index reads it: a dirty
    // file the base graph holds must not turn into a `file-too-large` omission.
    let pipeline = make_pipeline_for_names(needed.iter().copied(), size_cap);

    match scope {
        DiffScope::Staged => reanalyze_staged(repo, &pipeline, rel_paths),
        DiffScope::Unstaged | DiffScope::All | DiffScope::Compare(_) => {
            let pairs: Vec<(PathBuf, PathBuf)> = rel_paths
                .iter()
                .filter_map(|rp| {
                    let rel = PathBuf::from(rp);
                    let abs = repo.join(&rel);
                    if abs.exists() && abs.is_file() {
                        Some((abs, rel))
                    } else {
                        None
                    }
                })
                .collect();
            if pairs.is_empty() {
                return Vec::new();
            }
            pipeline.analyze(pairs)
        }
    }
}

/// Extract each staged file's content via `git show :<path>`, write it into
/// a tempdir mirroring the repo layout, and pipeline-analyze the tempdir.
/// We then rewrite each returned `LocalGraph.file_path` back to the original
/// relative path so callers see consistent paths regardless of scope.
fn reanalyze_staged(
    repo: &Path,
    pipeline: &AnalyzerPipeline,
    rel_paths: &[String],
) -> Vec<LocalGraph> {
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();

    for rp in rel_paths {
        let blob = match staged_blob(repo, rp) {
            Some(b) => b,
            None => continue, // staged deletion or unreadable — caller handles via old-graph diff
        };
        let rel = PathBuf::from(rp);
        let dst = tmp.path().join(&rel);
        if let Some(parent) = dst.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        if std::fs::write(&dst, &blob).is_err() {
            continue;
        }
        pairs.push((dst, rel));
    }

    if pairs.is_empty() {
        return Vec::new();
    }
    pipeline.analyze(pairs)
    // tempdir is dropped here, cleaning up the extracted files.
}

fn staged_blob(repo: &Path, rel_path: &str) -> Option<Vec<u8>> {
    let out = safe_exec::git()
        .args(["show", &format!(":{rel_path}")])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout)
}

/// List every file tracked by git in `repo` as relative `PathBuf`s.
/// Used to supply the `all_files` filter to `detect_shadow_candidates`.
/// Returns `None` if git is unavailable or the repo is bare.
fn tracked_files(repo: &Path) -> Option<Vec<PathBuf>> {
    let out = safe_exec::git()
        .args(["ls-files", "-z"])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let paths = out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf8_lossy(s).as_ref()))
        .collect();
    Some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full pipeline holds every registered provider, so a provider added
    /// to `PROVIDER_CONSTRUCTORS` reaches the overlay with no second list.
    #[test]
    fn test_make_pipeline_holds_every_registered_provider() {
        assert_eq!(
            make_pipeline().provider_count(),
            PROVIDER_CONSTRUCTORS.len()
        );
    }

    /// `bash` was once dropped here by a hand list; a scoped pipeline now
    /// registers whatever name it is given.
    #[test]
    fn test_make_pipeline_for_names_bash_registers_provider() {
        assert_eq!(
            make_pipeline_for_names(["bash", "bash"], SizeCap::BUILT_IN).provider_count(),
            1
        );
    }

    #[test]
    fn test_make_pipeline_for_names_unknown_name_skipped() {
        assert_eq!(
            make_pipeline_for_names(["no-such-provider"], SizeCap::BUILT_IN).provider_count(),
            0
        );
    }

    #[test]
    fn test_provider_names_for_paths_edge_inputs_select_nothing() {
        assert!(provider_names_for_paths([]).is_empty());
        assert!(provider_names_for_paths(["", "   ", "README.md", "a.unknownext"]).is_empty());
    }

    /// The generic-YAML candidate `yaml` has no graph yet (C5 open point 6),
    /// so no path selects its provider.
    #[test]
    fn test_provider_names_for_paths_sniffed_yaml_omits_yaml_provider() {
        let names = provider_names_for_paths(["k8s/deploy.yaml", "run.sh"]);
        assert!(
            names.contains("kubernetes") && names.contains("openapi") && names.contains("bash"),
            "{names:?}"
        );
        assert!(!names.contains("yaml"), "{names:?}");
    }

    #[test]
    fn test_pipeline_for_path_same_provider_set_returns_same_pipeline() {
        let a = pipeline_for_path(Path::new("a.sh"), SizeCap::BUILT_IN).expect("bash routes");
        let b = pipeline_for_path(Path::new("dir/b.sh"), SizeCap::BUILT_IN).expect("bash routes");
        assert!(std::ptr::eq(a, b));
        assert_eq!(a.provider_count(), 1);
        assert!(pipeline_for_path(Path::new("notes.txt"), SizeCap::BUILT_IN).is_none());
    }

    /// The cache key holds the cap: a second cap never reuses a pipeline
    /// built under the first, and one cap from two sources shares one.
    #[test]
    fn test_pipeline_for_path_distinct_cap_returns_distinct_pipeline() {
        let built_in = pipeline_for_path(Path::new("a.sh"), SizeCap::BUILT_IN).expect("routes");
        let lowered =
            pipeline_for_path(Path::new("a.sh"), SizeCap::resolve(None, Some(64))).expect("routes");
        let same_bytes = pipeline_for_path(Path::new("b.sh"), SizeCap::resolve(Some("64"), None))
            .expect("routes");
        assert!(!std::ptr::eq(built_in, lowered));
        assert!(std::ptr::eq(lowered, same_bytes));

        let source = "# c\n".repeat(25).into_bytes();
        let kinds = |pipeline: &AnalyzerPipeline| -> Vec<String> {
            pipeline
                .parse_file_raw(Path::new("a.sh"), &source)
                .expect("an omission is not an error")
                .blind_spots
                .into_iter()
                .map(|b| b.kind)
                .collect()
        };
        assert!(kinds(built_in).iter().all(|k| k != "file-too-large"));
        assert_eq!(kinds(lowered), vec!["file-too-large".to_string()]);
    }
}
