//! A Kotlin import that names a repo file binds the callee to that file's
//! member (ImportScoped); one that names nothing local leaves no edge; any
//! other import keeps the resolution path the call had before the import tier.
mod instantiation_calls_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use instantiation_calls_support::{assert_call_targets, graph_of};

const UTIL: (&str, &str) = (
    "src/main/kotlin/pkg/Util.kt",
    "package pkg\nfun helper() {}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
);

fn kotlin_graph(files: &[(&str, &str)]) -> ecp_core::graph::ZeroCopyGraph {
    graph_of(&KotlinProvider::new().unwrap(), files)
}

fn import_scoped() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

fn heritage() -> f32 {
    ResolutionTier::HeritageScoped.base_confidence()
}

fn global() -> f32 {
    ResolutionTier::Global.base_confidence()
}

#[test]
fn test_resolve_call_class_import_returns_import_scoped() {
    let graph = kotlin_graph(&[
        UTIL,
        ("App.kt", "import pkg.Helper\nfun go() { Helper.work() }"),
    ]);
    assert_call_targets(&graph, "go", &[(UTIL.0, "Helper", "work", import_scoped())]);
}

#[test]
fn test_resolve_call_local_shadows_class_import_returns_no_edge() {
    let graph = kotlin_graph(&[
        (
            "pkg/Helper.kt",
            "package pkg\nclass Helper {\n companion object {\n fun work() {}\n }\n}",
        ),
        ("Other.kt", "class Other {\n fun work() {}\n}"),
        (
            "App.kt",
            "import pkg.Helper\nfun go() {\n val Helper = factory()\n Helper.work()\n}",
        ),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

/// Several classes in the imported file: the member must belong to the
/// imported class, not to the first class that declares the name.
#[test]
fn test_resolve_call_class_import_member_of_other_class_binds_imported_class() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/pkg/Util.kt",
            "package pkg\nclass Other {\n fun work() {}\n}\nclass Helper {\n companion object {\n fun work() {}\n }\n}\n",
        ),
        ("App.kt", "import pkg.Helper\nfun go() { Helper.work() }"),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[(
            "src/main/kotlin/pkg/Util.kt",
            "Helper",
            "work",
            import_scoped(),
        )],
    );
}

#[test]
fn test_resolve_call_member_import_returns_import_scoped() {
    let graph = kotlin_graph(&[UTIL, ("App.kt", "import pkg.helper\nfun go() { helper() }")]);
    assert_call_targets(&graph, "go", &[(UTIL.0, "", "helper", import_scoped())]);
}

#[test]
fn test_resolve_call_member_import_missing_member_keeps_global() {
    let graph = kotlin_graph(&[
        UTIL,
        ("App.kt", "import pkg.missing.helper\nfun go() { helper() }"),
        (
            "pkg/missing/Empty.kt",
            "package pkg.missing\nfun other() {}",
        ),
    ]);
    assert_call_targets(&graph, "go", &[(UTIL.0, "", "helper", global())]);
}

#[test]
fn test_resolve_call_alias_import_returns_import_scoped() {
    let graph = kotlin_graph(&[UTIL, ("App.kt", "import pkg.helper as h\nfun go() { h() }")]);
    assert_call_targets(&graph, "go", &[(UTIL.0, "", "helper", import_scoped())]);
}

#[test]
fn test_resolve_call_alias_import_missing_member_returns_no_edge() {
    let graph = kotlin_graph(&[
        ("pkg/Util.kt", "package pkg\nfun other() {}"),
        ("Other.kt", "fun helper() {}"),
        ("App.kt", "import pkg.helper as h\nfun go() { h() }"),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

#[test]
fn test_resolve_call_wildcard_import_returns_import_scoped() {
    let graph = kotlin_graph(&[UTIL, ("App.kt", "import pkg.*\nfun go() { helper() }")]);
    assert_call_targets(&graph, "go", &[(UTIL.0, "", "helper", import_scoped())]);
}

/// Wildcards never suppress: the external star import leaves the global tier.
#[test]
fn test_resolve_call_external_wildcard_import_keeps_global() {
    let graph = kotlin_graph(&[UTIL, ("App.kt", "import external.*\nfun go() { helper() }")]);
    assert_call_targets(&graph, "go", &[(UTIL.0, "", "helper", global())]);
}

/// A package wildcard imports top-level declarations, never the member of a
/// class in that package.
#[test]
fn test_resolve_call_wildcard_import_class_member_keeps_global() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/util/Adapter.kt",
            "package util\nclass Adapter {\n fun refresh() {}\n}\n",
        ),
        ("App.kt", "import util.*\nfun go() { refresh() }"),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[(
            "src/main/kotlin/util/Adapter.kt",
            "Adapter",
            "refresh",
            global(),
        )],
    );
}

