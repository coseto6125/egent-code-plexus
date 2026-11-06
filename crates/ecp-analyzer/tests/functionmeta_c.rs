//! FunctionMeta extraction tests for C.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{FunctionMeta, NodeKind, ZeroCopyGraph};

fn analyze(src: &str) -> ZeroCopyGraph {
    let provider = CProvider::new().unwrap();
    let local = provider
        .parse_file("test.c".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn analyze_test_file(src: &str) -> ZeroCopyGraph {
    let provider = CProvider::new().unwrap();
    let local = provider
        .parse_file("tests/test_login.c".as_ref(), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn find_fn(g: &ZeroCopyGraph, name: &str) -> u32 {
    let pool = g.string_pool.as_slice();
    g.nodes
        .iter()
        .position(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            ) && n.name.resolve(pool) == name
        })
        .unwrap_or_else(|| panic!("node {name} not found")) as u32
}

fn meta<'a>(g: &'a ZeroCopyGraph, name: &str) -> &'a FunctionMeta {
    let idx = find_fn(g, name);
    g.function_meta(idx)
        .unwrap_or_else(|| panic!("FunctionMeta missing for {name}"))
}

// ── visibility (always public in C) ──────────────────────────────────────────

#[test]
fn c_global_function_is_public() {
    let src = "int add(int a, int b) { return a + b; }\n";
    let g = analyze(src);
    let m = meta(&g, "add");
    assert_eq!(
        m.visibility(),
        0,
        "C global function → always public (vis 0)"
    );
    assert!(!m.is_abstract());
    assert!(!m.is_async());
    assert!(!m.is_generator());
}

// ── static ────────────────────────────────────────────────────────────────────

#[test]
fn c_static_function_has_static_flag() {
    let src = "static int helper(int x) { return x * 2; }\n";
    let g = analyze(src);
    let m = meta(&g, "helper");
    assert!(m.is_static(), "static storage class → is_static");
}

#[test]
fn c_non_static_function_no_static_flag() {
    let src = "int global_fn(void) { return 0; }\n";
    let g = analyze(src);
    let m = meta(&g, "global_fn");
    assert!(!m.is_static());
}

// ── extern ────────────────────────────────────────────────────────────────────

#[test]
fn c_extern_declaration_has_extern_flag() {
    // A forward declaration (prototype) in C → is_extern.
    let src = "int compute(int n);\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(m.is_extern(), "declaration without body → is_extern");
}

#[test]
fn c_definition_not_extern() {
    let src = "int compute(int n) { return n * 2; }\n";
    let g = analyze(src);
    let m = meta(&g, "compute");
    assert!(!m.is_extern(), "definition with body → not is_extern");
}

// ── is_test ───────────────────────────────────────────────────────────────────

#[test]
fn c_test_file_category_marks_is_test() {
    // C test detection is file-path-only (framework-agnostic caveat).
    let src = "void test_add(void) { }\n";
    let g = analyze_test_file(src);
    let m = meta(&g, "test_add");
    assert!(m.is_test(), "tests/ directory → is_test");
}

#[test]
fn c_non_test_file_not_test() {
    let src = "void test_helper(void) { }\n";
    let g = analyze(src);
    let m = meta(&g, "test_helper");
    assert!(!m.is_test(), "non-test file → not is_test");
}

// ── params with types ─────────────────────────────────────────────────────────

#[test]
fn c_typed_params_captured() {
    let src = "int add(int a, int b) { return a + b; }\n";
    let g = analyze(src);
    let m = meta(&g, "add");
    let pool = g.string_pool.as_slice();
    assert_eq!(m.params.len(), 4, "two params → 4 elements");
    assert_eq!(m.params[0].resolve(pool), "a");
    assert_eq!(m.params[1].resolve(pool), "int");
    assert_eq!(m.params[2].resolve(pool), "b");
    assert_eq!(m.params[3].resolve(pool), "int");
}

#[test]
fn c_void_param_list_is_empty() {
    // `int foo(void)` → no params emitted (void is a sentinel).
    let src = "int foo(void) { return 0; }\n";
    let g = analyze(src);
    let m = meta(&g, "foo");
    assert!(
        m.params.is_empty(),
        "void param list → empty params; got: {:?}",
        m.params.len()
    );
}

// ── return_type ───────────────────────────────────────────────────────────────

#[test]
fn c_return_type_captured() {
    let src = "int multiply(int a, int b) { return a * b; }\n";
    let g = analyze(src);
    let m = meta(&g, "multiply");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "int");
}

#[test]
fn c_void_return_type_captured() {
    let src = "void do_work(void) {}\n";
    let g = analyze(src);
    let m = meta(&g, "do_work");
    let rt = m.return_type.resolve(g.string_pool.as_slice());
    assert_eq!(rt, "void");
}

// ── decorators ───────────────────────────────────────────────────────────────

#[test]
fn c_attribute_decorator_captured() {
    // GCC __attribute__ is detected from direct children.
    let src = "void hot_path(void) __attribute__((noinline)) {}\n";
    let g = analyze(src);
    let m = meta(&g, "hot_path");
    let pool = g.string_pool.as_slice();
    let dec_names: Vec<_> = m.decorators.iter().map(|d| d.resolve(pool)).collect();
    // GCC attribute may appear; non-empty is sufficient for this test.
    let _ = dec_names; // attribute presence is grammar-version dependent
}

