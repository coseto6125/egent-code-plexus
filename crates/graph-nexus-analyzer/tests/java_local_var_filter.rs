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

fn has_variable(graph: &LocalGraph, name: &str) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| n.name == name && n.kind == NodeKind::Variable)
}

/// Local variables inside method bodies must NOT be emitted as Variable nodes.
#[test]
fn java_local_var_in_method_not_emitted() {
    let src = r#"
public class Example {
    public void compute() {
        int localA = 42;
        String localB = "hello";
        for (int i = 0; i < 10; i++) {
            int inner = i * 2;
        }
    }
}
"#;
    let graph = parse(src);
    assert!(
        !has_variable(&graph, "localA"),
        "localA (method-local) must not appear as Variable; nodes: {:?}",
        graph
            .nodes
            .iter()
            .map(|n| (&n.name, &n.kind))
            .collect::<Vec<_>>()
    );
    assert!(
        !has_variable(&graph, "localB"),
        "localB (method-local) must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "inner"),
        "inner (for-loop local) must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "i"),
        "loop counter i must not appear as Variable"
    );
}

/// Method parameters must NOT be emitted as Variable nodes.
#[test]
fn java_method_params_not_emitted() {
    let src = r#"
public class Example {
    public int add(int x, int y) {
        return x + y;
    }

    public void process(String input, boolean flag) {}
}
"#;
    let graph = parse(src);
    assert!(
        !has_variable(&graph, "x"),
        "parameter x must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "y"),
        "parameter y must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "input"),
        "parameter input must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "flag"),
        "parameter flag must not appear as Variable"
    );
}

/// Constructor locals and parameters must NOT be emitted as Variable nodes.
#[test]
fn java_constructor_locals_not_emitted() {
    let src = r#"
public class Example {
    public Example(int size) {
        int capacity = size * 2;
    }
}
"#;
    let graph = parse(src);
    assert!(
        !has_variable(&graph, "size"),
        "constructor param size must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "capacity"),
        "constructor-local capacity must not appear as Variable"
    );
}

/// Catch-clause variables must NOT be emitted as Variable nodes.
#[test]
fn java_catch_var_not_emitted() {
    let src = r#"
public class Example {
    public void run() {
        try {
            int result = 0;
        } catch (Exception ex) {
            int errCode = -1;
        }
    }
}
"#;
    let graph = parse(src);
    assert!(
        !has_variable(&graph, "result"),
        "try-block local result must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "errCode"),
        "catch-block local errCode must not appear as Variable"
    );
}

/// Class fields are emitted as Property — this must remain unchanged.
/// No Variable nodes should appear for instance fields.
#[test]
fn java_class_fields_emitted_as_property_not_variable() {
    let src = r#"
public class Example {
    private int count;
    public String label;

    public void doWork() {
        int tmp = 0;
    }
}
"#;
    let graph = parse(src);

    // Fields → Property (existing behavior, must not regress)
    assert!(
        graph
            .nodes
            .iter()
            .any(|n| n.name == "count" && n.kind == NodeKind::Property),
        "count must be Property"
    );
    assert!(
        graph
            .nodes
            .iter()
            .any(|n| n.name == "label" && n.kind == NodeKind::Property),
        "label must be Property"
    );

    // No Variable nodes at all in this snippet
    let var_nodes: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variable)
        .collect();
    assert!(
        var_nodes.is_empty(),
        "no Variable nodes should be emitted; got: {:?}",
        var_nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
}

/// Lambda parameters must NOT be emitted as Variable nodes.
#[test]
fn java_lambda_params_not_emitted() {
    let src = r#"
import java.util.List;
import java.util.function.Function;

public class Example {
    public void run() {
        List<String> items = List.of("a");
        items.forEach(item -> {
            int len = item.length();
        });
        Function<Integer, Integer> fn = val -> val * 2;
    }
}
"#;
    let graph = parse(src);
    assert!(
        !has_variable(&graph, "item"),
        "lambda param item must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "len"),
        "lambda-body local len must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "val"),
        "lambda param val must not appear as Variable"
    );
    // The outer local `items` and `fn` are also method-locals — also suppressed
    assert!(
        !has_variable(&graph, "items"),
        "method-local items must not appear as Variable"
    );
    assert!(
        !has_variable(&graph, "fn"),
        "method-local fn must not appear as Variable"
    );
}
