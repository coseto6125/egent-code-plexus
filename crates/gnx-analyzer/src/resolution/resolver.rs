use gnx_core::analyzer::types::RawImport;
use std::cell::RefCell;
use std::path::Path;

use crate::resolution::heuristics::ResolutionTier;
use crate::resolution::index::SymbolTable;

pub type NodeId = u32;

/// One resolver attempt, captured when the dump buffer is enabled. The
/// builder later materializes `target_file` via [`SymbolTable::file_of`]
/// and serializes the records to JSONL — see
/// `docs/superpowers/specs/2026-05-15-resolver-oracle-harness.md`.
#[derive(Debug, Clone)]
pub struct ResolverDecision {
    pub src_file: String,
    pub name: String,
    pub specifier: Option<String>,
    pub tier: &'static str,
    pub target_id: Option<NodeId>,
    pub alt_count: u32,
    pub confidence: Option<f32>,
}

/// The core resolver engine that matches symbol names to concrete global nodes.
pub struct Resolver<'a> {
    symbol_table: &'a SymbolTable,
    /// Optional decision buffer. `None` = dumping disabled (production path,
    /// zero overhead). Builder calls [`Resolver::enable_dump`] before pass 2
    /// when the user requested `--dump-resolver`.
    decisions: RefCell<Option<Vec<ResolverDecision>>>,
}

impl<'a> Resolver<'a> {
    /// Creates a new `Resolver` with a reference to the global `SymbolTable`.
    pub fn new(symbol_table: &'a SymbolTable) -> Self {
        Self {
            symbol_table,
            decisions: RefCell::new(None),
        }
    }

    /// Turn on the decision recorder. Each subsequent `resolve_symbol` call
    /// pushes a [`ResolverDecision`] into the internal buffer.
    pub fn enable_dump(&self) {
        *self.decisions.borrow_mut() = Some(Vec::new());
    }

    /// Drain the recorded decisions. Returns `None` if dumping was never
    /// enabled.
    pub fn take_decisions(&self) -> Option<Vec<ResolverDecision>> {
        self.decisions.borrow_mut().take()
    }

    /// Resolves a symbol name to possible target nodes with confidence scores.
    pub fn resolve_symbol(
        &self,
        source_file: &Path,
        symbol_name: &str,
        raw_imports: &[RawImport],
    ) -> Vec<(NodeId, f32)> {
        let mut results = Vec::new();
        let source_file_str = source_file.to_string_lossy();

        // Tier 1: Try SameFile
        if let Some(node_id) = self
            .symbol_table
            .lookup_in_file(&source_file_str, symbol_name)
        {
            results.push((node_id, ResolutionTier::SameFile.base_confidence()));
            self.record(
                &source_file_str,
                symbol_name,
                None,
                "SameFile",
                Some(node_id),
                0,
                Some(ResolutionTier::SameFile.base_confidence()),
            );
            return results; // Highest precedence, return early
        }

        // Tier 2: Try ImportScoped (with L0 path normalization).
        //
        // The literal `import.source` is rarely a SymbolTable key on its own
        // — TS writes `./foo`, Python writes `.helpers`, etc., while
        // `SymbolTable.file_scoped` keys are repo-relative file paths like
        // `src/bar/foo.ts`. We expand each specifier into a small set of
        // candidate keys (relative-resolution + extension/index/__init__
        // guesses) and probe them in order.
        for import in raw_imports {
            let is_match = match &import.alias {
                Some(alias) => alias == symbol_name,
                None => import.imported_name == symbol_name,
            };

            if is_match {
                let exported_name = &import.imported_name;
                for candidate in resolve_specifier_candidates(source_file, &import.source) {
                    if let Some(node_id) = self
                        .symbol_table
                        .lookup_in_file(&candidate, exported_name)
                    {
                        results.push((node_id, ResolutionTier::ImportScoped.base_confidence()));
                        self.record(
                            &source_file_str,
                            symbol_name,
                            Some(import.source.as_str()),
                            "ImportScoped",
                            Some(node_id),
                            0,
                            Some(ResolutionTier::ImportScoped.base_confidence()),
                        );
                        return results;
                    }
                }
            }
        }

        // Tier 3: Try Global (Fallback)
        let global_matches = self.symbol_table.lookup_global(symbol_name);
        if !global_matches.is_empty() {
            // For now, if there are multiple global matches, we just push the first one or all
            // To match original behavior we push all with Global confidence
            let first = global_matches[0];
            let alt = (global_matches.len() - 1) as u32;
            for node_id in global_matches {
                results.push((node_id, ResolutionTier::Global.base_confidence()));
            }
            // For the dump we record the first match + alt_count so the diff
            // harness can compute the FP_overmatch class without serializing
            // every alternative.
            let specifier = raw_imports
                .iter()
                .find(|i| match &i.alias {
                    Some(a) => a == symbol_name,
                    None => i.imported_name == symbol_name,
                })
                .map(|i| i.source.as_str());
            self.record(
                &source_file_str,
                symbol_name,
                specifier,
                "Global",
                Some(first),
                alt,
                Some(ResolutionTier::Global.base_confidence()),
            );
        } else {
            // Nothing matched — still record so the diff can spot FN_dangling.
            let specifier = raw_imports
                .iter()
                .find(|i| match &i.alias {
                    Some(a) => a == symbol_name,
                    None => i.imported_name == symbol_name,
                })
                .map(|i| i.source.as_str());
            self.record(
                &source_file_str,
                symbol_name,
                specifier,
                "Unresolved",
                None,
                0,
                None,
            );
        }

        results
    }

}

