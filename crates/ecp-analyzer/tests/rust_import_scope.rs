//! Rust import scope: a `use` declaration binds in the scope that holds it,
//! never in the whole file. Each fixture pins the edge the language's own
//! scope rule gives (Rust Reference, "Names > Scopes" and "Use
//! declarations"), not the edge ecp builds today.
//!
//! | Row | Scope kinds covered |
//! |---|---|
//! | inner-import-beats-outer-decl | inline mod, fn body, block |
//! | sibling-isolation | inline mod, fn body, block |
//! | no-leak-outward | inline mod, fn body, block |
//! | inner-decl-beats-outer-import | inline mod, fn body, block |
//! | outer-import-reaches-inner | inline mod (hidden: no edge), fn body, block |
//! | unresolved-import-blocks-fallback | inline mod, fn body, block |
//! | alias-import (extra) | inline mod, fn body, block |
//! | empty-scope (extra) | inline mod |
//!
//! Rows skipped: reopened-scope. A Rust `mod` name is declared once per
//! parent (E0428), so a scope cannot be opened twice.
//!
//! Overlap with `rust_macro_and_inline_mod_calls.rs`: its inline-mod `use`
//! cases (`use super::x`, `use self::x`, `use super::*`) pin path forms, not
//! the scope rows here; no case is duplicated.
//!
//! Every fixture is a real crate on disk (`Cargo.toml` beside `src/`).
//! `b.rs` and `c.rs` each declare `pub fn g` on line 1.

use crate::import_scope_support::{assert_overlay_matches_full, build, calls, hit};
use ecp_analyzer::rust::parser::RustProvider;

const MANIFEST: &str = "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const B: &str = "pub fn g() {}\n";
const C: &str = "pub fn g() {}\n";

fn provider() -> RustProvider {
    RustProvider::new().expect("RustProvider::new")
}

/// One target test (`|g| body` reads graph `g`) and one overlay test per fixture.
macro_rules! case {
    ($(#[$attr:meta])* $fixture:ident, $target:ident, $overlay:ident, |$g:ident| $body:block) => {
        $(#[$attr])*
        #[test]
        fn $target() {
            let $g = build(
                &provider(),
                &[("src/lib.rs", $fixture), ("src/b.rs", B), ("src/c.rs", C)],
                &[("Cargo.toml", MANIFEST)],
            );
            $body
        }

        #[test]
        fn $overlay() {
            assert_overlay_matches_full(
                &provider(),
                &[("src/lib.rs", $fixture), ("src/b.rs", B), ("src/c.rs", C)],
                &[("Cargo.toml", MANIFEST)],
                "src/lib.rs",
            );
        }
    };
}

// ---- inner-import-beats-outer-decl ----
// A `use` in a scope shadows an item of the same name in an outer scope
// (Reference, Names > Scopes > "Item scopes" / "Shadowing").

const R1_MOD: &str = r#"mod b;
fn g() {}
mod inner {
    use crate::b::g;
    pub fn caller_m() { g(); }
}
"#;
const R1_FN: &str = r#"mod b;
fn g() {}
fn caller_f() {
    use crate::b::g;
    g();
}
"#;
const R1_BLK: &str = r#"mod b;
fn g() {}
fn caller_k() {
    {
        use crate::b::g;
        g();
    }
}
"#;

case!(
    #[ignore = "needs import scope: the file's outer namesake beats the scoped import (C4 scope table)"]
    R1_MOD,
    test_rust_import_scope_inner_import_beats_outer_decl_inline_mod_binds_import,
    test_rust_import_scope_inner_import_beats_outer_decl_inline_mod_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_m"), vec![hit("src/b.rs", "g", 1)]);
    }
);
case!(
    #[ignore = "needs import scope: the file's outer namesake beats the scoped import (C4 scope table)"]
    R1_FN,
    test_rust_import_scope_inner_import_beats_outer_decl_fn_body_binds_import,
    test_rust_import_scope_inner_import_beats_outer_decl_fn_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_f"), vec![hit("src/b.rs", "g", 1)]);
    }
);
case!(
    #[ignore = "needs import scope: the file's outer namesake beats the scoped import (C4 scope table)"]
    R1_BLK,
    test_rust_import_scope_inner_import_beats_outer_decl_block_binds_import,
    test_rust_import_scope_inner_import_beats_outer_decl_block_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_k"), vec![hit("src/b.rs", "g", 1)]);
    }
);

// ---- sibling-isolation ----
// A `use` is visible in its own scope only (Reference, Use declarations:
// "the use declaration binds in the scope it appears in").

