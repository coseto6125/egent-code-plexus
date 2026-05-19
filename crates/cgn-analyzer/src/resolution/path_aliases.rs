//! Path-alias expansion for module specifiers.
//!
//! Mirrors TypeScript's `compilerOptions.paths` semantics (a single `*`
//! wildcard per pattern, multiple replacement candidates per pattern,
//! tried in declaration order). The CLI parses `tsconfig.json` and
//! feeds the result into [`GraphBuilder::with_path_aliases`]; this
//! module knows nothing about JSON or filesystems — it only expands
//! string specifiers into candidate paths that the resolver's existing
//! file-key probing then matches against the `SymbolTable`.
//!
//! Example:
//!
//! ```
//! use cgn_analyzer::resolution::path_aliases::PathAliases;
//! let mut aliases = PathAliases::new();
//! aliases.add("@/*", vec!["src/*".to_string()]);
//! let mut got = vec![];
//! aliases.expand("@/components/Button", |c| {
//!     got.push(c.to_string());
//!     true
//! });
//! assert_eq!(got, vec!["src/components/Button".to_string()]);
//! ```

/// Ordered list of `(pattern, replacements)` entries.
///
/// Pattern syntax mirrors TypeScript: at most one `*` per pattern,
/// which captures everything between the literal prefix and suffix.
/// Patterns without `*` are exact-match only.
#[derive(Debug, Default, Clone)]
pub struct PathAliases {
    entries: Vec<(String, Vec<String>)>,
}

impl PathAliases {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn add(&mut self, pattern: impl Into<String>, replacements: Vec<String>) {
        self.entries.push((pattern.into(), replacements));
    }

    /// Walk every alias-derived candidate path for `specifier`, invoking
    /// `visit` for each. Returning `false` from the closure short-circuits
    /// the remaining replacements + entries.
    pub fn expand(&self, specifier: &str, mut visit: impl FnMut(&str) -> bool) {
        for (pattern, replacements) in &self.entries {
            let Some(matched) = match_pattern(pattern, specifier) else {
                continue;
            };
            for replacement in replacements {
                let expanded = apply_replacement(replacement, matched);
                if !visit(&expanded) {
                    return;
                }
            }
        }
    }
}

/// Match `specifier` against `pattern` (at most one `*`).
///
/// Returns the substring captured by the wildcard, or `Some("")` for
/// an exact (wildcard-free) match. `None` if no match.
fn match_pattern<'a>(pattern: &str, specifier: &'a str) -> Option<&'a str> {
    match pattern.split_once('*') {
        None => {
            if pattern == specifier {
                Some("")
            } else {
                None
            }
        }
        Some((prefix, suffix)) => {
            if specifier.len() < prefix.len() + suffix.len() {
                return None;
            }
            if !specifier.starts_with(prefix) || !specifier.ends_with(suffix) {
                return None;
            }
            Some(&specifier[prefix.len()..specifier.len() - suffix.len()])
        }
    }
}

/// Substitute `matched` for the `*` in `replacement`. If the replacement
/// has no `*`, return it verbatim (TypeScript treats this as a literal
/// fallback for the matched pattern).
fn apply_replacement(replacement: &str, matched: &str) -> String {
    match replacement.split_once('*') {
        None => replacement.to_string(),
        Some((prefix, suffix)) => {
            let mut out = String::with_capacity(prefix.len() + matched.len() + suffix.len());
            out.push_str(prefix);
            out.push_str(matched);
            out.push_str(suffix);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(aliases: &PathAliases, spec: &str) -> Vec<String> {
        let mut out = Vec::new();
        aliases.expand(spec, |c| {
            out.push(c.to_string());
            true
        });
        out
    }

    #[test]
    fn wildcard_prefix_expands() {
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into()]);
        assert_eq!(collect(&a, "@/utils"), vec!["src/utils"]);
        assert_eq!(
            collect(&a, "@/components/Button"),
            vec!["src/components/Button"]
        );
    }

    #[test]
    fn multiple_replacements_yield_multiple_candidates() {
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into(), "lib/*".into()]);
        assert_eq!(collect(&a, "@/x"), vec!["src/x", "lib/x"]);
    }

    #[test]
    fn exact_match_pattern_returns_replacement_verbatim() {
        let mut a = PathAliases::new();
        a.add("config", vec!["app/config/index.ts".into()]);
        assert_eq!(collect(&a, "config"), vec!["app/config/index.ts"]);
        assert_eq!(collect(&a, "config2"), Vec::<String>::new());
    }

    #[test]
    fn non_matching_specifier_yields_nothing() {
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into()]);
        assert_eq!(collect(&a, "./relative"), Vec::<String>::new());
        assert_eq!(collect(&a, "lodash"), Vec::<String>::new());
    }

    #[test]
    fn first_matching_entry_short_circuits_on_false() {
        // visitor returning false stops further expansion in the SAME entry
        // (matches the resolver convention used by for_each_specifier_candidate).
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into(), "lib/*".into()]);
        let mut out = Vec::new();
        a.expand("@/x", |c| {
            out.push(c.to_string());
            false
        });
        assert_eq!(out, vec!["src/x"]);
    }

    #[test]
    fn suffix_in_pattern_is_respected() {
        let mut a = PathAliases::new();
        a.add("*.svg", vec!["assets/*.svg".into()]);
        assert_eq!(collect(&a, "logo.svg"), vec!["assets/logo.svg"]);
        assert_eq!(collect(&a, "logo.png"), Vec::<String>::new());
    }
}