/// L0 path normalization: expand `import.source` into the set of
/// `SymbolTable` file keys it could plausibly map to.
///
/// We always include the verbatim specifier first so behavior is a strict
/// superset of pre-L0 (callers that worked before keep working). After that:
///
/// * **Relative** (`./x`, `../x`, `.x`, `..x.y`): join with the source
///   file's parent directory, accounting for Python-style multi-dot prefixes
///   and dotted submodule paths (`from .a.b import C`).
/// * **Both relative and absolute**: try common extensions (`.ts .tsx .py
///   .rs ...`) and package-style suffixes (`/index.ts`, `/__init__.py`,
///   `/mod.rs`).
///
/// All candidates use POSIX separators so they line up with the keys stored
/// by `register_node`.
fn resolve_specifier_candidates(source_file: &std::path::Path, specifier: &str) -> Vec<String> {
    const EXT_CANDIDATES: &[&str] = &[
        ".ts", ".tsx", ".jsx", ".js", ".mjs", ".cjs", ".py", ".pyi", ".rs", ".go", ".java",
        ".kt", ".rb", ".php", ".cs", ".swift", ".dart", ".sol", ".sql",
    ];
    const INDEX_SUFFIXES: &[&str] = &[
        "/index.ts",
        "/index.tsx",
        "/index.js",
        "/index.jsx",
        "/__init__.py",
        "/mod.rs",
        "/lib.rs",
        "/main.rs",
    ];

    let mut out: Vec<String> = Vec::with_capacity(EXT_CANDIDATES.len() + INDEX_SUFFIXES.len() + 4);
    out.push(specifier.to_string());

    let dir = source_file.parent().unwrap_or(std::path::Path::new(""));
    let base_path: Option<std::path::PathBuf> = if let Some(rest) = specifier.strip_prefix("./") {
        Some(dir.join(rest))
    } else if specifier.starts_with("../") {
        let mut p = dir.to_path_buf();
        let mut s = specifier;
        while let Some(rest) = s.strip_prefix("../") {
            p = p.parent().unwrap_or(std::path::Path::new("")).to_path_buf();
            s = rest;
        }
        Some(p.join(s))
    } else if specifier.starts_with('.') {
        // Python-style relative: count leading dots, then a dotted submodule
        // path. `.foo` from `src/pkg/x.py` → `src/pkg/foo`. `..foo.bar` →
        // walk parent once, then `foo/bar`.
        let dots = specifier.bytes().take_while(|&b| b == b'.').count();
        let rest = &specifier[dots..];
        let dotted = rest.replace('.', "/");
        let mut p = dir.to_path_buf();
        for _ in 1..dots {
            p = p.parent().unwrap_or(std::path::Path::new("")).to_path_buf();
        }
        Some(if dotted.is_empty() { p } else { p.join(&dotted) })
    } else {
        // Absolute / bare specifier — alias resolution belongs to L1; here
        // we still emit extension guesses (to cover the rare `import x from
        // "a/b.ts"` written without `./`).
        None
    };

    let push_with_suffixes = |base: &str, out: &mut Vec<String>| {
        out.push(base.to_string());
        for ext in EXT_CANDIDATES {
            out.push(format!("{base}{ext}"));
        }
        for suf in INDEX_SUFFIXES {
            out.push(format!("{base}{suf}"));
        }
    };

    if let Some(b) = base_path {
        let b_str = b.to_string_lossy().replace('\\', "/");
        let b_str = b_str
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        push_with_suffixes(&b_str, &mut out);
    } else if !specifier.contains("://") && !specifier.is_empty() {
        // Absolute-but-pathlike: `a/b` style. Still worth probing.
        let s = specifier.trim_end_matches('/');
        push_with_suffixes(s, &mut out);
    }

    out
}