const R2_MOD: &str = r#"mod b;
mod c;
mod x {
    use crate::b::g;
    pub fn caller_x() { g(); }
}
mod y {
    use crate::c::g;
    pub fn caller_y() { g(); }
}
"#;
const R2_FN: &str = r#"mod b;
mod c;
fn caller_x() {
    use crate::b::g;
    g();
}
fn caller_y() {
    use crate::c::g;
    g();
}
"#;
const R2_BLK: &str = r#"mod b;
mod c;
fn holder() {
    {
        use crate::b::g;
        fn caller_x() { g(); }
    }
    {
        use crate::c::g;
        fn caller_y() { g(); }
    }
}
"#;

case!(
    #[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
    R2_MOD,
    test_rust_import_scope_sibling_isolation_inline_mod_binds_each_own,
    test_rust_import_scope_sibling_isolation_inline_mod_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_x"), vec![hit("src/b.rs", "g", 1)]);
        assert_eq!(calls(&g, "caller_y"), vec![hit("src/c.rs", "g", 1)]);
    }
);
case!(
    #[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
    R2_FN,
    test_rust_import_scope_sibling_isolation_fn_body_binds_each_own,
    test_rust_import_scope_sibling_isolation_fn_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_x"), vec![hit("src/b.rs", "g", 1)]);
        assert_eq!(calls(&g, "caller_y"), vec![hit("src/c.rs", "g", 1)]);
    }
);
case!(
    #[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
    R2_BLK,
    test_rust_import_scope_sibling_isolation_block_binds_each_own,
    test_rust_import_scope_sibling_isolation_block_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_x"), vec![hit("src/b.rs", "g", 1)]);
        assert_eq!(calls(&g, "caller_y"), vec![hit("src/c.rs", "g", 1)]);
    }
);

// ---- no-leak-outward ----
// A `use` in an inner scope does not bind the name in the enclosing scope
// (Reference, Names > Scopes). The outside `g()` has no declaration; the
// fixture is parse-only, rustc would reject it.

const R3_MOD: &str = r#"mod b;
mod inner {
    use crate::b::g;
    pub fn caller_in() { g(); }
}
fn caller_out() { g(); }
"#;
const R3_FN: &str = r#"mod b;
fn caller_in() {
    use crate::b::g;
    g();
}
fn caller_out() { g(); }
"#;
const R3_BLK: &str = r#"mod b;
fn caller_out() {
    {
        use crate::b::g;
    }
    g();
}
"#;

case!(
    #[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
    R3_MOD,
    test_rust_import_scope_no_leak_outward_inline_mod_outside_caller_unbound,
    test_rust_import_scope_no_leak_outward_inline_mod_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_out").contains(&hit("src/b.rs", "g", 1)));
    }
);
case!(
    #[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
    R3_FN,
    test_rust_import_scope_no_leak_outward_fn_body_outside_caller_unbound,
    test_rust_import_scope_no_leak_outward_fn_body_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_out").contains(&hit("src/b.rs", "g", 1)));
    }
);
case!(
    #[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
    R3_BLK,
    test_rust_import_scope_no_leak_outward_block_outside_caller_unbound,
    test_rust_import_scope_no_leak_outward_block_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_out").contains(&hit("src/b.rs", "g", 1)));
    }
);

// ---- inner-decl-beats-outer-import ----
// An item declared in an inner scope shadows an outer `use` (Reference,
// Names > Scopes > "Item scopes"; an inline mod sees no outer name at all).

const R4_MOD: &str = r#"mod b;
use crate::b::g;
mod inner {
    fn g() {}
    pub fn caller_m() { g(); }
}
"#;
const R4_FN: &str = r#"mod b;
use crate::b::g;
fn caller_f() {
    fn g() {}
    g();
}
"#;
const R4_BLK: &str = r#"mod b;
use crate::b::g;
fn caller_k() {
    {
        fn g() {}
        g();
    }
}
"#;

case!(
    R4_MOD,
    test_rust_import_scope_inner_decl_beats_outer_import_inline_mod_binds_inner_decl,
    test_rust_import_scope_inner_decl_beats_outer_import_inline_mod_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_m"), vec![hit("src/lib.rs", "g", 4)]);
    }
);
case!(
    R4_FN,
    test_rust_import_scope_inner_decl_beats_outer_import_fn_body_binds_inner_decl,
    test_rust_import_scope_inner_decl_beats_outer_import_fn_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_f"), vec![hit("src/lib.rs", "g", 4)]);
    }
);
case!(
    R4_BLK,
    test_rust_import_scope_inner_decl_beats_outer_import_block_binds_inner_decl,
    test_rust_import_scope_inner_decl_beats_outer_import_block_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_k"), vec![hit("src/lib.rs", "g", 5)]);
    }
);

// ---- outer-import-reaches-inner ----
// Items of a module are in scope in the fn bodies and blocks nested in it;
// an inline `mod` starts a fresh module and sees no outer name without
// `super::` (Reference, Names > Scopes; Paths > "super").

