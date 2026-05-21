use ecp_analyzer::incremental::shadow_candidates::detect_shadow_candidates;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn pb(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn bs(paths: &[&str]) -> BTreeSet<PathBuf> {
    paths.iter().map(|s| pb(s)).collect()
}

// ── Pattern (a): same basename, different extension ─────────────────────────

#[test]
fn test_new_ts_shadows_sibling_js_import() {
    // Adding foo.ts next to existing foo.js → foo.js needs reanalysis.
    let changed = vec![pb("src/foo.ts")];
    let all = vec![pb("src/foo.ts"), pb("src/foo.js")];
    let got = detect_shadow_candidates(&changed, &all);
    assert_eq!(got, bs(&["src/foo.js"]));
}

#[test]
fn test_new_ts_shadows_multiple_siblings() {
    // A single new .ts file can shadow several pre-existing siblings.
    let changed = vec![pb("lib/mod.ts")];
    let all = vec![
        pb("lib/mod.ts"),
        pb("lib/mod.js"),
        pb("lib/mod.jsx"),
        pb("lib/mod.mjs"),
    ];
    let got = detect_shadow_candidates(&changed, &all);
    assert_eq!(got, bs(&["lib/mod.js", "lib/mod.jsx", "lib/mod.mjs"]));
}

#[test]
fn test_new_dts_shadows_js() {
    // .d.ts extension is also in SHADOW_EXTS.
    let changed = vec![pb("types/api.d.ts")];
    let all = vec![pb("types/api.d.ts"), pb("types/api.js")];
    let got = detect_shadow_candidates(&changed, &all);
    assert_eq!(got, bs(&["types/api.js"]));
}

// ── Pattern (b): bare file shadows directory-style index ────────────────────

#[test]
fn test_bare_file_shadows_index() {
    // added foo/bar.ts shadows foo/bar/index.js (and other index variants)
    let changed = vec![pb("foo/bar.ts")];
    let all = vec![pb("foo/bar.ts"), pb("foo/bar/index.js")];
    let got = detect_shadow_candidates(&changed, &all);
    assert_eq!(got, bs(&["foo/bar/index.js"]));
}

// ── Pattern (c): directory-index shadows bare file ──────────────────────────

#[test]
fn test_new_index_shadows_sibling_bare_file() {
    // added foo/bar/index.ts shadows foo/bar.js
    let changed = vec![pb("foo/bar/index.ts")];
    let all = vec![pb("foo/bar/index.ts"), pb("foo/bar.js")];
    let got = detect_shadow_candidates(&changed, &all);
    assert_eq!(got, bs(&["foo/bar.js"]));
}

// ── Distinct basenames → no shadow ──────────────────────────────────────────

#[test]
fn test_distinct_basenames_no_shadow() {
    // bar.ts vs foo.js — completely different names, no shadow.
    let changed = vec![pb("src/bar.ts")];
    let all = vec![pb("src/bar.ts"), pb("src/foo.js")];
    let got = detect_shadow_candidates(&changed, &all);
    assert!(got.is_empty(), "expected empty, got {got:?}");
}

#[test]
fn test_different_directories_same_basename_both_returned() {
    // TS source does not filter by directory — it generates string-based
    // candidate paths and leaves membership filtering to the caller (us).
    // Our implementation filters against all_files, so two files with the
    // same basename in different dirs are independent: adding a/foo.ts only
    // shadows a/foo.js, not b/foo.js, because the generated candidate path
    // is "a/foo.js", which is exactly what we check against all_files.
    let changed = vec![pb("a/foo.ts")];
    let all = vec![pb("a/foo.ts"), pb("a/foo.js"), pb("b/foo.js")];
    let got = detect_shadow_candidates(&changed, &all);
    // Only the sibling in the same dir is in the generated candidate set.
    assert_eq!(got, bs(&["a/foo.js"]));
}

// ── Guard: changed file not duplicated in result ─────────────────────────────

#[test]
fn test_changed_file_excluded_from_result() {
    // foo.js is being re-analysed already (in changed); must not appear again.
    let changed = vec![pb("src/foo.ts"), pb("src/foo.js")];
    let all = vec![pb("src/foo.ts"), pb("src/foo.js"), pb("src/foo.jsx")];
    let got = detect_shadow_candidates(&changed, &all);
    assert!(
        !got.contains(&pb("src/foo.ts")),
        "changed file leaked into result"
    );
    assert!(
        !got.contains(&pb("src/foo.js")),
        "changed file leaked into result"
    );
    assert_eq!(got, bs(&["src/foo.jsx"]));
}

// ── Non-shadow extension → empty ─────────────────────────────────────────────

#[test]
fn test_non_shadow_ext_no_cascade() {
    let changed = vec![pb("src/util.py")];
    let all = vec![pb("src/util.py"), pb("src/util.js")];
    let got = detect_shadow_candidates(&changed, &all);
    assert!(got.is_empty());
}
