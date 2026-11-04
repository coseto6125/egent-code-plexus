//! PHP import scope: a `use` statement (class, `use function`, `use const`)
//! applies to the `namespace` declaration that holds it, either a bracketed
//! `namespace A { }` block or an unbracketed `namespace A;` section that runs
//! to the next `namespace` statement (PHP manual, "Namespaces" > "Using
//! namespaces: Aliasing/Importing"). Each fixture states the edge the PHP
//! name-resolution rule gives, never the edge ecp builds today. An unqualified
//! function call resolves through the `use function` table first, then the
//! current namespace, then the global namespace (manual, "Namespaces fallback").
//!
//! | Row | Scope kinds covered |
//! |---|---|
//! | inner-import-beats-outer-decl | bracketed namespace vs global function |
//! | sibling-isolation | bracketed and unbracketed namespaces |
//! | no-leak-outward | bracketed namespace |
//! | inner-decl-beats-outer-import | skipped (see return notes) |
//! | outer-import-reaches-inner | file-level (global) use, function body |
//! | unresolved-import-blocks-fallback | bracketed namespace |
//! | reopened-scope | two `namespace A { }` blocks |
//! | alias | bracketed namespace (`use function B\g as h`) |
//! | empty scope | bracketed namespace with a use and no caller |
use crate::import_scope_support::{assert_overlay_matches_full, build, calls, hit};
use ecp_analyzer::php::parser::PhpProvider;

fn provider() -> PhpProvider {
    PhpProvider::new().expect("PhpProvider::new")
}

const B_PHP: &str = "<?php
namespace B;
function g() {}
";

const C_PHP: &str = "<?php
namespace C;
function g() {}
";

// ---- inner-import-beats-outer-decl ----
const INNER_BEATS_OUTER_A: &str = "<?php
namespace {
    function g() {}
}
namespace A {
    use function B\\g;
    function caller_inner() { g(); }
}
";
const INNER_BEATS_OUTER: &[(&str, &str)] = &[("a.php", INNER_BEATS_OUTER_A), ("b.php", B_PHP)];

// Manual "Namespaces fallback": `use function B\g` is consulted before the
// global fallback, so the global `g` is not the target.
#[ignore = "needs import scope: the file's outer namesake beats the scoped import (C4 scope table)"]
#[test]
fn test_php_import_scope_inner_import_beats_outer_decl_bracketed_namespace_binds_import() {
    let g = build(&provider(), INNER_BEATS_OUTER, &[]);
    assert_eq!(calls(&g, "caller_inner"), vec![hit("b.php", "g", 3)]);
}

#[test]
fn test_php_import_scope_inner_import_beats_outer_decl_bracketed_namespace_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), INNER_BEATS_OUTER, &[], "a.php");
}

// ---- sibling-isolation (bracketed) ----
const SIBLING_A: &str = "<?php
namespace A {
    use function B\\g;
    function caller_a() { g(); }
}
namespace D {
    use function C\\g;
    function caller_d() { g(); }
}
";
const SIBLING: &[(&str, &str)] = &[("a.php", SIBLING_A), ("b.php", B_PHP), ("c.php", C_PHP)];

// Manual "Importing": imports are per namespace declaration; the second
// namespace block starts with an empty import table.
#[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
#[test]
fn test_php_import_scope_sibling_isolation_bracketed_namespace_binds_each_own() {
    let g = build(&provider(), SIBLING, &[]);
    assert_eq!(calls(&g, "caller_a"), vec![hit("b.php", "g", 3)]);
    assert_eq!(calls(&g, "caller_d"), vec![hit("c.php", "g", 3)]);
}

#[test]
fn test_php_import_scope_sibling_isolation_bracketed_namespace_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), SIBLING, &[], "a.php");
}

// ---- sibling-isolation (unbracketed) ----
const SIBLING_UNBRACKETED_A: &str = "<?php
namespace A;
use function B\\g;
function caller_ua() { g(); }
namespace D;
use function C\\g;
function caller_ud() { g(); }
";
const SIBLING_UNBRACKETED: &[(&str, &str)] = &[
    ("a.php", SIBLING_UNBRACKETED_A),
    ("b.php", B_PHP),
    ("c.php", C_PHP),
];

// Manual "Defining namespaces": an unbracketed section runs to the next
// `namespace` statement, and its imports end with it.
#[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
#[test]
fn test_php_import_scope_sibling_isolation_unbracketed_namespace_binds_each_own() {
    let g = build(&provider(), SIBLING_UNBRACKETED, &[]);
    assert_eq!(calls(&g, "caller_ua"), vec![hit("b.php", "g", 3)]);
    assert_eq!(calls(&g, "caller_ud"), vec![hit("c.php", "g", 3)]);
}

#[test]
fn test_php_import_scope_sibling_isolation_unbracketed_namespace_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), SIBLING_UNBRACKETED, &[], "a.php");
}

// ---- no-leak-outward ----
const NO_LEAK_A: &str = "<?php
namespace A {
    use function B\\g;
    function caller_inside() { g(); }
}
namespace D {
    function caller_outside() { g(); }
}
";
const NO_LEAK: &[(&str, &str)] = &[("a.php", NO_LEAK_A), ("b.php", B_PHP)];

