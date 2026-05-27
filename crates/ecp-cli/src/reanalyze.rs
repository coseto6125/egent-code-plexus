//! Re-parse a specific subset of repository files on demand.
//!
//! `detect_changes` uses this to materialise a fresh AST view of only the
//! files git-diff says changed, so it can compare per-symbol content hashes
//! against the stale `graph.bin` snapshot. Doing a full repo re-analyze just
//! to answer "which symbols actually changed?" would defeat the point of the
//! mmap-backed graph.

use crate::git::{safe_exec, DiffScope};
use ecp_analyzer::{
    astro::parser::AstroProvider, c::parser::CProvider, c_sharp::parser::CSharpProvider,
    cpp::parser::CppProvider, dart::parser::DartProvider, go::parser::GoProvider,
    incremental::shadow_candidates::detect_shadow_candidates, java::parser::JavaProvider,
    javascript::parser::JavaScriptProvider, kotlin::parser::KotlinProvider,
    markdown::parser::MarkdownProvider, openapi::schema_scan::OpenApiProvider,
    php::parser::PhpProvider, python::parser::PythonProvider, ruby::parser::RubyProvider,
    rust::parser::RustProvider, svelte::parser::SvelteProvider, swift::parser::SwiftProvider,
    typescript::parser::TypeScriptProvider, vue::parser::VueProvider, yaml::parser::YamlProvider,
};
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use rustc_hash::FxHashSet;
use std::path::{Path, PathBuf};

/// Process-wide cached pipeline. Every consumer (`reanalyze_files`,
/// `overlay_writer`) goes through this accessor so the 20+ tree-sitter
/// `Query` objects are constructed exactly once per process. Each
/// `make_pipeline()` call costs ~100-300ms in grammar registrations.
pub fn pipeline() -> &'static AnalyzerPipeline {
    static PIPELINE: std::sync::OnceLock<AnalyzerPipeline> = std::sync::OnceLock::new();
    PIPELINE.get_or_init(make_pipeline)
}

/// Build one provider by its `provider_name_for_path` name. Returns `None`
/// for names that `make_pipeline` does not register (e.g. crystal/zig/sql —
/// not yet wired into the reanalyze path), so the caller silently skips them
/// exactly as the full pipeline would (their files resolve to no provider).
fn make_provider(name: &str) -> Option<Box<dyn LanguageProvider>> {
    let p: Box<dyn LanguageProvider> = match name {
        "typescript" => Box::new(TypeScriptProvider::new().unwrap()),
        "python" => Box::new(PythonProvider::new().unwrap()),
        "go" => Box::new(GoProvider::new().unwrap()),
        "rust" => Box::new(RustProvider::new().unwrap()),
        "java" => Box::new(JavaProvider::new().unwrap()),
        "javascript" => Box::new(JavaScriptProvider::new().unwrap()),
        "php" => Box::new(PhpProvider::new().unwrap()),
        "ruby" => Box::new(RubyProvider::new().unwrap()),
        "kotlin" => Box::new(KotlinProvider::new().unwrap()),
        "c_sharp" => Box::new(CSharpProvider::new().unwrap()),
        "c" => Box::new(CProvider::new().unwrap()),
        "cpp" => Box::new(CppProvider::new().unwrap()),
        "swift" => Box::new(SwiftProvider::new().unwrap()),
        "dart" => Box::new(DartProvider::new().unwrap()),
        "openapi" => Box::new(OpenApiProvider::new().unwrap()),
        "vue" => Box::new(VueProvider::new().unwrap()),
        "astro" => Box::new(AstroProvider::new().unwrap()),
        "svelte" => Box::new(SvelteProvider::new().unwrap()),
        _ => return None,
    };
    Some(p)
}

