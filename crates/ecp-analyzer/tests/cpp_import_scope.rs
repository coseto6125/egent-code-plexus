//! C++ import scope: a using-declaration (`using b::g;`) or a using-directive
//! (`using namespace b;`) inside a namespace body, a function body or a block
//! applies to that scope only, while one at file scope applies to the whole
//! file. Each fixture states the edge the C++ lookup rule gives (ISO C++
//! [namespace.udecl], [namespace.udir], [basic.lookup.unqual]), never the edge
//! ecp builds today. `b::g` and `c::g` live in `b.h` / `c.h` as inline
//! definitions, so each header holds exactly one target node.
//!
//! Directive-versus-declaration asymmetry: names nominated by a using-directive
//! appear as if declared in the nearest enclosing namespace that contains both
//! the directive and the nominated namespace ([namespace.udir]/2). With a
//! global `g` in the file, `using namespace b;` makes the call ambiguous, so
//! the directive form has no row for inner-import-beats-outer-decl.
//!
//! | Row | Scope kinds covered |
//! |---|---|
//! | inner-import-beats-outer-decl | namespace, function, block (declaration form) |
//! | directive-hoists-to-common-namespace | nested namespace (directive form) |
//! | sibling-isolation | namespace, function, block (both forms) |
//! | no-leak-outward | namespace, function, block (both forms) |
//! | inner-decl-beats-outer-import | namespace (both forms) |
//! | inner-import-beats-outer-import | block (declaration form) |
//! | outer-import-reaches-inner | namespace, function-to-block, namespace-to-namespace (both forms) |
//! | unresolved-import-blocks-fallback | namespace, function, block (declaration form) |
//! | reopened-scope | two `namespace n` blocks (both forms) |
//! | empty scope, comment between import and call | namespace, function |
use crate::import_scope_support::{assert_overlay_matches_full, build, calls, hit};
use ecp_analyzer::cpp::parser::CppProvider;

fn provider() -> CppProvider {
    CppProvider::new().expect("CppProvider::new")
}

const B_H: &str = "namespace b {\ninline void g() {}\n}\n";
const C_H: &str = "namespace c {\ninline void g() {}\n}\n";

fn in_b() -> crate::import_scope_support::Hit {
    hit("b.h", "g", 2)
}
fn in_c() -> crate::import_scope_support::Hit {
    hit("c.h", "g", 2)
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
                &[("a.cpp", $a), ("b.h", B_H), ("c.h", C_H)],
                &[],
            );
            $(assert_eq!(calls(&g, $caller), vec![$($h),*], "caller {}", $caller);)+
        }

        #[test]
        fn $overlay() {
            assert_overlay_matches_full(
                &provider(),
                &[("a.cpp", $a), ("b.h", B_H), ("c.h", C_H)],
                &[],
                "a.cpp",
            );
        }
    };
}

// ---- inner-import-beats-outer-decl ----
// [namespace.udecl]/1: a using-declaration declares the name in its own
// scope, so lookup from inside `n` finds `b::g` before the global `g`.
const INNER_BEATS_OUTER_NS: &str = r#"#include "b.h"
void g() {}
namespace n {
    using b::g;
    void caller_ns_decl() { g(); }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_inner_import_beats_outer_decl_namespace_declaration_binds_import,
    test_cpp_import_scope_inner_import_beats_outer_decl_namespace_declaration_overlay_matches_full,
    INNER_BEATS_OUTER_NS,
    [("caller_ns_decl", [in_b()])]
);

// [namespace.udecl]/1 + [basic.scope.block]: a block-scope using-declaration
// hides the global `g` for the rest of the function body.
const INNER_BEATS_OUTER_FN: &str = r#"#include "b.h"
void g() {}
void caller_fn_decl() {
    using b::g;
    g();
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_inner_import_beats_outer_decl_function_declaration_binds_import,
    test_cpp_import_scope_inner_import_beats_outer_decl_function_declaration_overlay_matches_full,
    INNER_BEATS_OUTER_FN,
    [("caller_fn_decl", [in_b()])]
);

// Same rule, nested block scope inside the function.
const INNER_BEATS_OUTER_BLOCK: &str = r#"#include "b.h"
void g() {}
void caller_blk_decl() {
    {
        using b::g;
        g();
    }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_inner_import_beats_outer_decl_block_declaration_binds_import,
    test_cpp_import_scope_inner_import_beats_outer_decl_block_declaration_overlay_matches_full,
    INNER_BEATS_OUTER_BLOCK,
    [("caller_blk_decl", [in_b()])]
);