// Manual "Importing": the import table belongs to namespace A only, so `g()`
// in D resolves to D\g, then global g, never B\g.
#[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
#[test]
fn test_php_import_scope_no_leak_outward_bracketed_namespace_outside_has_no_edge() {
    let g = build(&provider(), NO_LEAK, &[]);
    assert_eq!(calls(&g, "caller_inside"), vec![hit("b.php", "g", 3)]);
    assert!(!calls(&g, "caller_outside").contains(&hit("b.php", "g", 3)));
}

#[test]
fn test_php_import_scope_no_leak_outward_bracketed_namespace_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), NO_LEAK, &[], "a.php");
}

// ---- outer-import-reaches-inner ----
const OUTER_REACHES_A: &str = "<?php
use function B\\g;
function caller_nested() {
    $f = function () { return g(); };
    return $f();
}
function caller_plain() { g(); }
";
const OUTER_REACHES: &[(&str, &str)] = &[("a.php", OUTER_REACHES_A), ("b.php", B_PHP)];

// Manual "Importing": a `use` in the global (unnamed) namespace covers every
// function body that follows it in the file.
#[test]
fn test_php_import_scope_outer_import_reaches_inner_file_level_use_binds_import() {
    let g = build(&provider(), OUTER_REACHES, &[]);
    assert_eq!(calls(&g, "caller_plain"), vec![hit("b.php", "g", 3)]);
}

#[test]
fn test_php_import_scope_outer_import_reaches_inner_file_level_use_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), OUTER_REACHES, &[], "a.php");
}

// ---- unresolved-import-blocks-fallback ----
const UNRESOLVED_A: &str = "<?php
namespace {
    function g() {}
}
namespace A {
    use function Missing\\g;
    function caller_unresolved() { g(); }
}
";
const UNRESOLVED: &[(&str, &str)] = &[("a.php", UNRESOLVED_A)];

// Manual "Namespaces fallback": the import binds `g` to Missing\g, so the
// global fallback never runs.
#[ignore = "needs import scope: an unresolved scoped import falls back to the outer namesake (C4 scope table)"]
#[test]
fn test_php_import_scope_unresolved_import_blocks_fallback_bracketed_namespace_no_edge() {
    let g = build(&provider(), UNRESOLVED, &[]);
    assert!(!calls(&g, "caller_unresolved").contains(&hit("a.php", "g", 3)));
}

#[test]
fn test_php_import_scope_unresolved_import_blocks_fallback_bracketed_namespace_overlay_matches_full(
) {
    assert_overlay_matches_full(&provider(), UNRESOLVED, &[], "a.php");
}

// ---- reopened-scope ----
const REOPENED_A: &str = "<?php
namespace A {
    use function B\\g;
    function caller_importing() { g(); }
}
namespace A {
    function caller_plain() { g(); }
}
";
const REOPENED: &[(&str, &str)] = &[("a.php", REOPENED_A), ("b.php", B_PHP)];

// Manual "Importing": imports reset at each `namespace` declaration, even
// when the same namespace name is opened again.
#[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
#[test]
fn test_php_import_scope_reopened_scope_bracketed_namespace_only_importing_block_binds() {
    let g = build(&provider(), REOPENED, &[]);
    assert_eq!(calls(&g, "caller_importing"), vec![hit("b.php", "g", 3)]);
    assert!(!calls(&g, "caller_plain").contains(&hit("b.php", "g", 3)));
}

#[test]
fn test_php_import_scope_reopened_scope_bracketed_namespace_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), REOPENED, &[], "a.php");
}

// ---- alias ----
const ALIAS_A: &str = "<?php
namespace A {
    use function B\\g as h;
    function caller_alias() { h(); }
}
namespace D {
    function caller_alias_outside() { h(); }
}
";
const ALIAS: &[(&str, &str)] = &[("a.php", ALIAS_A), ("b.php", B_PHP)];

// Manual "Aliasing": `use function B\g as h` binds `h` in its namespace only.
#[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
#[test]
fn test_php_import_scope_alias_bracketed_namespace_binds_only_inside() {
    let g = build(&provider(), ALIAS, &[]);
    assert_eq!(calls(&g, "caller_alias"), vec![hit("b.php", "g", 3)]);
    assert!(!calls(&g, "caller_alias_outside").contains(&hit("b.php", "g", 3)));
}

#[test]
fn test_php_import_scope_alias_bracketed_namespace_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), ALIAS, &[], "a.php");
}

// ---- empty scope ----
const EMPTY_A: &str = "<?php
namespace Empty {
    use function B\\g;
}
namespace Full {
    function caller_full() {}
}
";
const EMPTY: &[(&str, &str)] = &[("a.php", EMPTY_A), ("b.php", B_PHP)];

// Manual "Importing": a namespace with an import and no code is legal.
#[test]
fn test_php_import_scope_empty_scope_bracketed_namespace_no_edge_no_panic() {
    let g = build(&provider(), EMPTY, &[]);
    assert!(calls(&g, "caller_full").is_empty());
}

#[test]
fn test_php_import_scope_empty_scope_bracketed_namespace_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), EMPTY, &[], "a.php");
}
