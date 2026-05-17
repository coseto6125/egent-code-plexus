use graph_nexus_analyzer::java::parser::JavaProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = JavaProvider::new().expect("provider");
    p.parse_file(Path::new("Test.java"), src.as_bytes())
        .expect("parse")
}

fn find_kind(graph: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| n.name == name && n.kind == kind)
}

#[test]
fn java_annotation_bare() {
    let graph = parse("@interface MyAnn {}");
    assert!(
        find_kind(&graph, "MyAnn", NodeKind::Annotation),
        "MyAnn must be Annotation; got: {:?}",
        graph.nodes.iter().map(|n| (&n.name, &n.kind)).collect::<Vec<_>>()
    );
}

#[test]
fn java_annotation_with_retention_and_method() {
    let src = r#"
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

@Retention(RetentionPolicy.RUNTIME)
@interface MyAnn {
    String value() default "";
}
"#;
    let graph = parse(src);
    assert!(
        find_kind(&graph, "MyAnn", NodeKind::Annotation),
        "MyAnn must be Annotation"
    );
    assert!(
        find_kind(&graph, "value", NodeKind::Method),
        "annotation element `value` must be emitted as Method"
    );
}

#[test]
fn java_annotation_public() {
    let graph = parse("public @interface MyAnn { int v(); }");
    assert!(
        find_kind(&graph, "MyAnn", NodeKind::Annotation),
        "public @interface MyAnn must be Annotation"
    );
}