impl<'a> Resolver<'a> {
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        src_file: &str,
        name: &str,
        specifier: Option<&str>,
        tier: &'static str,
        target_id: Option<NodeId>,
        alt_count: u32,
        confidence: Option<f32>,
    ) {
        // Fast bail-out on the production path: one HashMap-free check.
        let mut slot = self.decisions.borrow_mut();
        let Some(buf) = slot.as_mut() else {
            return;
        };
        buf.push(ResolverDecision {
            src_file: src_file.to_string(),
            name: name.to_string(),
            specifier: specifier.map(|s| s.to_string()),
            tier,
            target_id,
            alt_count,
            confidence,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cands(src: &str, spec: &str) -> Vec<String> {
        resolve_specifier_candidates(&PathBuf::from(src), spec)
    }

    #[test]
    fn verbatim_specifier_is_always_first_candidate() {
        let c = cands("src/a/b.ts", "./foo");
        assert_eq!(c[0], "./foo", "verbatim must lead the candidate list");
    }

    #[test]
    fn ts_dot_relative_resolves_against_source_dir_with_ext_and_index() {
        let c = cands("src/a/b.ts", "./foo");
        assert!(
            c.contains(&"src/a/foo.ts".to_string()),
            "should include src/a/foo.ts: {c:?}"
        );
        assert!(
            c.contains(&"src/a/foo/index.ts".to_string()),
            "should include src/a/foo/index.ts: {c:?}"
        );
    }

    #[test]
    fn ts_parent_relative_walks_up_one_dir() {
        let c = cands("src/a/b.ts", "../helpers/util");
        assert!(
            c.contains(&"src/helpers/util.ts".to_string()),
            "should include src/helpers/util.ts: {c:?}"
        );
    }

    #[test]
    fn python_single_dot_resolves_to_current_package() {
        let c = cands("src/flask/__init__.py", ".globals");
        assert!(
            c.contains(&"src/flask/globals.py".to_string()),
            "should include src/flask/globals.py: {c:?}"
        );
    }

    #[test]
    fn python_dotted_submodule_replaces_dots_with_slashes() {
        let c = cands("src/pkg/x.py", ".sub.mod");
        assert!(
            c.contains(&"src/pkg/sub/mod.py".to_string()),
            "should include src/pkg/sub/mod.py: {c:?}"
        );
    }

    #[test]
    fn python_double_dot_walks_one_parent_then_drills() {
        let c = cands("src/pkg/inner/x.py", "..helpers.util");
        assert!(
            c.contains(&"src/pkg/helpers/util.py".to_string()),
            "should include src/pkg/helpers/util.py: {c:?}"
        );
    }

    #[test]
    fn bare_pathlike_specifier_still_emits_extension_probes() {
        let c = cands("any.ts", "components/Button");
        assert!(
            c.contains(&"components/Button.tsx".to_string())
                || c.contains(&"components/Button.ts".to_string()),
            "bare specifier should still trigger ext probes: {c:?}"
        );
    }

    #[test]
    fn package_style_index_suffix_is_offered() {
        let c = cands("src/a/b.ts", "./foo");
        assert!(
            c.iter().any(|s| s.ends_with("/index.tsx")),
            "should include some /index.tsx candidate: {c:?}"
        );
    }
}
