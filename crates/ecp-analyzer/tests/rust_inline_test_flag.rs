use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, ZeroCopyGraph};

fn assert_flags(path: &str, source: &str, expected: &[(&str, bool)]) -> ZeroCopyGraph {
    let local = RustProvider::new()
        .unwrap()
        .parse_file(path.as_ref(), source.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    let graph = builder.build();
    for (name, expected) in expected {
        let index = graph
            .nodes
            .iter()
            .position(|node| {
                matches!(node.kind, NodeKind::Function | NodeKind::Method)
                    && node.name.resolve(&graph.string_pool) == *name
            })
            .unwrap_or_else(|| panic!("missing callable {name}"));
        let meta = graph
            .function_meta(index as u32)
            .unwrap_or_else(|| panic!("missing metadata for {name}"));
        assert_eq!(meta.is_test(), *expected, "{path}: {name}");
    }
    graph
}

#[test]
fn test_extract_nested_modules_inherit_test_flag() {
    assert_flags(
        "src/lib.rs",
        r#"
fn production() {}
mod a {
    #[cfg(test)]
    mod b {
        fn helper() {}
        mod deeper { fn nested() {} }
        struct S;
        impl S { fn method(&self) {} }
    }
    fn sibling() {}
}
mod tests { fn name_only() {} }
"#,
        &[
            ("production", false),
            ("helper", true),
            ("nested", true),
            ("method", true),
            ("sibling", false),
            ("name_only", false),
        ],
    );
}

#[test]
fn test_extract_gated_impl_and_function_inherit_test_flag() {
    assert_flags(
        "src/lib.rs",
        r#"
struct S;
#[cfg(test)]
impl S { fn method(&self) {} }
#[cfg(test)]
fn outer() { mod nested { fn local() {} } }
fn normal() { mod nested { fn local_prod() {} } }
"#,
        &[
            ("method", true),
            ("outer", true),
            ("local", true),
            ("normal", false),
            ("local_prod", false),
        ],
    );
}

#[test]
fn test_extract_test_attributes_flag_functions() {
    assert_flags(
        "src/lib.rs",
        r#"
#[test]
fn standard() {}
#[tokio::test(flavor = "multi_thread")]
async fn asynchronous() {}
#[custom::test]
fn custom() { mod nested { fn helper() {} } }
#[ test ]
// Attributes still attach across comments.
fn spaced() {}
#[custom::test_case]
fn other() {}
"#,
        &[
            ("standard", true),
            ("asynchronous", true),
            ("custom", true),
            ("helper", true),
            ("spaced", true),
            ("other", false),
        ],
    );
}

#[test]
fn test_extract_cfg_predicates_only_mark_test_only_code() {
    for (predicate, expected) in [
        ("test", true),
        ("all(test, feature = \"extra\")", true),
        ("all(feature = \"extra\", any(test, all(test, unix)))", true),
        ("any(test, feature = \"extra\")", false),
        ("feature = \"test\"", false),
        ("not(test)", false),
        ("not(not(test))", true),
        ("not(all(test, unix))", false),
        ("not(any(not(test), unix))", true),
        ("all()", false),
        ("any()", false),
        ("not(all())", false),
        ("all(test, any())", false),
        ("all(unix, target_os = \"linux\")", false),
        ("any(test, all())", false),
        ("all(/* comment */ test,)", true),
    ] {
        let source = format!(
            "#[cfg({predicate})] mod scope {{ fn helper() {{}} }}
             struct S;
             #[cfg({predicate})] impl S {{ fn method(&self) {{}} }}
             #[cfg({predicate})] fn direct() {{ mod nested {{ fn local() {{}} }} }}
             fn sibling() {{}}"
        );
        assert_flags(
            "src/lib.rs",
            &source,
            &[
                ("helper", expected),
                ("method", expected),
                ("direct", expected),
                ("local", expected),
                ("sibling", false),
            ],
        );
    }
}

#[test]
fn test_extract_inner_cfg_applies_to_entire_scope() {
    assert_flags(
        "src/lib.rs",
        r#"
mod scope {
    #![cfg(test)]
    fn first() {}
    fn second() {}
}
fn outside() {}
"#,
        &[("first", true), ("second", true), ("outside", false)],
    );
    assert_flags(
        "src/lib.rs",
        "#![cfg(test)]\nfn first() {} fn second() {}",
        &[("first", true), ("second", true)],
    );
}

#[test]
fn test_extract_test_file_preserves_test_flag() {
    assert_flags(
        "tests/integration.rs",
        "fn helper() {}",
        &[("helper", true)],
    );
}

#[test]
fn test_extract_test_scope_marks_emitted_closure() {
    assert_flags(
        "src/lib.rs",
        "#[cfg(test)] mod scope { fn run() { consume(|| target()); } }",
        &[("run", true), ("<anonymous:1:44>", true)],
    );
}

#[test]
fn test_extract_async_closure_preserves_async_and_test_flags() {
    for (attribute, expected) in [("#[cfg(test)]", true), ("", false)] {
        let source = format!("{attribute} fn run() {{ consume(async || target().await); }}");
        let graph = assert_flags("src/lib.rs", &source, &[("run", expected)]);
        let index = graph
            .nodes
            .iter()
            .position(|node| {
                node.kind == NodeKind::Function
                    && node
                        .name
                        .resolve(&graph.string_pool)
                        .starts_with("<anonymous:")
            })
            .expect("missing anonymous closure");
        let meta = graph
            .function_meta(index as u32)
            .expect("missing closure metadata");
        assert_eq!(meta.is_test(), expected);
        assert!(meta.is_async());
    }
}

fn closure_name(source: &str, needle: &str) -> String {
    let offset = source.find(needle).expect("closure marker");
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line = source[..offset].matches('\n').count() + 1;
    format!("<anonymous:{line}:{}>", offset - line_start)
}

#[test]
fn test_extract_cfg_match_arm_marks_only_that_arm_closure() {
    let source = r#"
fn run(x: bool) {
    match x {
        #[cfg(test)]
        true => consume(|| target()),
        _ => consume(|| other()),
    }
}
"#;
    assert_flags(
        "src/lib.rs",
        source,
        &[
            ("run", false),
            (&closure_name(source, "|| target"), true),
            (&closure_name(source, "|| other"), false),
        ],
    );
}

#[test]
fn test_extract_cfg_field_initializer_marks_only_that_field_closure() {
    let source = r#"
fn run() {
    let _ = S {
        #[cfg(test)]
        x: consume(|| target()),
        y: consume(|| other()),
    };
}
"#;
    assert_flags(
        "src/lib.rs",
        source,
        &[
            ("run", false),
            (&closure_name(source, "|| target"), true),
            (&closure_name(source, "|| other"), false),
        ],
    );
}

#[test]
fn test_extract_inner_cfg_in_own_body_marks_function() {
    assert_flags(
        "src/lib.rs",
        r#"
struct S;
fn helper() {
    #![cfg(test)]
    target();
}
impl S {
    fn method(&self) {
        #![cfg(test)]
    }
}
fn plain() {
    target();
}
"#,
        &[("helper", true), ("method", true), ("plain", false)],
    );
}
