//! Targets and confidence tiers are measured against edd45c5b.
mod instantiation_calls_support;

use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_core::{analyzer::provider::LanguageProvider, graph::RelType};
use instantiation_calls_support::graph_of;

fn assert_parity<P: LanguageProvider>(
    provider: P,
    files: &[(&str, &str)],
    name: &str,
    tier: ResolutionTier,
) {
    let graph = graph_of(&provider, files);
    let hits: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.rel_type == RelType::Calls
                && graph.nodes[edge.source as usize]
                    .name
                    .resolve(&graph.string_pool)
                    == if files[1].0 == "app.go" { "run" } else { "go" }
        })
        .map(|edge| {
            let node = &graph.nodes[edge.target as usize];
            (
                graph.files[node.file_idx as usize]
                    .path
                    .resolve(&graph.string_pool),
                node.name.resolve(&graph.string_pool),
                edge.confidence,
            )
        })
        .collect();
    println!("{}: {hits:?}", files[1].0);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].0, files[0].0);
    assert_eq!(hits[0].1, name);
    assert_eq!(hits[0].2, tier.base_confidence());
}

macro_rules! parity {
    ($test:ident, $provider:path, $lib:literal, $definition:literal, $app:literal, $source:literal, $name:literal, $tier:ident) => {
        #[test]
        fn $test() {
            assert_parity(
                <$provider>::new().unwrap(),
                &[($lib, $definition), ($app, $source)],
                $name,
                ResolutionTier::$tier,
            );
        }
    };
}

parity!(
    test_typescript_import_target_and_tier,
    ecp_analyzer::typescript::TypeScriptProvider,
    "lib.ts",
    "export function helper() {}",
    "app.ts",
    "import { helper } from './lib'; export function go() { helper(); }",
    "helper",
    ImportScoped
);
parity!(
    test_javascript_import_target_and_tier,
    ecp_analyzer::javascript::parser::JavaScriptProvider,
    "lib.js",
    "export function helper() {}",
    "app.js",
    "import { helper } from './lib'; export function go() { helper(); }",
    "helper",
    ImportScoped
);
parity!(
    test_python_import_target_and_tier,
    ecp_analyzer::python::parser::PythonProvider,
    "lib.py",
    "def helper(): pass\n",
    "app.py",
    "import lib\ndef go(): return lib.helper()\n",
    "helper",
    ImportScoped
);
parity!(
    test_java_import_target_and_tier,
    ecp_analyzer::java::parser::JavaProvider,
    "pkg/Helper.java",
    "package pkg; public class Helper { public static void helper() {} }",
    "App.java",
    "import pkg.Helper; class App { void go() { Helper.helper(); } }",
    "helper",
    QualifierScoped
);
parity!(
    test_kotlin_import_target_and_tier,
    ecp_analyzer::kotlin::parser::KotlinProvider,
    "pkg/Util.kt",
    "package pkg\nfun helper() {}",
    "App.kt",
    "import pkg.helper\nfun go() { helper() }",
    "helper",
    Global
);
parity!(
    test_csharp_import_target_and_tier,
    ecp_analyzer::c_sharp::parser::CSharpProvider,
    "Helper.cs",
    "namespace Lib { public class Helper { public static void helper() {} } }",
    "App.cs",
    "using Lib; class App { void go() { Helper.helper(); } }",
    "helper",
    Global
);
parity!(
    test_go_import_target_and_tier,
    ecp_analyzer::go::parser::GoProvider,
    "pkg/pkg.go",
    "package pkg\nfunc Helper() {}",
    "app.go",
    "package main\nimport \"pkg\"\nfunc run() { pkg.Helper() }",
    "Helper",
    Global
);
parity!(
    test_rust_import_target_and_tier,
    ecp_analyzer::rust::parser::RustProvider,
    "util.rs",
    "pub fn helper() {}",
    "app.rs",
    "mod util; fn go() { util::helper(); }",
    "helper",
    QualifierScoped
);
parity!(
    test_php_import_target_and_tier,
    ecp_analyzer::php::parser::PhpProvider,
    "lib.php",
    "<?php function helper() {}",
    "app.php",
    "<?php require 'lib.php'; function go() { helper(); }",
    "helper",
    Global
);
parity!(
    test_ruby_import_target_and_tier,
    ecp_analyzer::ruby::parser::RubyProvider,
    "lib.rb",
    "def helper; end",
    "app.rb",
    "require_relative 'lib'\ndef go; helper(); end",
    "helper",
    Global
);
parity!(
    test_swift_import_target_and_tier,
    ecp_analyzer::swift::parser::SwiftProvider,
    "Helpers.swift",
    "func helper() {}",
    "App.swift",
    "import Helpers\nfunc go() { helper() }",
    "helper",
    Global
);
parity!(
    test_c_import_target_and_tier,
    ecp_analyzer::c::parser::CProvider,
    "lib.c",
    "void helper(void) {}",
    "app.c",
    "#include \"lib.c\"\nvoid go(void) { helper(); }",
    "helper",
    Global
);
parity!(
    test_cpp_import_target_and_tier,
    ecp_analyzer::cpp::parser::CppProvider,
    "lib.hpp",
    "inline void helper() {}",
    "app.cpp",
    "#include \"lib.hpp\"\nvoid go() { helper(); }",
    "helper",
    Global
);
parity!(
    test_dart_import_target_and_tier,
    ecp_analyzer::dart::parser::DartProvider,
    "lib.dart",
    "void helper() {}",
    "app.dart",
    "import 'lib.dart'; void go() { helper(); }",
    "helper",
    Global
);
parity!(
    test_lua_import_target_and_tier,
    ecp_analyzer::lua::parser::LuaProvider,
    "x.lua",
    "function f() end",
    "app.lua",
    "local m = require('x')\nfunction go() m.f() end",
    "f",
    Global
);
