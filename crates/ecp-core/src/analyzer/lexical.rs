//! JS / TS lexical call scoping: a bare call binds the innermost named
//! function in scope before any name tier. Full indexing (Pass 2) and the
//! session overlay build this same index over one file's declarations.

use crate::analyzer::types::{innermost_enclosing, span_contains, Span};
use crate::graph::NodeKind;
use rustc_hash::FxHashMap;
use std::path::Path;

/// One file's lexical candidates. Parent discovery runs once, not once per
/// call.
#[derive(Default)]
pub struct LexicalFunctions {
    by_name: FxHashMap<String, Vec<(Option<Span>, u32)>>,
}

impl LexicalFunctions {
    /// The index over `symbols`, one file's declarations in source order as
    /// `(kind, name, has_owner, span)`. The i-th declaration's id is
    /// `start + i`. A file that is not JS / TS gets an empty index.
    pub fn new<'a>(
        path: &Path,
        symbols: impl Iterator<Item = (NodeKind, &'a str, bool, Span)>,
        start: u32,
    ) -> Self {
        let mut index = Self::default();
        if !matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts")
        ) {
            return index;
        }
        let functions: Vec<_> = symbols
            .enumerate()
            .filter(|(_, (kind, ..))| {
                matches!(
                    kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                )
            })
            .collect();
        let spans: Vec<Span> = functions.iter().map(|(_, (.., span))| *span).collect();
        let parents = innermost_enclosing(&spans);
        for (k, &(offset, (kind, name, has_owner, _))) in functions.iter().enumerate() {
            if kind != NodeKind::Function {
                continue;
            }
            // The entry exists even when no candidate joins it: a class's
            // own function named like the callee still keeps the call off
            // the name tiers.
            let candidates = index.by_name.entry(name.to_string()).or_default();
            let parent = parents[k].map(|parent| spans[parent]);
            if parent.is_some() || !has_owner {
                candidates.push((parent, start + offset as u32));
            }
        }
        index
    }

    fn scope_size(scope: Option<Span>) -> (u32, i64) {
        scope
            .map(|span| (span.2 - span.0, span.3 as i64 - span.1 as i64))
            .unwrap_or((u32::MAX, i64::MAX))
    }

    /// The function a bare call to `callee` from `caller` binds, at 1.0.
    /// `None`: the file declares no function of that name, so the name
    /// tiers decide. An empty result prevents fallback to an inaccessible
    /// or ambiguous closure: only the import tiers may bind then.
    pub fn targets(&self, caller: Span, callee: &str) -> Option<Vec<(u32, f32)>> {
        if callee.contains(['.', ':']) {
            return None;
        }
        let candidates = self.by_name.get(callee)?;
        let mut best = None;
        let mut ambiguous = false;
        for &(scope, target) in candidates {
            if scope.is_some_and(|span| !span_contains(span, caller)) {
                continue;
            }
            let size = Self::scope_size(scope);
            match best {
                Some((best_size, _)) if size > best_size => {}
                Some((best_size, _)) if size == best_size => ambiguous = true,
                _ => {
                    best = Some((size, target));
                    ambiguous = false;
                }
            }
        }
        Some(match best {
            Some((_, target)) if !ambiguous => vec![(target, 1.0)],
            _ => Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(path: &str, symbols: &[(NodeKind, &str, bool, Span)]) -> LexicalFunctions {
        LexicalFunctions::new(Path::new(path), symbols.iter().copied(), 10)
    }

    #[test]
    fn test_targets_two_top_level_twins_returns_empty() {
        let lex = index(
            "a.ts",
            &[
                (NodeKind::Function, "helper", false, (0, 0, 0, 27)),
                (NodeKind::Function, "helper", false, (1, 0, 1, 27)),
                (NodeKind::Function, "run", false, (2, 0, 2, 40)),
            ],
        );
        assert_eq!(lex.targets((2, 0, 2, 40), "helper"), Some(Vec::new()));
    }

    #[test]
    fn test_targets_nested_function_shadows_top_level_returns_inner() {
        let lex = index(
            "a.js",
            &[
                (NodeKind::Function, "helper", false, (0, 0, 0, 20)),
                (NodeKind::Function, "outer", false, (1, 0, 5, 1)),
                (NodeKind::Function, "helper", false, (2, 2, 2, 20)),
                (NodeKind::Function, "inner", false, (3, 2, 4, 3)),
            ],
        );
        assert_eq!(lex.targets((3, 2, 4, 3), "helper"), Some(vec![(12, 1.0)]));
        assert_eq!(lex.targets((6, 0, 6, 9), "helper"), Some(vec![(10, 1.0)]));
    }

    #[test]
    fn test_targets_owned_top_level_function_only_returns_empty() {
        let lex = index("a.ts", &[(NodeKind::Function, "make", true, (1, 2, 1, 20))]);
        assert_eq!(lex.targets((5, 0, 5, 9), "make"), Some(Vec::new()));
    }

    #[test]
    fn test_targets_qualified_or_unknown_callee_returns_none() {
        let lex = index("a.ts", &[(NodeKind::Function, "f", false, (0, 0, 0, 9))]);
        assert_eq!(lex.targets((1, 0, 1, 9), "o.f"), None);
        assert_eq!(lex.targets((1, 0, 1, 9), "g"), None);
    }

    #[test]
    fn test_new_non_ecmascript_path_returns_empty_index() {
        let lex = index("a.py", &[(NodeKind::Function, "f", false, (0, 0, 0, 9))]);
        assert_eq!(lex.targets((1, 0, 1, 9), "f"), None);
    }
}