// ---- directive-hoists-to-common-namespace ----
// [namespace.udir]/2: `b`'s names appear as if in the global namespace (the
// nearest namespace containing both `n` and `b`). Unqualified lookup from
// `n` finds nothing in `n`, then finds `outer::g` in `outer` before it ever
// reaches the global namespace, so the call binds `outer::g`.
const DIRECTIVE_HOIST: &str = r#"#include "b.h"
namespace outer {
    void g() {}
    namespace n {
        using namespace b;
        void caller_dir_nested() { g(); }
    }
}
"#;
scope_case!(
    test_cpp_import_scope_directive_hoists_to_common_namespace_nested_namespace_binds_outer_decl,
    test_cpp_import_scope_directive_hoists_to_common_namespace_nested_namespace_overlay_matches_full,
    DIRECTIVE_HOIST,
    [("caller_dir_nested", [hit("a.cpp", "g", 3)])]
);

// ---- sibling-isolation ----
// [namespace.udecl]/[namespace.udir]: each directive/declaration affects its
// own namespace scope only.
const SIBLING_NS_DECL: &str = r#"#include "b.h"
#include "c.h"
namespace n1 {
    using b::g;
    void caller_one() { g(); }
}
namespace n2 {
    using c::g;
    void caller_two() { g(); }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_sibling_isolation_namespace_declaration_binds_each_own,
    test_cpp_import_scope_sibling_isolation_namespace_declaration_overlay_matches_full,
    SIBLING_NS_DECL,
    [("caller_one", [in_b()]), ("caller_two", [in_c()])]
);

const SIBLING_NS_DIR: &str = r#"#include "b.h"
#include "c.h"
namespace n1 {
    using namespace b;
    void caller_one() { g(); }
}
namespace n2 {
    using namespace c;
    void caller_two() { g(); }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_sibling_isolation_namespace_directive_binds_each_own,
    test_cpp_import_scope_sibling_isolation_namespace_directive_overlay_matches_full,
    SIBLING_NS_DIR,
    [("caller_one", [in_b()]), ("caller_two", [in_c()])]
);

const SIBLING_FN_DECL: &str = r#"#include "b.h"
#include "c.h"
void caller_one() {
    using b::g;
    g();
}
void caller_two() {
    using c::g;
    g();
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_sibling_isolation_function_declaration_binds_each_own,
    test_cpp_import_scope_sibling_isolation_function_declaration_overlay_matches_full,
    SIBLING_FN_DECL,
    [("caller_one", [in_b()]), ("caller_two", [in_c()])]
);

const SIBLING_FN_DIR: &str = r#"#include "b.h"
#include "c.h"
void caller_one() {
    using namespace b;
    g();
}
void caller_two() {
    using namespace c;
    g();
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_sibling_isolation_function_directive_binds_each_own,
    test_cpp_import_scope_sibling_isolation_function_directive_overlay_matches_full,
    SIBLING_FN_DIR,
    [("caller_one", [in_b()]), ("caller_two", [in_c()])]
);

// Two sibling blocks of one function: one caller, so the expectation is both
// targets. A wrong binding of both calls to the first import yields one hit.
const SIBLING_BLOCK_DECL: &str = r#"#include "b.h"
#include "c.h"
void caller_blocks() {
    {
        using b::g;
        g();
    }
    {
        using c::g;
        g();
    }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_sibling_isolation_block_declaration_binds_each_own,
    test_cpp_import_scope_sibling_isolation_block_declaration_overlay_matches_full,
    SIBLING_BLOCK_DECL,
    [("caller_blocks", [in_b(), in_c()])]
);

const SIBLING_BLOCK_DIR: &str = r#"#include "b.h"
#include "c.h"
void caller_blocks() {
    {
        using namespace b;
        g();
    }
    {
        using namespace c;
        g();
    }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_sibling_isolation_block_directive_binds_each_own,
    test_cpp_import_scope_sibling_isolation_block_directive_overlay_matches_full,
    SIBLING_BLOCK_DIR,
    [("caller_blocks", [in_b(), in_c()])]
);