const R5_MOD: &str = r#"mod b;
use crate::b::g;
mod inner {
    pub fn caller_m() { g(); }
}
"#;
const R5_FN: &str = r#"mod b;
use crate::b::g;
fn caller_f() {
    g();
}
"#;
const R5_BLK: &str = r#"mod b;
use crate::b::g;
fn caller_k() {
    {
        g();
    }
}
"#;

case!(
    #[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
    R5_MOD,
    test_rust_import_scope_outer_import_reaches_inner_inline_mod_is_hidden,
    test_rust_import_scope_outer_import_reaches_inner_inline_mod_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_m").contains(&hit("src/b.rs", "g", 1)));
    }
);
case!(
    R5_FN,
    test_rust_import_scope_outer_import_reaches_inner_fn_body_binds_import,
    test_rust_import_scope_outer_import_reaches_inner_fn_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_f"), vec![hit("src/b.rs", "g", 1)]);
    }
);
case!(
    R5_BLK,
    test_rust_import_scope_outer_import_reaches_inner_block_binds_import,
    test_rust_import_scope_outer_import_reaches_inner_block_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_k"), vec![hit("src/b.rs", "g", 1)]);
    }
);

// ---- unresolved-import-blocks-fallback ----
// The `use` binds the name even when its path does not resolve, so the
// outer `g` is shadowed (Reference, Use declarations; Names > Scopes).

const R6_MOD: &str = r#"fn g() {}
mod inner {
    use crate::missing::g;
    pub fn caller_m() { g(); }
}
"#;
const R6_FN: &str = r#"fn g() {}
fn caller_f() {
    use crate::missing::g;
    g();
}
"#;
const R6_BLK: &str = r#"fn g() {}
fn caller_k() {
    {
        use crate::missing::g;
        g();
    }
}
"#;

case!(
    #[ignore = "needs import scope: an unresolved scoped import falls back to the outer namesake (C4 scope table)"]
    R6_MOD,
    test_rust_import_scope_unresolved_import_blocks_fallback_inline_mod_no_outer_edge,
    test_rust_import_scope_unresolved_import_blocks_fallback_inline_mod_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_m").contains(&hit("src/lib.rs", "g", 1)));
    }
);
case!(
    #[ignore = "needs import scope: an unresolved scoped import falls back to the outer namesake (C4 scope table)"]
    R6_FN,
    test_rust_import_scope_unresolved_import_blocks_fallback_fn_body_no_outer_edge,
    test_rust_import_scope_unresolved_import_blocks_fallback_fn_body_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_f").contains(&hit("src/lib.rs", "g", 1)));
    }
);
case!(
    #[ignore = "needs import scope: an unresolved scoped import falls back to the outer namesake (C4 scope table)"]
    R6_BLK,
    test_rust_import_scope_unresolved_import_blocks_fallback_block_no_outer_edge,
    test_rust_import_scope_unresolved_import_blocks_fallback_block_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_k").contains(&hit("src/lib.rs", "g", 1)));
    }
);

// ---- alias-import ----
// `use p::g as h;` binds `h` (not `g`) in the scope; a comment between the
// import and the call changes nothing (Reference, Use declarations > "as").

const R7_MOD: &str = r#"mod b;
mod inner {
    use crate::b::g as h;
    // a comment between the import and the call
    pub fn caller_m() { h(); }
}
"#;
const R7_FN: &str = r#"mod b;
fn caller_f() {
    use crate::b::g as h;
    // a comment between the import and the call
    h();
}
"#;
const R7_BLK: &str = r#"mod b;
fn caller_k() {
    {
        use crate::b::g as h;
        /* a comment between the import and the call */
        h();
    }
}
"#;

case!(
    R7_MOD,
    test_rust_import_scope_alias_import_inline_mod_binds_aliased_target,
    test_rust_import_scope_alias_import_inline_mod_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_m"), vec![hit("src/b.rs", "g", 1)]);
    }
);
case!(
    R7_FN,
    test_rust_import_scope_alias_import_fn_body_binds_aliased_target,
    test_rust_import_scope_alias_import_fn_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_f"), vec![hit("src/b.rs", "g", 1)]);
    }
);
case!(
    R7_BLK,
    test_rust_import_scope_alias_import_block_binds_aliased_target,
    test_rust_import_scope_alias_import_block_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_k"), vec![hit("src/b.rs", "g", 1)]);
    }
);

// ---- empty-scope ----
// A scope that imports and calls nothing adds no edge and does not panic.

const R8_MOD: &str = r#"mod b;
mod inner {
    use crate::b::g;
}
fn caller_top() {}
"#;

case!(
    R8_MOD,
    test_rust_import_scope_empty_scope_inline_mod_adds_no_edge,
    test_rust_import_scope_empty_scope_inline_mod_overlay_matches_full,
    |g| {
        assert!(calls(&g, "caller_top").is_empty());
    }
);
