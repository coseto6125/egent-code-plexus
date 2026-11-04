//! C# import scope: a `using` directive inside a block-bodied `namespace N { }`
//! applies to that namespace block only, while a compilation-unit `using`
//! applies to the whole file. Each fixture states the edge the C# scope rule
//! gives (ECMA-334 / C# spec "Namespaces" > "Using directives" and "Namespace
//! and type names"), never the edge ecp builds today. The import forms are
//! `using H = B.Helpers;` with `H.G()` and `using static B.Helpers;` with `G()`,
//! so the call depends on the import.
//!
//! | Row | Scope kinds covered |
//! |---|---|
//! | inner-import-beats-outer-decl | namespace block (alias) |
//! | sibling-isolation | namespace block (alias) |
//! | no-leak-outward | namespace block (alias) |
//! | inner-decl-beats-outer-import | namespace block vs compilation-unit using |
//! | outer-import-reaches-inner | compilation-unit using, namespace block |
//! | unresolved-import-blocks-fallback | namespace block (alias) |
//! | reopened-scope | two `namespace N` blocks |
//! | alias / using static | namespace block (`using static`) |
//! | empty scope | namespace block with a using and no caller |
//!
//! File-scoped `namespace N;` cannot be combined with block namespaces, so no
//! fixture uses it.
use crate::import_scope_support::{assert_overlay_matches_full, build, calls, hit};
use ecp_analyzer::c_sharp::parser::CSharpProvider;

fn provider() -> CSharpProvider {
    CSharpProvider::new().expect("CSharpProvider::new")
}

const B_CS: &str = r#"namespace B {
    public static class Helpers {
        public static void G() { }
    }
}
"#;

const C_CS: &str = r#"namespace C {
    public static class Helpers {
        public static void G() { }
    }
}
"#;

// ---- inner-import-beats-outer-decl ----
const INNER_BEATS_OUTER_A: &str = r#"class H {
    public static void G() { }
}
namespace N {
    using H = B.Helpers;
    class User {
        void CallerInner() { H.G(); }
    }
}
"#;
const INNER_BEATS_OUTER: &[(&str, &str)] = &[("A.cs", INNER_BEATS_OUTER_A), ("B.cs", B_CS)];

// Spec "Namespace and type names": a using alias in `N` is found before the
// type `H` declared in the enclosing global namespace.
#[ignore = "needs import scope: the file's outer namesake beats the scoped import (C4 scope table)"]
#[test]
fn test_csharp_import_scope_inner_import_beats_outer_decl_namespace_block_binds_import() {
    let g = build(&provider(), INNER_BEATS_OUTER, &[]);
    assert_eq!(calls(&g, "CallerInner"), vec![hit("B.cs", "G", 3)]);
}

#[test]
fn test_csharp_import_scope_inner_import_beats_outer_decl_namespace_block_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), INNER_BEATS_OUTER, &[], "A.cs");
}

// ---- sibling-isolation ----
const SIBLING_A: &str = r#"namespace N1 {
    using H = B.Helpers;
    class U1 {
        void CallerOne() { H.G(); }
    }
}
namespace N2 {
    using H = C.Helpers;
    class U2 {
        void CallerTwo() { H.G(); }
    }
}
"#;
const SIBLING: &[(&str, &str)] = &[("A.cs", SIBLING_A), ("B.cs", B_CS), ("C.cs", C_CS)];

// Spec "Using directives": a using directive affects only the namespace body
// that contains it, so sibling namespace blocks do not share aliases.
#[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
#[test]
fn test_csharp_import_scope_sibling_isolation_namespace_block_binds_each_own() {
    let g = build(&provider(), SIBLING, &[]);
    assert_eq!(calls(&g, "CallerOne"), vec![hit("B.cs", "G", 3)]);
    assert_eq!(calls(&g, "CallerTwo"), vec![hit("C.cs", "G", 3)]);
}

#[test]
fn test_csharp_import_scope_sibling_isolation_namespace_block_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), SIBLING, &[], "A.cs");
}

// ---- no-leak-outward ----
const NO_LEAK_A: &str = r#"namespace N1 {
    using H = B.Helpers;
    class U1 {
        void CallerInside() { H.G(); }
    }
}
namespace N2 {
    class U2 {
        void CallerOutside() { H.G(); }
    }
}
"#;
const NO_LEAK: &[(&str, &str)] = &[("A.cs", NO_LEAK_A), ("B.cs", B_CS)];

// Spec "Using directives": the alias is not visible outside its namespace
// body, so `H` in N2 names nothing. (The fixture does not compile under csc
// for that very reason; it pins the lookup rule.)
#[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
#[test]
fn test_csharp_import_scope_no_leak_outward_namespace_block_outside_has_no_edge() {
    let g = build(&provider(), NO_LEAK, &[]);
    assert_eq!(calls(&g, "CallerInside"), vec![hit("B.cs", "G", 3)]);
    assert!(!calls(&g, "CallerOutside").contains(&hit("B.cs", "G", 3)));
}

#[test]
fn test_csharp_import_scope_no_leak_outward_namespace_block_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), NO_LEAK, &[], "A.cs");
}

// ---- inner-decl-beats-outer-import ----
const INNER_DECL_A: &str = r#"using H = B.Helpers;
namespace N {
    class H {
        public static void G() { }
    }
    class User {
        void CallerDecl() { H.G(); }
    }
}
"#;
const INNER_DECL: &[(&str, &str)] = &[("A.cs", INNER_DECL_A), ("B.cs", B_CS)];

