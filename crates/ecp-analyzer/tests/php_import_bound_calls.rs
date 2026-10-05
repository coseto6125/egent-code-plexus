mod instantiation_calls_support;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use instantiation_calls_support::{assert_calls_with_confidence, graph_of};

fn assert_import_alias_preserves_inherited_caller() {
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph = graph_of(&provider, &[
        ("Base.php", "<?php class Base { public function helper() {} }"),
        ("Contract.php", "<?php namespace Contracts; interface App { public function helper(); }"),
        ("App.php", "<?php use Contracts\\App as Contract; class App extends Base implements Contract { function go() { $this->helper(); } }"),
    ]);
    let hits =
        instantiation_calls_support::edges_from(&graph, "go", ecp_core::graph::RelType::Calls);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].file, "Base.php");
}

fn assert_import_external_group_never_suppresses() {
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "Helper.php",
                "<?php class Helper { static function helper() {} }",
            ),
            (
                "App.php",
                "<?php use External\\{Helper}; function go() { Helper::helper(); }",
            ),
        ],
    );
    let hits =
        instantiation_calls_support::edges_from(&graph, "go", ecp_core::graph::RelType::Calls);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].file, "Helper.php");
}

#[test]
fn test_import_class_binding() {
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph=graph_of(&provider, &[("src/pkg/Helper.php", "<?php namespace pkg; function helper() {} class Helper { public static function work() {} }"),("app.php", "<?php use pkg\\Helper; function go() { Helper::work(); }")]);
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/pkg/Helper.php",
            "work",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_member_binding() {
    assert_import_missing_member_binding();
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph=graph_of(&provider, &[("src/pkg/Helper.php", "<?php namespace pkg; function helper() {} class Helper { public static function work() {} }"),("app.php", "<?php use function pkg\\helper; function go() { helper(); }")]);
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/pkg/Helper.php",
            "helper",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_alias_binding() {
    assert_import_alias_preserves_inherited_caller();
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph=graph_of(&provider, &[("src/pkg/Helper.php", "<?php namespace pkg; function helper() {} class Helper { public static function work() {} }"),("app.php", "<?php use function pkg\\helper as h; function go() { h(); }")]);
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/pkg/Helper.php",
            "helper",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_wildcard_binding() {
    assert_import_external_group_never_suppresses();
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph=graph_of(&provider, &[("src/pkg/Helper.php", "<?php namespace pkg; function helper() {} class Helper { public static function work() {} }"),("app.php", "<?php use pkg\\{Helper}; function go() { Helper::work(); }")]);
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/pkg/Helper.php",
            "work",
            ResolutionTier::ImportScoped.base_confidence(),
        )],
    );
}

#[test]
fn test_import_external_binding() {
    assert_import_external_root_binding();
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph=graph_of(&provider, &[("src/pkg/Helper.php", "<?php namespace pkg; function helper() {} class Helper { public static function work() {} }"),("app.php", "<?php use function external\\helper; function go() { helper(); }")]);
    assert_calls_with_confidence(&graph, "go", &[]);
}

fn assert_import_external_root_binding() {
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph=graph_of(&provider, &[("src/pkg/Helper.php", "<?php namespace pkg; function helper() {} class Helper { public static function work() {} }"),("app.php", "<?php use function external\\helper; function go() { helper(); }"),("external/marker.php", "")]);
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/pkg/Helper.php",
            "helper",
            ResolutionTier::Global.base_confidence(),
        )],
    );
}

fn assert_import_missing_member_binding() {
    let provider = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let graph=graph_of(&provider, &[("src/pkg/Helper.php", "<?php namespace pkg; function helper() {} class Helper { public static function work() {} }"),("app.php", "<?php use function pkg\\missing\\helper; function go() { helper(); }"),("src/pkg/missing/Empty.php", "<?php namespace pkg\\missing; function other() {}")]);
    assert_calls_with_confidence(
        &graph,
        "go",
        &[(
            "src/pkg/Helper.php",
            "helper",
            ResolutionTier::Global.base_confidence(),
        )],
    );
}