// ---- no-leak-outward ----
// The file declares no `g`, so the outside call finds nothing ([basic.lookup.unqual]).
const LEAK_NS_DECL: &str = r#"#include "b.h"
namespace n {
    using b::g;
}
void caller_outside() { g(); }
"#;
scope_case!(
    test_cpp_import_scope_no_leak_outward_namespace_declaration_outside_has_no_edge,
    test_cpp_import_scope_no_leak_outward_namespace_declaration_overlay_matches_full,
    LEAK_NS_DECL,
    [("caller_outside", [])]
);

const LEAK_NS_DIR: &str = r#"#include "b.h"
namespace n {
    using namespace b;
}
void caller_outside() { g(); }
"#;
scope_case!(
    test_cpp_import_scope_no_leak_outward_namespace_directive_outside_has_no_edge,
    test_cpp_import_scope_no_leak_outward_namespace_directive_overlay_matches_full,
    LEAK_NS_DIR,
    [("caller_outside", [])]
);

const LEAK_FN_DECL: &str = r#"#include "b.h"
void caller_inside() {
    using b::g;
    g();
}
void caller_outside() { g(); }
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_no_leak_outward_function_declaration_outside_has_no_edge,
    test_cpp_import_scope_no_leak_outward_function_declaration_overlay_matches_full,
    LEAK_FN_DECL,
    [("caller_inside", [in_b()]), ("caller_outside", [])]
);

const LEAK_FN_DIR: &str = r#"#include "b.h"
void caller_inside() {
    using namespace b;
    g();
}
void caller_outside() { g(); }
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_no_leak_outward_function_directive_outside_has_no_edge,
    test_cpp_import_scope_no_leak_outward_function_directive_overlay_matches_full,
    LEAK_FN_DIR,
    [("caller_inside", [in_b()]), ("caller_outside", [])]
);

// The only call sits after the block that holds the import.
const LEAK_BLOCK_DECL: &str = r#"#include "b.h"
void caller_after_block() {
    {
        using b::g;
    }
    g();
}
"#;
scope_case!(
    test_cpp_import_scope_no_leak_outward_block_declaration_after_block_has_no_edge,
    test_cpp_import_scope_no_leak_outward_block_declaration_overlay_matches_full,
    LEAK_BLOCK_DECL,
    [("caller_after_block", [])]
);

const LEAK_BLOCK_DIR: &str = r#"#include "b.h"
void caller_after_block() {
    {
        using namespace b;
    }
    g();
}
"#;
scope_case!(
    test_cpp_import_scope_no_leak_outward_block_directive_after_block_has_no_edge,
    test_cpp_import_scope_no_leak_outward_block_directive_overlay_matches_full,
    LEAK_BLOCK_DIR,
    [("caller_after_block", [])]
);

// ---- inner-decl-beats-outer-import ----
// [basic.lookup.unqual]: lookup stops at the first scope with a declaration,
// so `n::g` is found before the file-scope using-declaration / directive.
const INNER_DECL_DECL: &str = r#"#include "b.h"
using b::g;
namespace n {
    void g() {}
    void caller_ns() { g(); }
}
"#;
scope_case!(
    test_cpp_import_scope_inner_decl_beats_outer_import_namespace_declaration_binds_inner,
    test_cpp_import_scope_inner_decl_beats_outer_import_namespace_declaration_overlay_matches_full,
    INNER_DECL_DECL,
    [("caller_ns", [hit("a.cpp", "g", 4)])]
);

const INNER_DECL_DIR: &str = r#"#include "b.h"
using namespace b;
namespace n {
    void g() {}
    void caller_ns() { g(); }
}
"#;
scope_case!(
    test_cpp_import_scope_inner_decl_beats_outer_import_namespace_directive_binds_inner,
    test_cpp_import_scope_inner_decl_beats_outer_import_namespace_directive_overlay_matches_full,
    INNER_DECL_DIR,
    [("caller_ns", [hit("a.cpp", "g", 4)])]
);

// ---- inner-import-beats-outer-import ----
// A block-scope using-declaration hides the file-scope one.
const INNER_IMPORT_BEATS_IMPORT: &str = r#"#include "b.h"
#include "c.h"
using b::g;
void caller_inner_import() {
    using c::g;
    g();
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_inner_import_beats_outer_import_function_declaration_binds_inner,
    test_cpp_import_scope_inner_import_beats_outer_import_function_declaration_overlay_matches_full,
    INNER_IMPORT_BEATS_IMPORT,
    [("caller_inner_import", [in_c()])]
);