/// A member the caller's class inherits beats a star-imported function.
#[test]
fn test_resolve_call_wildcard_import_and_inherited_member_returns_heritage_scoped() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/lib/Base.kt",
            "package lib\nopen class Base {\n fun refresh() {}\n}\n",
        ),
        (
            "src/main/kotlin/util/Funcs.kt",
            "package util\nfun refresh() {}\n",
        ),
        (
            "App.kt",
            "package app\nimport lib.Base\nimport util.*\nclass Screen : Base() {\n fun go() { refresh() }\n}\n",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[("src/main/kotlin/lib/Base.kt", "Base", "refresh", heritage())],
    );
}

/// Two explicit imports bind different members: Kotlin picks by signature,
/// so the general tiers decide (here: two `helper`s, no edge).
#[test]
fn test_resolve_call_two_member_imports_bind_different_members_returns_no_edge() {
    let graph = kotlin_graph(&[
        ("src/main/kotlin/a/A.kt", "package a\nfun helper() {}\n"),
        (
            "src/main/kotlin/b/B.kt",
            "package b\nfun helper(x: Int) {}\n",
        ),
        (
            "App.kt",
            "import a.helper\nimport b.helper\nfun go() { helper() }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

#[test]
fn test_resolve_call_external_import_returns_no_edge() {
    let graph = kotlin_graph(&[
        UTIL,
        ("App.kt", "import external.helper\nfun go() { helper() }"),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

#[test]
fn test_resolve_call_external_import_with_indexed_root_keeps_global() {
    let graph = kotlin_graph(&[
        UTIL,
        ("App.kt", "import external.helper\nfun go() { helper() }"),
        ("external/marker.kt", ""),
    ]);
    assert_call_targets(&graph, "go", &[(UTIL.0, "", "helper", global())]);
}

/// A default-package import (`import helper`) names no package: it may be a
/// repo function in a file of another name, so it never suppresses.
#[test]
fn test_resolve_call_single_segment_import_keeps_global() {
    let graph = kotlin_graph(&[
        ("helpers.kt", "fun helper() {}\n"),
        (
            "src/app/App.kt",
            "package app\nimport helper\nfun go() { helper() }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[("helpers.kt", "", "helper", global())]);
}

/// `ProtoBuf` is an external import, yet `ProtoBuf.asConverterFactory()`
/// calls an extension function this repo declares on that type: the
/// external receiver must not suppress the call.
#[test]
fn test_resolve_call_extension_on_external_class_keeps_global() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/conv/Factory.kt",
            "package conv\nimport kotlinx.serialization.BinaryFormat\nfun BinaryFormat.asConverterFactory(t: String): Any = t\n",
        ),
        (
            "src/test/kotlin/conv/FactoryTest.kt",
            "package conv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun setUp() { ProtoBuf.asConverterFactory(\"x\") }\n",
        ),
    ]);
    assert_call_targets(
        &graph,
        "setUp",
        &[(
            "src/main/kotlin/conv/Factory.kt",
            "",
            "asConverterFactory",
            global(),
        )],
    );
}

/// The external import's first segment names an in-repo directory, so it
/// is not proven external; the qualified call still falls back to the bare
/// extension function once every qualified tier misses.
#[test]
fn test_resolve_call_extension_on_unindexed_class_under_indexed_root_keeps_global() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/kotlinx/conv/Factory.kt",
            "package kotlinx.conv\nimport kotlinx.serialization.BinaryFormat\nfun BinaryFormat.asConverterFactory(t: String): Any = t\n",
        ),
        (
            "src/test/kotlin/conv/FactoryTest.kt",
            "package conv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun setUp() { ProtoBuf.asConverterFactory(\"x\") }\n",
        ),
    ]);
    assert_call_targets(
        &graph,
        "setUp",
        &[(
            "src/main/kotlin/kotlinx/conv/Factory.kt",
            "",
            "asConverterFactory",
            global(),
        )],
    );
}

/// `gson: Gson` makes the callee `Gson.fromJson`; the library type's member
/// is not the same-named method of the caller's own class.
#[test]
fn test_resolve_call_typed_receiver_of_library_type_skips_member_namesake_returns_no_edge() {
    let graph = kotlin_graph(&[(
        "src/main/kotlin/com/example/UserAdapter.kt",
        "package com.example\nimport com.google.gson.Gson\nclass UserAdapter {\n fun fromJson(json: String): Any = json\n fun parse(json: String, gson: Gson): Any = gson.fromJson(json, Any::class.java)\n}\n",
    )]);
    assert_call_targets(&graph, "parse", &[]);
}

/// The extension retry keeps the file's imports, so an aliased member import
/// still names the extension function.
#[test]
fn test_resolve_call_extension_through_aliased_member_import_returns_import_scoped() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/conv/Convert.kt",
            "package conv\nimport kotlinx.serialization.BinaryFormat\nfun BinaryFormat.convert(): Any = this\n",
        ),
        (
            "src/main/kotlin/other/Other.kt",
            "package other\nfun convert(): Any = 1\n",
        ),
        (
            "src/test/kotlin/app/App.kt",
            "package app\nimport conv.convert as cv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun setUp() { ProtoBuf.cv() }\n",
        ),
    ]);
    assert_call_targets(
        &graph,
        "setUp",
        &[(
            "src/main/kotlin/conv/Convert.kt",
            "",
            "convert",
            import_scoped(),
        )],
    );
}