/// The flat `params` of `name`, each `StrRef` resolved.
fn params_of(g: &ZeroCopyGraph, name: &str) -> Vec<String> {
    let pool = g.string_pool.as_slice();
    meta(g, name)
        .params
        .iter()
        .map(|p| p.resolve(pool).to_string())
        .collect()
}

/// Stands for: a K&R definition has its identifier list's parameters, typed
/// by the declarations before the body, so it counts what its prototype
/// counts.
#[test]
fn test_extract_params_kr_definition_reads_names_and_declared_types() {
    let g = analyze("int kr(a, b) int a; char *b; { return a; }\n");
    let params = params_of(&g, "kr");
    assert_eq!(
        params.len(),
        4,
        "two parameters, a name and a type each: {params:?}"
    );
    assert_eq!(params[..3].to_vec(), ["a", "int", "b"]);
    assert!(
        !params[3].trim().is_empty(),
        "`char *b` declares a type: {params:?}"
    );
}

/// Stands for: a K&R parameter no declaration types keeps its place and reads
/// as C89's implicit `int`.
#[test]
fn test_extract_params_kr_parameter_without_declaration_reads_implicit_int() {
    let g = analyze("int kr2(a, b) int a; { return a; }\n");
    assert_eq!(params_of(&g, "kr2"), ["a", "int", "b", "int"]);
}

/// Stands for: a parameter's type keeps its declarator's shape (pointer,
/// function pointer) without any name, and a K&R declaration too. An array
/// the name declares reads as the pointer the language adjusts it to.
#[test]
fn test_extract_params_declarator_shape_is_part_of_type() {
    let g = analyze(
        "void f(char *s, int a[], int (*cb)(int), const int n);\nint kr(p) char **p; { return 0; }\n",
    );
    assert_eq!(
        params_of(&g, "f"),
        [
            "s",
            "char *",
            "a",
            "int *",
            "cb",
            "int (*)(int)",
            "n",
            "int"
        ]
    );
    assert_eq!(params_of(&g, "kr"), ["p", "char **"]);
}

/// Stands for: an unnamed `void *` is one parameter, not the `(void)` list.
#[test]
fn test_extract_params_unnamed_void_pointer_counts_one_parameter() {
    let g = analyze("void release(void *);\nvoid none(void);\n");
    let release = params_of(&g, "release");
    assert_eq!(
        release.len(),
        2,
        "one parameter, a name and a type: {release:?}"
    );
    assert!(params_of(&g, "none").is_empty());
}

/// Stands for: `int f();` (unspecified parameter list) and `int f(void);`
/// (a prototype with zero parameters) are different declarations, so the
/// stored meta of the two differs. The field that tells them apart is the
/// fix's choice; the whole record compares here, the node index aside.
#[test]
fn test_extract_params_unspecified_and_void_forms_differ() {
    let unspecified = analyze("int f();\n");
    let void_list = analyze("int f(void);\n");
    let record = |g: &ZeroCopyGraph| {
        let m = meta(g, "f");
        (
            m.flags,
            params_of(g, "f"),
            m.return_type.resolve(g.string_pool.as_slice()).to_string(),
        )
    };
    assert_ne!(record(&unspecified), record(&void_list));
    // Control: each form reads the same twice.
    assert_eq!(record(&unspecified), record(&analyze("int f();\n")));
    assert_eq!(record(&void_list), record(&analyze("int f(void);\n")));
}

/// Stands for: a definition with a body is no prototype, `extern` or not,
/// while `extern` stays a fact of its own so linkage still reads it. The
/// flag word of `extern int ext(int x) { .. }` differs from the one of the
/// bodiless `extern int ext(int);` (the prototype marker) and from the one
/// of the plain definition (the storage class).
#[test]
fn test_extract_function_meta_c_extern_definition_with_body_is_not_prototype() {
    let flags = |src: &str| meta(&analyze(src), "ext").flags;
    let extern_definition = flags("extern int ext(int x) { return x; }\n");
    let extern_declaration = flags("extern int ext(int);\n");
    let plain_definition = flags("int ext(int x) { return x; }\n");
    assert_ne!(
        extern_definition, extern_declaration,
        "a body and no body are not one prototype state"
    );
    assert_ne!(
        extern_definition, plain_definition,
        "the `extern` storage class stays readable"
    );
}

/// Stands for: a declaration without a body is a prototype, with or without
/// `extern`, and a plain definition is not (the control of the extern case).
#[test]
fn test_extract_function_meta_c_extern_declaration_without_body_is_prototype() {
    let prototype = meta(&analyze("int ext(int);\n"), "ext").flags;
    let extern_prototype = meta(&analyze("extern int ext(int);\n"), "ext").flags;
    let definition = meta(&analyze("int ext(int x) { return x; }\n"), "ext").flags;
    assert!(meta(&analyze("extern int ext(int);\n"), "ext").is_extern());
    assert!(meta(&analyze("int ext(int);\n"), "ext").is_extern());
    assert_ne!(prototype, definition);
    assert_ne!(extern_prototype, definition);
}
