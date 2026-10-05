//! A PHP `use` that names a repo file binds the callee to that file's member
//! (ImportScoped); one that names nothing local leaves no edge; any other
//! import keeps the resolution path the call had before the import tier.
//! The `Imports` edges of grouped and aliased `use` statements follow the
//! local name, as they did before the import tier.
mod instantiation_calls_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_core::graph::RelType;
use instantiation_calls_support::{assert_call_targets, edges_from, graph_of};

const HELPER: (&str, &str) = (
    "src/pkg/Helper.php",
    "<?php namespace pkg; function helper() {} class Helper { public static function work() {} }",
);

fn php_graph(files: &[(&str, &str)]) -> ecp_core::graph::ZeroCopyGraph {
    graph_of(&PhpProvider::new().unwrap(), files)
}

fn import_scoped() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

fn qualifier_scoped() -> f32 {
    ResolutionTier::QualifierScoped.base_confidence()
}

fn global() -> f32 {
    ResolutionTier::Global.base_confidence()
}

#[test]
fn test_resolve_call_class_use_returns_import_scoped() {
    let graph = php_graph(&[
        HELPER,
        (
            "app.php",
            "<?php use pkg\\Helper; function go() { Helper::work(); }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[(HELPER.0, "Helper", "work", import_scoped())],
    );
}

#[test]
fn test_resolve_call_use_function_returns_import_scoped() {
    let graph = php_graph(&[
        HELPER,
        (
            "app.php",
            "<?php use function pkg\\helper; function go() { helper(); }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[(HELPER.0, "", "helper", import_scoped())]);
}

#[test]
fn test_resolve_call_use_function_missing_member_keeps_global() {
    let graph = php_graph(&[
        HELPER,
        (
            "app.php",
            "<?php use function pkg\\missing\\helper; function go() { helper(); }",
        ),
        (
            "src/pkg/missing/Empty.php",
            "<?php namespace pkg\\missing; function other() {}",
        ),
    ]);
    assert_call_targets(&graph, "go", &[(HELPER.0, "", "helper", global())]);
}

#[test]
fn test_resolve_call_aliased_use_function_returns_import_scoped() {
    let graph = php_graph(&[
        HELPER,
        (
            "app.php",
            "<?php use function pkg\\helper as h; function go() { h(); }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[(HELPER.0, "", "helper", import_scoped())]);
}

#[test]
fn test_resolve_call_aliased_use_keeps_inherited_this_call() {
    let graph = php_graph(&[
        ("Base.php", "<?php class Base { public function helper() {} }"),
        (
            "Contract.php",
            "<?php namespace Contracts; interface App { public function helper(); }",
        ),
        (
            "App.php",
            "<?php use Contracts\\App as Contract; class App extends Base implements Contract { function go() { $this->helper(); } }",
        ),
    ]);
    let hits = instantiation_calls_support::calls_from(&graph, "go");
    assert!(hits.len() == 1 && hits[0].file == "Base.php", "{hits:?}");
}

#[test]
fn test_resolve_call_group_use_returns_import_scoped() {
    let graph = php_graph(&[
        HELPER,
        (
            "app.php",
            "<?php use pkg\\{Helper}; function go() { Helper::work(); }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[(HELPER.0, "Helper", "work", import_scoped())],
    );
}

/// A group import never suppresses, even when its namespace is external.
#[test]
fn test_resolve_call_external_group_use_keeps_qualifier_scope() {
    let graph = php_graph(&[
        (
            "Helper.php",
            "<?php class Helper { static function helper() {} }",
        ),
        (
            "App.php",
            "<?php use External\\{Helper}; function go() { Helper::helper(); }",
        ),
    ]);
    let hits = instantiation_calls_support::calls_from(&graph, "go");
    assert!(hits.len() == 1 && hits[0].file == "Helper.php", "{hits:?}");
}

#[test]
fn test_resolve_call_external_use_function_returns_no_edge() {
    let graph = php_graph(&[
        HELPER,
        (
            "app.php",
            "<?php use function external\\helper; function go() { helper(); }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

#[test]
fn test_resolve_call_external_use_with_indexed_root_keeps_global() {
    let graph = php_graph(&[
        HELPER,
        (
            "app.php",
            "<?php use function external\\helper; function go() { helper(); }",
        ),
        ("external/marker.php", ""),
    ]);
    assert_call_targets(&graph, "go", &[(HELPER.0, "", "helper", global())]);
}

/// `use Helper;` names no namespace: the class lives in a namespace-less
/// file of another name, so the import never suppresses.
#[test]
fn test_resolve_call_single_segment_use_keeps_qualifier_scope() {
    let graph = php_graph(&[
        (
            "legacy.php",
            "<?php class Helper { static function run() {} }",
        ),
        (
            "app.php",
            "<?php namespace App; use Helper; function go() { Helper::run(); }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[("legacy.php", "Helper", "run", qualifier_scoped())],
    );
}

/// `use InvalidArgumentException;` names the builtin class: its constructor
/// is not the constructor of a same-named project class in another
/// namespace, nor of that class's parent.
#[test]
fn test_resolve_call_parent_ctor_of_builtin_import_returns_no_edge() {
    let graph = php_graph(&[
        (
            "src/Testing/InvalidArgumentException.php",
            "<?php namespace Lib\\Testing; use PHPUnit\\Framework\\Exception; class InvalidArgumentException extends Exception {}",
        ),
        (
            "src/Renderer/Exception.php",
            "<?php namespace Lib\\Renderer; class Exception { function __construct() {} }",
        ),
        (
            "src/Db/Missing.php",
            "<?php namespace Lib\\Db; use InvalidArgumentException; class Missing extends InvalidArgumentException { function __construct() { parent::__construct('x'); } }",
        ),
    ]);
    let calls: Vec<_> = edges_from(&graph, "__construct", RelType::Calls)
        .into_iter()
        .filter(|hit| hit.file == "src/Renderer/Exception.php")
        .collect();
    assert!(
        calls.is_empty(),
        "builtin parent ctor bound to a project class: {calls:?}"
    );
}

/// `use InvalidArgumentException;` is the global class, not the file of a
/// namespaced namesake: the construction keeps its pre-import-tier Global
/// edge instead of an ImportScoped one.
#[test]
fn test_resolve_call_builtin_use_skips_namespaced_namesake_keeps_global() {
    let graph = php_graph(&[
        (
            "src/Testing/InvalidArgumentException.php",
            "<?php namespace Lib\\Testing; class InvalidArgumentException {}",
        ),
        (
            "app.php",
            "<?php use InvalidArgumentException; function go() { new InvalidArgumentException(); }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[(
            "src/Testing/InvalidArgumentException.php",
            "",
            "InvalidArgumentException",
            global(),
        )],
    );
}

/// `use function helper;` names a global function in `helpers.php`.
#[test]
fn test_resolve_call_single_segment_use_function_keeps_global() {
    let graph = php_graph(&[
        ("helpers.php", "<?php function helper() {}"),
        (
            "app.php",
            "<?php namespace App; use function helper; function go() { helper(); }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[("helpers.php", "", "helper", global())]);
}

/// A fully qualified `new \App\Models\User()` is not the external `User`
/// that a `use` imports: the last-segment retry skips the `use` binding.
#[test]
fn test_resolve_call_fully_qualified_new_ignores_use_keeps_global() {
    let graph = php_graph(&[
        (
            "src/App/Models/User.php",
            "<?php namespace App\\Models; class User {}",
        ),
        (
            "app.php",
            "<?php use External\\Lib\\User; function go() { return new \\App\\Models\\User(); }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[("src/App/Models/User.php", "", "User", global())],
    );
}

/// `$date->format()` is a method call: `use function` never names it, so it
/// neither binds the namespaced function nor suppresses the call.
#[test]
fn test_resolve_call_untyped_member_ignores_use_function_returns_no_edge() {
    let graph = php_graph(&[
        (
            "src/App/Support/helpers.php",
            "<?php namespace App\\Support; function format() {}",
        ),
        (
            "src/Date.php",
            "<?php class Date { public function format() {} }",
        ),
        (
            "app.php",
            "<?php use function App\\Support\\format; function go($date) { $date->format(); }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

/// An external `use function select` does not suppress `$repo->select()`.
#[test]
fn test_resolve_call_untyped_member_ignores_external_use_function_keeps_global() {
    let graph = php_graph(&[
        (
            "src/Repo.php",
            "<?php class Repo { public function select() {} }",
        ),
        (
            "app.php",
            "<?php use function External\\select; function go($repo) { $repo->select(); }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[("src/Repo.php", "Repo", "select", global())],
    );
}

#[test]
fn test_emit_edges_group_use_targets_member() {
    let graph = php_graph(&[
        HELPER,
        ("app.php", "<?php use pkg\\{Helper}; function go() {}"),
    ]);
    let hits = edges_from(&graph, "app.php", RelType::Imports);
    assert!(
        hits.iter()
            .any(|h| h.name == "Helper" && h.file == HELPER.0),
        "{hits:?}"
    );
}

/// `use Foo\Logger as BaseLogger` binds `BaseLogger`: the file's own
/// `Logger` is not what it imports.
#[test]
fn test_emit_edges_aliased_use_skips_same_file_namesake() {
    let graph = php_graph(&[(
        "app.php",
        "<?php use Foo\\Logger as BaseLogger; class Logger extends BaseLogger {}",
    )]);
    let hits = edges_from(&graph, "app.php", RelType::Imports);
    assert!(!hits.iter().any(|h| h.file == "app.php"), "{hits:?}");
}