/// `super.log()` names the external base; a top-level `log()` in the file
/// is not an extension on that base call.
#[test]
fn test_resolve_call_super_of_external_base_skips_top_level_namesake_returns_no_edge() {
    let graph = kotlin_graph(&[(
        "src/main/kotlin/com/example/Screen.kt",
        "package com.example\nimport android.app.Activity\nfun log() {}\nclass Screen : Activity() {\n override fun onResume() { super.log() }\n}\n",
    )]);
    assert_call_targets(&graph, "onResume", &[]);
}

/// `super.setup()` names the imported base, which does not declare
/// `setup`: the call keeps the receiver ladder, which finds the inherited
/// member, instead of retrying the bare name (a self-call).
#[test]
fn test_resolve_call_super_member_outside_imported_file_returns_type_heritage() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/base/Root.kt",
            "package base\nopen class Root {\n open fun setup() {}\n}\n",
        ),
        (
            "src/main/kotlin/base/BaseScreen.kt",
            "package base\nopen class BaseScreen : Root()\n",
        ),
        (
            "src/main/kotlin/app/Screen.kt",
            "package app\nimport base.BaseScreen\nclass Screen : BaseScreen() {\n override fun setup() { super.setup() }\n}\n",
        ),
    ]);
    assert_call_targets(
        &graph,
        "setup",
        &[("src/main/kotlin/base/Root.kt", "Root", "setup", heritage())],
    );
}

/// The imported base neither declares `setup` nor reaches it through a
/// project type (its own base is external): no edge, never a self-call.
#[test]
fn test_resolve_call_super_member_of_external_ancestor_returns_no_edge() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/base/BaseScreen.kt",
            "package base\nimport android.app.Activity\nopen class BaseScreen : Activity()\n",
        ),
        (
            "src/main/kotlin/app/Screen.kt",
            "package app\nimport base.BaseScreen\nclass Screen : BaseScreen() {\n override fun onResume() { super.onResume() }\n}\n",
        ),
    ]);
    assert_call_targets(&graph, "onResume", &[]);
}

/// A typed parameter of an imported class: the ladder finds the inherited
/// member although another class declares the same name.
#[test]
fn test_resolve_call_typed_receiver_of_imported_class_returns_type_heritage() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/lib/Base.kt",
            "package lib\nopen class Base {\n fun greet() {}\n}\n",
        ),
        (
            "src/main/kotlin/lib/Derived.kt",
            "package lib\nclass Derived : Base()\n",
        ),
        (
            "src/main/kotlin/other/Decoy.kt",
            "package other\nclass Decoy {\n fun greet() {}\n}\n",
        ),
        (
            "src/main/kotlin/app/App.kt",
            "package app\nimport lib.Derived\nfun go(d: Derived) { d.greet() }\n",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[("src/main/kotlin/lib/Base.kt", "Base", "greet", heritage())],
    );
}

/// A star-imported package named like a constructor property: the call on
/// the property resolves by its member name, as before the import tier.
#[test]
fn test_resolve_call_wildcard_package_named_receiver_keeps_global() {
    let graph = kotlin_graph(&[
        (
            "src/main/kotlin/com/app/repository/UserRepository.kt",
            "package com.app.repository\nclass UserRepository {\n fun fetchUsers() {}\n}\n",
        ),
        (
            "src/main/kotlin/com/app/ui/VM.kt",
            "package com.app.ui\nimport com.app.repository.*\nclass VM(private val repository: UserRepository) {\n fun load() { repository.fetchUsers() }\n}\n",
        ),
    ]);
    assert_call_targets(
        &graph,
        "load",
        &[(
            "src/main/kotlin/com/app/repository/UserRepository.kt",
            "UserRepository",
            "fetchUsers",
            global(),
        )],
    );
}