/// All provider names `make_pipeline` registers, in registration order. The
/// full pipeline is `make_pipeline_for_names(ALL_PROVIDER_NAMES)` plus the two
/// providers (markdown, yaml) that have no `provider_name_for_path` extension
/// mapping and so are only reachable from the full `analyze` path.
const ALL_PROVIDER_NAMES: &[&str] = &[
    "typescript",
    "python",
    "go",
    "rust",
    "java",
    "javascript",
    "php",
    "ruby",
    "kotlin",
    "c_sharp",
    "c",
    "cpp",
    "swift",
    "dart",
    "openapi",
    "vue",
    "astro",
    "svelte",
];

/// Build the production analyzer pipeline with every registered language
/// provider. Shared between full `analyze` and partial `reanalyze` paths so
/// they observe identical parse behaviour.
///
/// Most callers should use `pipeline()` instead — this constructor is
/// retained for the `OnceLock::get_or_init` initializer and for tests that
/// need an isolated pipeline.
pub fn make_pipeline() -> AnalyzerPipeline {
    let mut pipeline = make_pipeline_for_names(ALL_PROVIDER_NAMES.iter().copied());
    // markdown + yaml have no extension dispatch in provider_name_for_path
    // (only the GitHub-Actions / docker-compose path specials route to yaml),
    // so make_provider does not list them; register them here to keep the full
    // pipeline byte-identical to the pre-refactor set.
    pipeline.register_provider(Box::new(MarkdownProvider::new().unwrap()));
    pipeline.register_provider(Box::new(YamlProvider::new().unwrap()));
    pipeline
}

/// Build a pipeline holding only the named providers (dedup'd). The incremental
/// reanalyze path uses this to construct a pipeline for exactly the languages a
/// dirty set touches, avoiding the full 20-provider tree-sitter `Query` compile
/// (~0.65s) when reparsing a handful of changed files (~8ms of actual parse).
pub(crate) fn make_pipeline_for_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> AnalyzerPipeline {
    let mut pipeline = AnalyzerPipeline::new();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    for name in names {
        if seen.insert(name) {
            if let Some(provider) = make_provider(name) {
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
    if rel_paths.is_empty() {
        return Vec::new();
    }

    // Expand the path set with any pre-existing files whose import-resolution
    // the changed files can steal (ref-gitnexus PR #1479 stale-Calls fix).
    let expanded: Vec<String> = if let Some(all_tracked) = tracked_files(repo) {
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
    };
    let rel_paths = expanded.as_slice();

    // Build a pipeline holding only the providers this dirty set's extensions
    // map to — reparsing a handful of files no longer pays the full
    // 20-provider tree-sitter compile. The full `pipeline()` singleton stays
    // reserved for the cold-index path that touches every language at once.
    let needed: FxHashSet<&str> = rel_paths
        .iter()
        .filter_map(|p| AnalyzerPipeline::provider_name_for_path(Path::new(p)))
        .collect();
    if needed.is_empty() {
        return Vec::new();
    }
    let pipeline = make_pipeline_for_names(needed.iter().copied());

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

    /// Pin the `ALL_PROVIDER_NAMES` ↔ `make_provider` invariant: every name the
    /// full pipeline claims to register must be constructible. A drift here would
    /// silently drop a language from `make_pipeline()` (the cold-index path), not
    /// just the incremental subset.
    #[test]
    fn all_provider_names_are_constructible() {
        for name in ALL_PROVIDER_NAMES {
            assert!(
                make_provider(name).is_some(),
                "make_provider({name:?}) returned None — ALL_PROVIDER_NAMES drifted from make_provider's arms"
            );
        }
    }

    /// The full pipeline must hold exactly `ALL_PROVIDER_NAMES` + the two
    /// extension-less providers (markdown, yaml) appended in `make_pipeline`.
    #[test]
    fn make_pipeline_holds_all_named_providers_plus_two() {
        let subset = make_pipeline_for_names(ALL_PROVIDER_NAMES.iter().copied());
        let full = make_pipeline();
        assert_eq!(
            full.provider_count(),
            subset.provider_count() + 2,
            "make_pipeline should be the named subset plus markdown + yaml"
        );
    }
}
