//! Python import scope: an `import` / `from .. import` statement binds the
//! name in the scope that holds it (a function body), never in the whole
//! file, and a class body's names are not visible inside its methods.
//! Each fixture pins the edge the language's own scope rule gives (Python
//! Language Reference, "Execution model > Naming and binding"), not the
//! edge ecp builds today. Every import precedes its call, so the runtime
//! order and the textual order agree.
//!
//! | Row | Scope kinds covered |
//! |---|---|
//! | inner-import-beats-outer-decl | function body, nested function (import in the enclosing function) |
//! | sibling-isolation | function body, nested function |
//! | no-leak-outward | function body, nested function |
//! | inner-decl-beats-outer-import | function body, nested function |
//! | outer-import-reaches-inner | function body, nested function, class method (module import) |
//! | unresolved-import-blocks-fallback | function body, nested function |
//! | class-body-not-visible-in-method (extra) | class body, two fixtures |
//! | alias-import (extra) | function body, nested function |
//! | empty-scope (extra) | function body |
//!
//! Rows skipped: reopened-scope. A Python `class` or `def` statement
//! rebinds a name; it does not reopen the earlier scope.
//!
//! The class body has no inner-import-beats-outer-decl, sibling, no-leak,
//! inner-decl or unresolved row: no caller lives in a class body, and its
//! names never reach the methods (the extra class rows pin that).
//!
//! `b.py` and `c.py` each declare `def g` on line 1.

use crate::import_scope_support::{assert_overlay_matches_full, build, calls, hit};
use ecp_analyzer::python::parser::PythonProvider;

const B: &str = "def g(): pass\n";
const C: &str = "def g(): pass\n";

fn provider() -> PythonProvider {
    PythonProvider::new().expect("PythonProvider::new")
}

/// One target test (`|g| body` reads graph `g`) and one overlay test per fixture.
macro_rules! case {
    ($(#[$attr:meta])* $fixture:ident, $target:ident, $overlay:ident, |$g:ident| $body:block) => {
        $(#[$attr])*
        #[test]
        fn $target() {
            let $g = build(
                &provider(),
                &[("a.py", $fixture), ("b.py", B), ("c.py", C)],
                &[],
            );
            $body
        }

        #[test]
        fn $overlay() {
            assert_overlay_matches_full(
                &provider(),
                &[("a.py", $fixture), ("b.py", B), ("c.py", C)],
                &[],
                "a.py",
            );
        }
    };
}

// ---- inner-import-beats-outer-decl ----
// A local binding (import statement) in a function shadows the module-level
// `def g`; a nested function reads the enclosing function's binding
// (Execution model > Resolution of names: local, enclosing, global).

const R1_FN: &str = r#"def g(): pass
def caller_f():
    from b import g
    g()
"#;
const R1_NEST: &str = r#"def g(): pass
def outer():
    from b import g
    def caller_n():
        g()
"#;

case!(
    #[ignore = "needs import scope: the file's outer namesake beats the scoped import (C4 scope table)"]
    R1_FN,
    test_python_import_scope_inner_import_beats_outer_decl_function_body_binds_import,
    test_python_import_scope_inner_import_beats_outer_decl_function_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_f"), vec![hit("b.py", "g", 1)]);
    }
);
case!(
    #[ignore = "needs import scope: the file's outer namesake beats the scoped import (C4 scope table)"]
    R1_NEST,
    test_python_import_scope_inner_import_beats_outer_decl_nested_function_binds_import,
    test_python_import_scope_inner_import_beats_outer_decl_nested_function_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_n"), vec![hit("b.py", "g", 1)]);
    }
);

// ---- sibling-isolation ----
// Each function body is its own local scope (Execution model > Binding of names).

const R2_FN: &str = r#"def caller_x():
    from b import g
    g()
def caller_y():
    from c import g
    g()
"#;
const R2_NEST: &str = r#"def outer():
    def caller_x():
        from b import g
        g()
    def caller_y():
        from c import g
        g()
"#;

case!(
    #[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
    R2_FN,
    test_python_import_scope_sibling_isolation_function_body_binds_each_own,
    test_python_import_scope_sibling_isolation_function_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_x"), vec![hit("b.py", "g", 1)]);
        assert_eq!(calls(&g, "caller_y"), vec![hit("c.py", "g", 1)]);
    }
);
case!(
    #[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
    R2_NEST,
    test_python_import_scope_sibling_isolation_nested_function_binds_each_own,
    test_python_import_scope_sibling_isolation_nested_function_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_x"), vec![hit("b.py", "g", 1)]);
        assert_eq!(calls(&g, "caller_y"), vec![hit("c.py", "g", 1)]);
    }
);

// ---- no-leak-outward ----
// A name bound in a function is local to it; the enclosing scope does not
// see it (Execution model > Binding of names).

const R3_FN: &str = r#"def holder():
    from b import g
    g()
def caller_out():
    g()
"#;
const R3_NEST: &str = r#"def caller_out():
    def inner():
        from b import g
        g()
    g()
"#;

case!(
    #[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
    R3_FN,
    test_python_import_scope_no_leak_outward_function_body_outside_caller_unbound,
    test_python_import_scope_no_leak_outward_function_body_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_out").contains(&hit("b.py", "g", 1)));
    }
);
case!(
    #[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
    R3_NEST,
    test_python_import_scope_no_leak_outward_nested_function_outside_caller_unbound,
    test_python_import_scope_no_leak_outward_nested_function_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_out").contains(&hit("b.py", "g", 1)));
    }
);

// ---- inner-decl-beats-outer-import ----
// A `def g` in a function is a local binding and shadows the module-level
// import (Execution model > Resolution of names).

