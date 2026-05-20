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
    java::parser::JavaProvider, javascript::parser::JavaScriptProvider,
    kotlin::parser::KotlinProvider, markdown::parser::MarkdownProvider, php::parser::PhpProvider,
    python::parser::PythonProvider, ruby::parser::RubyProvider, rust::parser::RustProvider,
    swift::parser::SwiftProvider, typescript::parser::TypeScriptProvider, vue::parser::VueProvider,
    yaml::parser::YamlProvider,
};
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::types::LocalGraph;
use std::path::{Path, PathBuf};

/// Build the production analyzer pipeline with every registered language
/// provider. Shared between full `analyze` and partial `reanalyze` paths so
/// they observe identical parse behaviour.
pub fn make_pipeline() -> AnalyzerPipeline {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(TypeScriptProvider::new().unwrap()));
    pipeline.register_provider(Box::new(PythonProvider::new().unwrap()));
    pipeline.register_provider(Box::new(GoProvider::new().unwrap()));
    pipeline.register_provider(Box::new(RustProvider::new().unwrap()));
    pipeline.register_provider(Box::new(JavaProvider::new().unwrap()));
    pipeline.register_provider(Box::new(JavaScriptProvider::new().unwrap()));
    pipeline.register_provider(Box::new(PhpProvider::new().unwrap()));
    pipeline.register_provider(Box::new(RubyProvider::new().unwrap()));
    pipeline.register_provider(Box::new(KotlinProvider::new().unwrap()));
    pipeline.register_provider(Box::new(CSharpProvider::new().unwrap()));
    pipeline.register_provider(Box::new(CProvider::new().unwrap()));
    pipeline.register_provider(Box::new(CppProvider::new().unwrap()));
    pipeline.register_provider(Box::new(SwiftProvider::new().unwrap()));
    pipeline.register_provider(Box::new(DartProvider::new().unwrap()));
    pipeline.register_provider(Box::new(MarkdownProvider::new().unwrap()));
    pipeline.register_provider(Box::new(YamlProvider::new().unwrap()));
    pipeline.register_provider(Box::new(VueProvider::new().unwrap()));
    pipeline.register_provider(Box::new(AstroProvider::new().unwrap()));
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

    let pipeline = make_pipeline();

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
