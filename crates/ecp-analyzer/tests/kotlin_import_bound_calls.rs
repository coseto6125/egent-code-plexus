mod instantiation_calls_support;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use instantiation_calls_support::{assert_calls_with_confidence, graph_of};

fn assert_import_missing_aliased_member_keeps_alias_fallback() {
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            ("pkg/Util.kt", "package pkg\nfun other() {}"),
            ("Other.kt", "fun helper() {}"),
            ("App.kt", "import pkg.helper as h\nfun go() { h() }"),
        ],
    );
    assert_calls_with_confidence(&graph, "go", &[]);
}

#[test]
fn test_import_class_binding() {
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let shadowed = graph_of(
        &provider,
        &[
            (
                "pkg/Helper.kt",
                "package pkg\nclass Helper {\n companion object {\n fun work() {}\n }\n}",
            ),
            ("Other.kt", "class Other {\n fun work() {}\n}"),
            (
                "App.kt",
                "import pkg.Helper\nfun go() {\n val Helper = factory()\n Helper.work()\n}",
            ),
        ],
    );
    assert_calls_with_confidence(&shadowed, "go", &[]);
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/kotlin/pkg/Util.kt",
                "package pkg\nfun helper() {}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
            ),
            ("App.kt", "import pkg.Helper\nfun go() { Helper.work() }"),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/kotlin/pkg/Util.kt",
            "work",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_member_binding() {
    assert_import_missing_member_binding();
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/kotlin/pkg/Util.kt",
                "package pkg\nfun helper() {}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
            ),
            ("App.kt", "import pkg.helper\nfun go() { helper() }"),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/kotlin/pkg/Util.kt",
            "helper",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_alias_binding() {
    assert_import_missing_aliased_member_keeps_alias_fallback();
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/kotlin/pkg/Util.kt",
                "package pkg\nfun helper() {}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
            ),
            ("App.kt", "import pkg.helper as h\nfun go() { h() }"),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/kotlin/pkg/Util.kt",
            "helper",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_wildcard_binding() {
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/kotlin/pkg/Util.kt",
                "package pkg\nfun helper() {}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
            ),
            ("App.kt", "import pkg.*\nfun go() { helper() }"),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/kotlin/pkg/Util.kt",
            "helper",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_external_binding() {
    assert_import_external_root_binding();
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/kotlin/pkg/Util.kt",
                "package pkg\nfun helper() {}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
            ),
            ("App.kt", "import external.helper\nfun go() { helper() }"),
        ],
    );
    assert_calls_with_confidence(&graph, "go", &[]);
}

fn assert_import_external_root_binding() {
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/kotlin/pkg/Util.kt",
                "package pkg\nfun helper() {}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
            ),
            ("App.kt", "import external.helper\nfun go() { helper() }"),
            ("external/marker.kt", ""),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/kotlin/pkg/Util.kt",
            "helper",
            ResolutionTier::Global.base_confidence(),
        )],
    );
}

fn assert_import_missing_member_binding() {
    let provider = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "src/main/kotlin/pkg/Util.kt",
                "package pkg\nfun helper() {}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
            ),
            ("App.kt", "import pkg.missing.helper\nfun go() { helper() }"),
        ],
    );
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/main/kotlin/pkg/Util.kt",
            "helper",
            ResolutionTier::Global.base_confidence(),
        )],
    );
}
