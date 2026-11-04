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
//! | order-use-before-import (extra) | function body |
//! | order-reimport-restores-first (extra) | function body |
//! | directive-global (extra) | function body (import binds module global) |
//! | directive-nonlocal (extra) | nested function (import binds enclosing local) |
//! | consumers-qualified-head-split (extra) | two sibling functions |
//! | dynamic-load-siblings (extra) | two sibling functions |
//! | precedence-parameter-shadows-import (extra) | function parameter |
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

// ---- order-use-before-import ----
// An import anywhere in a function body makes the name local to the whole
// body, so a call before it is an unbound local, not the module `g` and not
// the later import (Execution model > Naming and binding: "local variable
// referenced before assignment").

const R10_FN: &str = r#"def g(): pass
def caller_ub():
    g()
    from b import g
"#;

case!(
    #[ignore = "needs import scope: a later import in the function makes the name an unbound local (C4 scope table)"]
    R10_FN,
    test_python_import_scope_order_use_before_import_function_body_binds_nothing,
    test_python_import_scope_order_use_before_import_function_body_overlay_matches_full,
    |g| {
        assert!(calls(&g, "caller_ub").is_empty());
    }
);

// ---- order-reimport-restores-first ----
// Each import rebinds the local name; the last binding before the call wins
// (Import system > "The import statement": binding is an assignment).

const R11_FN: &str = r#"def caller_ri():
    from b import g
    from c import g
    from b import g
    g()
"#;

case!(
    R11_FN,
    test_python_import_scope_order_reimport_restores_first_function_body_binds_last_import,
    test_python_import_scope_order_reimport_restores_first_function_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_ri"), vec![hit("b.py", "g", 1)]);
    }
);

// ---- directive-global ----
// `global g` makes an import inside the function bind the module-global `g`,
// visible to every other function (Simple statements > "The global statement").

const R12_FN: &str = r#"def setter():
    global g
    from b import g
def caller_glob():
    g()
"#;

case!(
    R12_FN,
    test_python_import_scope_directive_global_function_body_binds_module_global,
    test_python_import_scope_directive_global_function_body_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_glob"), vec![hit("b.py", "g", 1)]);
    }
);

// ---- directive-nonlocal ----
// `nonlocal g` makes an import in the nested function rebind `outer`'s `g`,
// which a sibling nested function reads (Simple statements > "The nonlocal statement").

const R13_NEST: &str = r#"def outer():
    g = None
    def setter():
        nonlocal g
        from b import g
    def caller_nl():
        g()
"#;

case!(
    R13_NEST,
    test_python_import_scope_directive_nonlocal_nested_function_binds_enclosing_import,
    test_python_import_scope_directive_nonlocal_nested_function_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_nl"), vec![hit("b.py", "g", 1)]);
    }
);

// ---- consumers-qualified-head-split ----
// The same alias `m` names a different module in each sibling function; a
// qualified call `m.g()` follows its own function's binding (Execution model
// > Resolution of names: the local binding is per function).

const R14_FN: &str = r#"def caller_qa():
    import b as m
    m.g()
def caller_qc():
    import c as m
    m.g()
"#;

case!(
    #[ignore = "needs import scope: sibling scopes share one file-wide import set (C4 scope table)"]
    R14_FN,
    test_python_import_scope_consumers_qualified_head_split_sibling_functions_bind_own_module,
    test_python_import_scope_consumers_qualified_head_split_sibling_functions_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_qa"), vec![hit("b.py", "g", 1)]);
        assert_eq!(calls(&g, "caller_qc"), vec![hit("c.py", "g", 1)]);
    }
);

// ---- dynamic-load-siblings ----
// `m = importlib.import_module("b")` is an assignment local to its function;
// two sibling functions each hold their own `m` (Execution model > Naming and
// binding; importlib.import_module returns the named module).

const R15_FN: &str = r#"import importlib
def caller_dl1():
    m = importlib.import_module("b")
    m.g()
def caller_dl2():
    m = importlib.import_module("b")
    m.g()
"#;

case!(
    R15_FN,
    test_python_import_scope_dynamic_load_siblings_function_bodies_bind_loaded_module,
    test_python_import_scope_dynamic_load_siblings_function_bodies_overlay_matches_full,
    |g| {
        assert_eq!(calls(&g, "caller_dl1"), vec![hit("b.py", "g", 1)]);
        assert_eq!(calls(&g, "caller_dl2"), vec![hit("b.py", "g", 1)]);
    }
);

// ---- precedence-parameter-shadows-import ----
// A parameter is a local of the function and shadows the module-level import;
// `g()` calls the argument, which is unknown statically (Execution model >
// Resolution of names; Compound statements > "Function definitions").

const R16_FN: &str = r#"from b import g
def caller_p(g):
    g()
"#;

case!(
    #[ignore = "needs import scope: a parameter of the same name shadows the import (C4 scope table)"]
    R16_FN,
    test_python_import_scope_precedence_parameter_shadows_import_function_body_adds_no_edge,
    test_python_import_scope_precedence_parameter_shadows_import_function_body_overlay_matches_full,
    |g| {
        assert!(!calls(&g, "caller_p").contains(&hit("b.py", "g", 1)));
    }
);
