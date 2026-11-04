//! TypeScript import scope: an `import H = B.g;` alias inside a
//! `namespace N { }` block is local to that block, and a file-level alias
//! applies to the whole file. Each fixture states the edge the TypeScript
//! scope rule gives (TypeScript Handbook "Namespaces" > "Aliases", and the
//! lexical lookup of non-exported namespace members), never the edge ecp
//! builds today. `b.ts` / `c.ts` are script files holding the global
//! namespaces `B` / `C`, so the aliases reference them without any `import`
//! or `export` in `a.ts` (a file with a top-level `import ... from` would turn
//! `a.ts` into a module and a namespace alias could not reach `B`).
//!
//! | Row | Scope kinds covered |
//! |---|---|
//! | inner-import-beats-outer-decl | namespace alias |
//! | sibling-isolation | two namespaces |
//! | no-leak-outward | namespace alias; ambient `declare module` import |
//! | inner-decl-beats-outer-import | namespace vs file-level alias |
//! | outer-import-reaches-inner | file-level alias, namespace; namespace, nested namespace |
//! | unresolved-import-blocks-fallback | namespace alias |
//! | reopened-scope | two `namespace N` blocks |
//! | empty scope | namespace alias, ambient module import |
//!
//! Rows skipped. A plain ESM `import` cannot sit inside a function or a
//! regular namespace (TS1232), so no function or block scope exists for the
//! ESM form. An ambient `declare module "m" { }` body cannot hold a function
//! body (TS1183), so it has no caller and no row but no-leak-outward and
//! empty scope. The function-local `require` cases live in
//! `ts_js_import_bound_calls.rs` and are not repeated here.
use crate::import_scope_support::{assert_overlay_matches_full, build, calls, hit, Hit};
use ecp_analyzer::typescript::parser::TypeScriptProvider;

fn provider() -> TypeScriptProvider {
    TypeScriptProvider::new().expect("TypeScriptProvider::new")
}

const B_TS: &str = "namespace B {\n    export function g() {}\n}\n";
const C_TS: &str = "namespace C {\n    export function g() {}\n}\n";

fn in_b() -> Hit {
    hit("b.ts", "g", 2)
}
fn in_c() -> Hit {
    hit("c.ts", "g", 2)
}

/// One fixture, one target test and one overlay-parity test. Every
/// `(caller, [hits])` pair is compared exactly.
macro_rules! scope_case {
    ($(#[$attr:meta])* $target:ident, $overlay:ident, $a:expr, [$(($caller:expr, [$($h:expr),*])),+ $(,)?]) => {
        $(#[$attr])*
        #[test]
        fn $target() {
            let g = build(
                &provider(),
                &[("a.ts", $a), ("b.ts", B_TS), ("c.ts", C_TS)],
                &[],
            );
            $(assert_eq!(calls(&g, $caller), vec![$($h),*], "caller {}", $caller);)+
        }

        #[test]
        fn $overlay() {
            assert_overlay_matches_full(
                &provider(),
                &[("a.ts", $a), ("b.ts", B_TS), ("c.ts", C_TS)],
                &[],
                "a.ts",
            );
        }
    };
}

// ---- inner-import-beats-outer-decl ----
// Lexical lookup: the alias `H` declared in `N` is found before the global
// function `H` (Handbook "Namespaces" > "Aliases"; spec name resolution).
const INNER_BEATS_OUTER: &str = r#"function H() {}
namespace N {
    import H = B.g;
    export function callerInner() { H(); }
}
"#;
scope_case!(
    #[ignore = "needs TS `import X = A.B` alias rows and import scope (C4 scope table)"]
    test_typescript_import_scope_inner_import_beats_outer_decl_namespace_alias_binds_import,
    test_typescript_import_scope_inner_import_beats_outer_decl_namespace_alias_overlay_matches_full,
    INNER_BEATS_OUTER,
    [("callerInner", [in_b()])]
);

// ---- sibling-isolation ----
const SIBLING: &str = r#"namespace N1 {
    import H = B.g;
    export function callerOne() { H(); }
}
namespace N2 {
    import H = C.g;
    export function callerTwo() { H(); }
}
"#;
scope_case!(
    #[ignore = "needs TS `import X = A.B` alias rows and import scope (C4 scope table)"]
    test_typescript_import_scope_sibling_isolation_namespace_alias_binds_each_own,
    test_typescript_import_scope_sibling_isolation_namespace_alias_overlay_matches_full,
    SIBLING,
    [("callerOne", [in_b()]), ("callerTwo", [in_c()])]
);

// ---- no-leak-outward ----
// A namespace-local alias is invisible outside the namespace block.
const NO_LEAK: &str = r#"namespace N {
    import H = B.g;
    export function inside() { H(); }
}
function callerOutside() { H(); }
"#;
scope_case!(
    #[ignore = "needs TS `import X = A.B` alias rows and import scope (C4 scope table)"]
    test_typescript_import_scope_no_leak_outward_namespace_alias_outside_has_no_edge,
    test_typescript_import_scope_no_leak_outward_namespace_alias_overlay_matches_full,
    NO_LEAK,
    [("inside", [in_b()]), ("callerOutside", [])]
);

