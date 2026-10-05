mod instantiation_calls_support;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use instantiation_calls_support::{assert_calls_with_confidence, graph_of};

#[test]
fn test_import_class_binding() {
    let provider = ecp_analyzer::java::parser::JavaProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/java/pkg/Helper.java",
                "package pkg; public class Helper { public static void helper() {} }",
            ),
            (
                "App.java",
                "import pkg.Helper; class App { void go() { Helper.helper(); } }",
            ),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/java/pkg/Helper.java",
            "helper",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_static_binding() {
    assert_import_missing_member_binding();
    let provider = ecp_analyzer::java::parser::JavaProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/java/pkg/Helper.java",
                "package pkg; public class Helper { public static void helper() {} }",
            ),
            (
                "App.java",
                "import static pkg.Helper.helper; class App { void go() { helper(); } }",
            ),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/java/pkg/Helper.java",
            "helper",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_wildcard_binding() {
    let provider = ecp_analyzer::java::parser::JavaProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/java/pkg/Helper.java",
                "package pkg; public class Helper { public static void helper() {} }",
            ),
            (
                "App.java",
                "import static pkg.Helper.*; class App { void go() { helper(); } }",
            ),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/java/pkg/Helper.java",
            "helper",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_external_binding() {
    assert_import_external_root_binding();
    let provider = ecp_analyzer::java::parser::JavaProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/java/pkg/Helper.java",
                "package pkg; public class Helper { public static void helper() {} }",
            ),
            (
                "App.java",
                "import static external.Other.helper; class App { void go() { helper(); } }",
            ),
        ],
    );
    assert_calls_with_confidence(&graph, "go", &[]);
}

fn assert_import_external_root_binding() {
    let provider = ecp_analyzer::java::parser::JavaProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/java/pkg/Helper.java",
                "package pkg; public class Helper { public static void helper() {} }",
            ),
            (
                "App.java",
                "import static external.Other.helper; class App { void go() { helper(); } }",
            ),
            ("external/marker.java", ""),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/java/pkg/Helper.java",
            "helper",
            ResolutionTier::Global.base_confidence(),
        )],
    );
}

fn assert_import_missing_member_binding() {
    let provider = ecp_analyzer::java::parser::JavaProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/java/pkg/Helper.java",
                "package pkg; public class Helper { public static void helper() {} }",
            ),
            (
                "App.java",
                "import static pkg.Empty.helper; class App { void go() { helper(); } }",
            ),
            (
                "src/main/java/pkg/Empty.java",
                "package pkg; class Empty {}",
            ),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/java/pkg/Helper.java",
            "helper",
            ResolutionTier::Global.base_confidence(),
        )],
    );
}