const R4_FN: &str = r#"from b import g
def caller_f():
    def g(): pass
    g()
"#;
const R4_NEST: &str = r#"from b import g
def outer():
    def g(): pass
    def caller_n():
        g()
"#;

case!(
    R4_FN,
    test_python_import_scope_inner_decl_beats_outer_import_function_body_binds_inner_decl,
    test_python_import_scope_inner_decl_beats_outer_import_function_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_f"), vec![hit("a.py", "g", 3)]);
    }
);
case!(
    R4_NEST,
    test_python_import_scope_inner_decl_beats_outer_import_nested_function_binds_inner_decl,
    test_python_import_scope_inner_decl_beats_outer_import_nested_function_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_n"), vec![hit("a.py", "g", 3)]);
    }
);

// ---- outer-import-reaches-inner ----
// A module-level name is a global of every function and method in the
// module (Execution model > Resolution of names: global scope).

const R5_FN: &str = r#"from b import g
def caller_f():
    g()
"#;
const R5_NEST: &str = r#"from b import g
def outer():
    def caller_n():
        g()
"#;
const R5_CLASS: &str = r#"from b import g
class C:
    def caller_m(self):
        g()
"#;

case!(
    R5_FN,
    test_python_import_scope_outer_import_reaches_inner_function_body_binds_import,
    test_python_import_scope_outer_import_reaches_inner_function_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_f"), vec![hit("b.py", "g", 1)]);
    }
);
case!(
    R5_NEST,
    test_python_import_scope_outer_import_reaches_inner_nested_function_binds_import,
    test_python_import_scope_outer_import_reaches_inner_nested_function_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_n"), vec![hit("b.py", "g", 1)]);
    }
);
case!(
    R5_CLASS,
    test_python_import_scope_outer_import_reaches_inner_class_method_binds_import,
    test_python_import_scope_outer_import_reaches_inner_class_method_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_m"), vec![hit("b.py", "g", 1)]);
    }
);

// ---- unresolved-import-blocks-fallback ----
// The import statement binds `g` locally even when the module is missing,
// so the module-level `g` is shadowed (Execution model > Binding of names).

const R6_FN: &str = r#"def g(): pass
def caller_f():
    from missing import g
    g()
"#;
const R6_NEST: &str = r#"def g(): pass
def outer():
    from missing import g
    def caller_n():
        g()
"#;

case!(
    #[ignore = "needs import scope: an unresolved scoped import falls back to the outer namesake (C4 scope table)"]
    R6_FN,
    test_python_import_scope_unresolved_import_blocks_fallback_function_body_no_outer_edge,
    test_python_import_scope_unresolved_import_blocks_fallback_function_body_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_f").contains(&hit("a.py", "g", 1)));
    }
);
case!(
    #[ignore = "needs import scope: an unresolved scoped import falls back to the outer namesake (C4 scope table)"]
    R6_NEST,
    test_python_import_scope_unresolved_import_blocks_fallback_nested_function_no_outer_edge,
    test_python_import_scope_unresolved_import_blocks_fallback_nested_function_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_n").contains(&hit("a.py", "g", 1)));
    }
);

// ---- class-body-not-visible-in-method ----
// "The scope of names defined in a class block is limited to the class
// block; it does not extend to the code blocks of methods" (Execution model
// > Resolution of names). So a method skips the class-body import and reads
// the module global `g`, or nothing when no global exists.

const R7_NOGLOBAL: &str = r#"class C:
    from b import g
    def caller_m(self):
        g()
"#;
const R7_GLOBAL: &str = r#"def g(): pass
class C:
    from b import g
    def caller_m(self):
        g()
"#;

case!(
    #[ignore = "needs import scope: the scoped import leaks outside its scope (C4 scope table)"]
    R7_NOGLOBAL,
    test_python_import_scope_class_body_not_visible_in_method_no_global_unbound,
    test_python_import_scope_class_body_not_visible_in_method_no_global_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_m").contains(&hit("b.py", "g", 1)));
    }
);
case!(
    R7_GLOBAL,
    test_python_import_scope_class_body_not_visible_in_method_binds_module_global,
    test_python_import_scope_class_body_not_visible_in_method_global_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_m"), vec![hit("a.py", "g", 1)]);
    }
);

// ---- alias-import ----
// `from b import g as h` binds `h` only; a comment between the import and
// the call changes nothing (Reference, "The import statement").

const R8_FN: &str = r#"def caller_f():
    from b import g as h
    # a comment between the import and the call
    h()
"#;
const R8_NEST: &str = r#"def outer():
    from b import g as h
    def caller_n():
        # a comment between the import and the call
        h()
"#;

case!(
    R8_FN,
    test_python_import_scope_alias_import_function_body_binds_aliased_target,
    test_python_import_scope_alias_import_function_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_f"), vec![hit("b.py", "g", 1)]);
    }
);
case!(
    R8_NEST,
    test_python_import_scope_alias_import_nested_function_binds_aliased_target,
    test_python_import_scope_alias_import_nested_function_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_n"), vec![hit("b.py", "g", 1)]);
    }
);

// ---- empty-scope ----
// A function that imports and calls nothing adds no edge and does not panic.

const R9_FN: &str = r#"def holder():
    from b import g
def caller_top():
    pass
"#;

case!(
    R9_FN,
    test_python_import_scope_empty_scope_function_body_adds_no_edge,
    test_python_import_scope_empty_scope_function_body_overlay_matches_full,
    |g| {
        assert!(calls(&g, "caller_top").is_empty());
    }
);
