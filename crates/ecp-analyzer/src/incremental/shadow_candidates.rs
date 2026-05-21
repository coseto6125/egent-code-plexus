//! Shadow-candidate path derivation for incremental indexing.
//!
//! Port of `gitnexus/src/core/incremental/shadow-candidates.ts` (ref-gitnexus
//! PR #1479). When a newly-added file can steal import-resolution ownership
//! from a pre-existing sibling, the pre-existing file must be re-analysed so
//! its stale `Calls` edges are rebuilt against the new resolution target.
//!
//! Shadow patterns covered (resolution-priority-aware):
//!
//!   (a) Same basename, different extension —
//!       added `foo/bar.ts` shadows `foo/bar.{tsx,js,jsx,mjs,cjs,d.ts}`.
//!   (b) Bare file beats directory-style index —
//!       added `foo/bar.ts` shadows `foo/bar/index.{ts,tsx,...}`.
//!   (c) Directory-index beats bare file —
//!       added `foo/index.ts` shadows `foo.{ts,tsx,...}` (converting a
//!       single-file module into a directory module).
//!
//! Cross-platform path separators: pattern (b) is generated with both `/`
//! and `\` to match whatever separator the caller's `all_files` slice uses.

use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};

/// JS/TS extensions that participate in standard module-resolution shadowing.
/// Order matches the TS source so the generated candidate sets are identical.
const SHADOW_EXTS: &[&str] = &[".d.ts", ".tsx", ".ts", ".jsx", ".js", ".mjs", ".cjs"];

/// Returns every pre-existing file in `all_files` whose import-resolution
/// ownership any file in `changed` can steal.
///
/// Only paths with a recognised `SHADOW_EXTS` extension are considered as
/// sources of shadows; files with other extensions never trigger a cascade.
/// The result is deduplicated and excludes any path that also appears in
/// `changed` (a file already being re-analysed needs no second entry).
///
/// Returns a `BTreeSet` for deterministic, sorted iteration — callers receive
/// a stable order without an extra sort step.
pub fn detect_shadow_candidates(changed: &[PathBuf], all_files: &[PathBuf]) -> BTreeSet<PathBuf> {
    if changed.is_empty() || all_files.is_empty() {
        return BTreeSet::new();
    }

    // Build a set of all_files as strings for O(1) membership tests.
    let all_set: HashSet<&Path> = all_files.iter().map(PathBuf::as_path).collect();
    let changed_set: HashSet<&Path> = changed.iter().map(PathBuf::as_path).collect();

    // Accumulate unique shadow paths across all changed files.
    let mut result: BTreeSet<PathBuf> = BTreeSet::new();

    for added in changed {
        let added_str = added.to_string_lossy();

        // Find the matching extension (longest first, because ".d.ts" must
        // match before ".ts" for "foo.d.ts").
        let Some(ext) = SHADOW_EXTS.iter().find(|&&e| added_str.ends_with(e)) else {
            continue;
        };

        let no_ext = &added_str[..added_str.len() - ext.len()];

        // (a) Same basename, different extension.
        for alt in SHADOW_EXTS {
            if *alt != *ext {
                let candidate = PathBuf::from(format!("{no_ext}{alt}"));
                if all_set.contains(candidate.as_path())
                    && !changed_set.contains(candidate.as_path())
                {
                    result.insert(candidate);
                }
            }
        }

        // (b) Bare file beats sibling directory-style index.
        // Both separator styles to match whichever the OS wrote into fileHashes.
        for sep in ['/', '\\'] {
            for idx_ext in SHADOW_EXTS {
                let candidate = PathBuf::from(format!("{no_ext}{sep}index{idx_ext}"));
                if all_set.contains(candidate.as_path())
                    && !changed_set.contains(candidate.as_path())
                {
                    result.insert(candidate);
                }
            }
        }

        // (c) New `foo/index.ext` shadows old `foo.ext`.
        let dir = if let Some(s) = no_ext.strip_suffix("/index") {
            Some(s)
        } else {
            no_ext.strip_suffix("\\index")
        };
        if let Some(dir) = dir {
            for alt in SHADOW_EXTS {
                let candidate = PathBuf::from(format!("{dir}{alt}"));
                if all_set.contains(candidate.as_path())
                    && !changed_set.contains(candidate.as_path())
                {
                    result.insert(candidate);
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn test_empty_changed_returns_empty() {
        let all = vec![pb("foo/bar.js")];
        assert!(detect_shadow_candidates(&[], &all).is_empty());
    }

    #[test]
    fn test_empty_all_files_returns_empty() {
        let changed = vec![pb("foo/bar.ts")];
        assert!(detect_shadow_candidates(&changed, &[]).is_empty());
    }

    #[test]
    fn test_non_shadow_ext_returns_empty() {
        let changed = vec![pb("foo/bar.py")];
        let all = vec![pb("foo/bar.js"), pb("foo/bar.ts")];
        assert!(detect_shadow_candidates(&changed, &all).is_empty());
    }
}