// ---- outer-import-reaches-inner ----
// [basic.lookup.unqual]: lookup walks outward through enclosing scopes.
const OUTER_NS_DECL: &str = r#"#include "b.h"
using b::g;
namespace n {
    void caller_ns() { g(); }
}
"#;
scope_case!(#[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"] 
    test_cpp_import_scope_outer_import_reaches_inner_file_to_namespace_declaration_binds_import,
    test_cpp_import_scope_outer_import_reaches_inner_file_to_namespace_declaration_overlay_matches_full,
    OUTER_NS_DECL,
    [("caller_ns", [in_b()])]
);

const OUTER_NS_DIR: &str = r#"#include "b.h"
using namespace b;
namespace n {
    void caller_ns() { g(); }
}
"#;
scope_case!(#[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"] 
    test_cpp_import_scope_outer_import_reaches_inner_file_to_namespace_directive_binds_import,
    test_cpp_import_scope_outer_import_reaches_inner_file_to_namespace_directive_overlay_matches_full,
    OUTER_NS_DIR,
    [("caller_ns", [in_b()])]
);

const OUTER_BLOCK_DECL: &str = r#"#include "b.h"
using b::g;
void caller_blk() {
    {
        g();
    }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_outer_import_reaches_inner_file_to_block_declaration_binds_import,
    test_cpp_import_scope_outer_import_reaches_inner_file_to_block_declaration_overlay_matches_full,
    OUTER_BLOCK_DECL,
    [("caller_blk", [in_b()])]
);

const OUTER_BLOCK_DIR: &str = r#"#include "b.h"
using namespace b;
void caller_blk() {
    {
        g();
    }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_outer_import_reaches_inner_file_to_block_directive_binds_import,
    test_cpp_import_scope_outer_import_reaches_inner_file_to_block_directive_overlay_matches_full,
    OUTER_BLOCK_DIR,
    [("caller_blk", [in_b()])]
);

const OUTER_FN_TO_BLOCK_DECL: &str = r#"#include "b.h"
void caller_fn_to_blk() {
    using b::g;
    {
        g();
    }
}
"#;
scope_case!(#[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"] 
    test_cpp_import_scope_outer_import_reaches_inner_function_to_block_declaration_binds_import,
    test_cpp_import_scope_outer_import_reaches_inner_function_to_block_declaration_overlay_matches_full,
    OUTER_FN_TO_BLOCK_DECL,
    [("caller_fn_to_blk", [in_b()])]
);

const OUTER_FN_TO_BLOCK_DIR: &str = r#"#include "b.h"
void caller_fn_to_blk() {
    using namespace b;
    {
        g();
    }
}
"#;
scope_case!(#[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"] 
    test_cpp_import_scope_outer_import_reaches_inner_function_to_block_directive_binds_import,
    test_cpp_import_scope_outer_import_reaches_inner_function_to_block_directive_overlay_matches_full,
    OUTER_FN_TO_BLOCK_DIR,
    [("caller_fn_to_blk", [in_b()])]
);

const OUTER_NS_TO_NS_DECL: &str = r#"#include "b.h"
namespace n {
    using b::g;
    namespace inner {
        void caller_nested() { g(); }
    }
}
"#;
scope_case!(#[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"] 
    test_cpp_import_scope_outer_import_reaches_inner_namespace_to_namespace_declaration_binds_import,
    test_cpp_import_scope_outer_import_reaches_inner_namespace_to_namespace_declaration_overlay_matches_full,
    OUTER_NS_TO_NS_DECL,
    [("caller_nested", [in_b()])]
);

const OUTER_NS_TO_NS_DIR: &str = r#"#include "b.h"
namespace n {
    using namespace b;
    namespace inner {
        void caller_nested() { g(); }
    }
}
"#;
scope_case!(#[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"] 
    test_cpp_import_scope_outer_import_reaches_inner_namespace_to_namespace_directive_binds_import,
    test_cpp_import_scope_outer_import_reaches_inner_namespace_to_namespace_directive_overlay_matches_full,
    OUTER_NS_TO_NS_DIR,
    [("caller_nested", [in_b()])]
);