// Spec "Namespace and type names": a type member of `N` is found before the
// using alias of the enclosing compilation unit.
#[test]
fn test_csharp_import_scope_inner_decl_beats_outer_import_namespace_block_binds_decl() {
    let g = build(&provider(), INNER_DECL, &[]);
    assert_eq!(calls(&g, "CallerDecl"), vec![hit("A.cs", "G", 4)]);
}

#[test]
fn test_csharp_import_scope_inner_decl_beats_outer_import_namespace_block_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), INNER_DECL, &[], "A.cs");
}

// ---- outer-import-reaches-inner ----
const OUTER_REACHES_A: &str = r#"using H = B.Helpers;
namespace N {
    class User {
        void CallerNested() { H.G(); }
    }
}
"#;
const OUTER_REACHES: &[(&str, &str)] = &[("A.cs", OUTER_REACHES_A), ("B.cs", B_CS)];

// Spec "Using directives": a compilation-unit using is visible in every
// namespace body of the file.
#[test]
fn test_csharp_import_scope_outer_import_reaches_inner_namespace_block_binds_import() {
    let g = build(&provider(), OUTER_REACHES, &[]);
    assert_eq!(calls(&g, "CallerNested"), vec![hit("B.cs", "G", 3)]);
}

#[test]
fn test_csharp_import_scope_outer_import_reaches_inner_namespace_block_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), OUTER_REACHES, &[], "A.cs");
}

// ---- unresolved-import-blocks-fallback ----
const UNRESOLVED_A: &str = r#"class H {
    public static void G() { }
}
namespace N {
    using H = Missing.Helpers;
    class User {
        void CallerUnresolved() { H.G(); }
    }
}
"#;
const UNRESOLVED: &[(&str, &str)] = &[("A.cs", UNRESOLVED_A)];

// Spec "Namespace and type names": the alias `H` is bound in `N`, so lookup
// stops there even though its target is not in the repo.
#[ignore = "needs import scope: an unresolved scoped import falls back to the outer namesake (C4 scope table)"]
#[test]
fn test_csharp_import_scope_unresolved_import_blocks_fallback_namespace_block_no_edge() {
    let g = build(&provider(), UNRESOLVED, &[]);
    assert!(!calls(&g, "CallerUnresolved").contains(&hit("A.cs", "G", 2)));
}

#[test]
fn test_csharp_import_scope_unresolved_import_blocks_fallback_namespace_block_overlay_matches_full()
{
    assert_overlay_matches_full(&provider(), UNRESOLVED, &[], "A.cs");
}

// ---- reopened-scope ----
const REOPENED_A: &str = r#"namespace N {
    using H = B.Helpers;
    class U1 {
        void CallerImporting() { H.G(); }
    }
}
namespace N {
    class U2 {
        void CallerPlain() { H.G(); }
    }
}
"#;
const REOPENED: &[(&str, &str)] = &[("A.cs", REOPENED_A), ("B.cs", B_CS)];

// Spec "Using directives": a using directive affects its own namespace body,
// not another declaration of the same namespace in the same file.
#[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
#[test]
fn test_csharp_import_scope_reopened_scope_namespace_block_only_importing_block_binds() {
    let g = build(&provider(), REOPENED, &[]);
    assert_eq!(calls(&g, "CallerImporting"), vec![hit("B.cs", "G", 3)]);
    assert!(!calls(&g, "CallerPlain").contains(&hit("B.cs", "G", 3)));
}

#[test]
fn test_csharp_import_scope_reopened_scope_namespace_block_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), REOPENED, &[], "A.cs");
}

// ---- using static (bare call) ----
const USING_STATIC_A: &str = r#"namespace N {
    using static B.Helpers;
    class User {
        void CallerStatic() { G(); }
    }
}
namespace M {
    class Other {
        void CallerOtherStatic() { G(); }
    }
}
"#;
const USING_STATIC: &[(&str, &str)] = &[("A.cs", USING_STATIC_A), ("B.cs", B_CS)];

// Spec "Using static directives": `using static T;` makes T's static members
// callable by simple name within that namespace body only.
#[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
#[test]
fn test_csharp_import_scope_using_static_namespace_block_binds_only_inside() {
    let g = build(&provider(), USING_STATIC, &[]);
    assert_eq!(calls(&g, "CallerStatic"), vec![hit("B.cs", "G", 3)]);
    assert!(!calls(&g, "CallerOtherStatic").contains(&hit("B.cs", "G", 3)));
}

#[test]
fn test_csharp_import_scope_using_static_namespace_block_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), USING_STATIC, &[], "A.cs");
}

// ---- empty scope ----
const EMPTY_A: &str = r#"namespace Empty {
    using H = B.Helpers;
    /* no caller here */
}
namespace Full {
    class User {
        void CallerFull() { }
    }
}
"#;
const EMPTY: &[(&str, &str)] = &[("A.cs", EMPTY_A), ("B.cs", B_CS)];

// Spec "Using directives": a namespace body with a using and no member is legal.
#[test]
fn test_csharp_import_scope_empty_scope_namespace_block_no_edge_no_panic() {
    let g = build(&provider(), EMPTY, &[]);
    assert!(calls(&g, "CallerFull").is_empty());
}

#[test]
fn test_csharp_import_scope_empty_scope_namespace_block_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), EMPTY, &[], "A.cs");
}