// An import inside an ambient `declare module "m" { }` is scoped to that
// block (a relative specifier is illegal there, TS2439, so the specifier is
// bare and `tsconfig.json` `baseUrl` resolves it to `bm.ts`).
const AMBIENT_A: &str = r#"declare module "m" {
    import { g } from "bm";
}
function callerOutside() { g(); }
"#;
const AMBIENT_BM: &str = "export function g() {}\n";
const AMBIENT_TSCONFIG: &str = "{\n  \"compilerOptions\": { \"baseUrl\": \".\" }\n}\n";
const AMBIENT_SOURCES: &[(&str, &str)] = &[("a.ts", AMBIENT_A), ("bm.ts", AMBIENT_BM)];
const AMBIENT_AUX: &[(&str, &str)] = &[("tsconfig.json", AMBIENT_TSCONFIG)];

#[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
#[test]
fn test_typescript_import_scope_no_leak_outward_ambient_module_import_outside_has_no_edge() {
    let g = build(&provider(), AMBIENT_SOURCES, AMBIENT_AUX);
    assert_eq!(calls(&g, "callerOutside"), Vec::<Hit>::new());
}

#[test]
fn test_typescript_import_scope_no_leak_outward_ambient_module_import_overlay_matches_full() {
    assert_overlay_matches_full(&provider(), AMBIENT_SOURCES, AMBIENT_AUX, "a.ts");
}

// ---- inner-decl-beats-outer-import ----
const INNER_DECL: &str = r#"import H = B.g;
namespace N {
    function H() {}
    export function callerNs() { H(); }
}
"#;
scope_case!(
    test_typescript_import_scope_inner_decl_beats_outer_import_namespace_binds_inner,
    test_typescript_import_scope_inner_decl_beats_outer_import_namespace_overlay_matches_full,
    INNER_DECL,
    [("callerNs", [hit("a.ts", "H", 3)])]
);

// ---- outer-import-reaches-inner ----
const OUTER_FILE_TO_NS: &str = r#"import H = B.g;
namespace N {
    export function callerNs() { H(); }
}
"#;
scope_case!(#[ignore = "needs TS `import X = A.B` alias rows and import scope (C4 scope table)"] 
    test_typescript_import_scope_outer_import_reaches_inner_file_to_namespace_alias_binds_import,
    test_typescript_import_scope_outer_import_reaches_inner_file_to_namespace_alias_overlay_matches_full,
    OUTER_FILE_TO_NS,
    [("callerNs", [in_b()])]
);

const OUTER_NS_TO_NS: &str = r#"namespace N {
    import H = B.g;
    namespace M {
        export function callerNested() { H(); }
    }
}
"#;
scope_case!(#[ignore = "needs TS `import X = A.B` alias rows and import scope (C4 scope table)"] 
    test_typescript_import_scope_outer_import_reaches_inner_namespace_to_namespace_alias_binds_import,
    test_typescript_import_scope_outer_import_reaches_inner_namespace_to_namespace_alias_overlay_matches_full,
    OUTER_NS_TO_NS,
    [("callerNested", [in_b()])]
);

// ---- unresolved-import-blocks-fallback ----
// `Missing` is not in the fixture; the alias still binds `H` in `N`, so the
// global `H` is not reachable from the call.
const UNRESOLVED: &str = r#"function H() {}
namespace N {
    import H = Missing.g;
    export function callerNs() { H(); }
}
"#;
scope_case!(#[ignore = "needs TS `import X = A.B` alias rows and import scope (C4 scope table)"] 
    test_typescript_import_scope_unresolved_import_blocks_fallback_namespace_alias_has_no_edge,
    test_typescript_import_scope_unresolved_import_blocks_fallback_namespace_alias_overlay_matches_full,
    UNRESOLVED,
    [("callerNs", [])]
);

// ---- reopened-scope ----
// A non-exported alias belongs to one namespace block: only members declared
// with `export` merge across reopenings, so `callerReopened` does not see `H`.
const REOPENED: &str = r#"namespace N {
    import H = B.g;
    export function callerFirst() { H(); }
}
namespace N {
    export function callerReopened() { H(); }
}
"#;
scope_case!(
    #[ignore = "needs TS `import X = A.B` alias rows and import scope (C4 scope table)"]
    test_typescript_import_scope_reopened_scope_namespace_alias_only_importing_block_binds,
    test_typescript_import_scope_reopened_scope_namespace_alias_overlay_matches_full,
    REOPENED,
    [("callerFirst", [in_b()]), ("callerReopened", [])]
);

// ---- empty scope ----
const EMPTY_SCOPE: &str = r#"namespace N {
    import H = B.g;
}
function callerUnrelated() {}
"#;
scope_case!(
    test_typescript_import_scope_empty_scope_namespace_alias_has_no_edge,
    test_typescript_import_scope_empty_scope_namespace_alias_overlay_matches_full,
    EMPTY_SCOPE,
    [("callerUnrelated", [])]
);