// ---- unresolved-import-blocks-fallback ----
// `ext.h` is not in the fixture, so `ext::g` has no target node. The
// using-declaration still hides the global `g` ([namespace.udecl]/1), so the
// call must not bind it.
const UNRESOLVED_NS: &str = r#"#include "ext.h"
void g() {}
namespace n {
    using ext::g;
    void caller_ns() { g(); }
}
"#;
scope_case!(#[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"] 
    test_cpp_import_scope_unresolved_import_blocks_fallback_namespace_declaration_has_no_edge,
    test_cpp_import_scope_unresolved_import_blocks_fallback_namespace_declaration_overlay_matches_full,
    UNRESOLVED_NS,
    [("caller_ns", [])]
);

const UNRESOLVED_FN: &str = r#"#include "ext.h"
void g() {}
void caller_fn() {
    using ext::g;
    g();
}
"#;
scope_case!(#[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"] 
    test_cpp_import_scope_unresolved_import_blocks_fallback_function_declaration_has_no_edge,
    test_cpp_import_scope_unresolved_import_blocks_fallback_function_declaration_overlay_matches_full,
    UNRESOLVED_FN,
    [("caller_fn", [])]
);

const UNRESOLVED_BLOCK: &str = r#"#include "ext.h"
void g() {}
void caller_blk() {
    {
        using ext::g;
        g();
    }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_unresolved_import_blocks_fallback_block_declaration_has_no_edge,
    test_cpp_import_scope_unresolved_import_blocks_fallback_block_declaration_overlay_matches_full,
    UNRESOLVED_BLOCK,
    [("caller_blk", [])]
);

// ---- reopened-scope ----
// C++ result: a using-declaration in `namespace n` declares `g` into `n`
// ([namespace.udecl]/1), and the scope of a namespace member extends over
// every later reopening of `n` ([basic.scope.namespace]), so `caller_reopened`
// DOES bind `b::g`. This differs from C# and TS, where the import is local to
// one block. `caller_other` is in namespace `m`, which never sees `n`'s name.
const REOPENED_DECL: &str = r#"#include "b.h"
namespace n {
    using b::g;
    void caller_first() { g(); }
}
namespace n {
    void caller_reopened() { g(); }
}
namespace m {
    void caller_other() { g(); }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_reopened_scope_namespace_declaration_visible_in_reopening,
    test_cpp_import_scope_reopened_scope_namespace_declaration_overlay_matches_full,
    REOPENED_DECL,
    [
        ("caller_first", [in_b()]),
        ("caller_reopened", [in_b()]),
        ("caller_other", [])
    ]
);

// A using-directive in `namespace n` has the namespace scope of `n`, which
// spans the reopening; mainstream compilers accept the later call.
const REOPENED_DIR: &str = r#"#include "b.h"
namespace n {
    using namespace b;
    void caller_first() { g(); }
}
namespace n {
    void caller_reopened() { g(); }
}
namespace m {
    void caller_other() { g(); }
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_reopened_scope_namespace_directive_visible_in_reopening,
    test_cpp_import_scope_reopened_scope_namespace_directive_overlay_matches_full,
    REOPENED_DIR,
    [
        ("caller_first", [in_b()]),
        ("caller_reopened", [in_b()]),
        ("caller_other", [])
    ]
);

// ---- empty scope ----
// A scope with an import and no caller yields no edge and no panic.
const EMPTY_SCOPE: &str = r#"#include "b.h"
namespace n {
    using b::g;
}
void caller_unrelated() {}
"#;
scope_case!(
    test_cpp_import_scope_empty_scope_namespace_declaration_has_no_edge,
    test_cpp_import_scope_empty_scope_namespace_declaration_overlay_matches_full,
    EMPTY_SCOPE,
    [("caller_unrelated", [])]
);

// ---- comment between import and call ----
const COMMENT_BETWEEN: &str = r#"#include "b.h"
void g() {}
void caller_commented() {
    using b::g;
    // a line comment

    /* a block comment */ g();
}
"#;
scope_case!(
    #[ignore = "needs C++ using-declaration / using-directive import rows and import scope (C4 scope table)"]
    test_cpp_import_scope_comment_between_import_and_call_function_declaration_binds_import,
    test_cpp_import_scope_comment_between_import_and_call_function_declaration_overlay_matches_full,
    COMMENT_BETWEEN,
    [("caller_commented", [in_b()])]
);
