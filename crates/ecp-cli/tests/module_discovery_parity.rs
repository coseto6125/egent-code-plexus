//! Module discovery parity (B6a): one file set fed to the index's
//! `Resolver::import_module_file` and to the overlay's `ImportScope::module`
//! must name the same module file for each specifier, a miss included. Both
//! run `ecp_core::analyzer::module_discovery::discover_module`; what can
//! still drift is each side's facts (indexed paths, stems, namespaces,
//! declarations) and its memo.
//!
//! The index side is the symbol table Pass 1 registers from the files after
//! the edit. The overlay side is the base graph the builder writes from the
//! files before it, plus the dirty files' fresh parses.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::index::SymbolTable;
use ecp_analyzer::resolution::resolver::Resolver;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::module_discovery::Module;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::file_category::Language;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::session::import_scope::ImportScope;
use ecp_core::session::view::OverlayFileInput;
use rustc_hash::FxHashSet;
use std::path::Path;

/// A module answer with owned paths, namespace files sorted: the two sides
/// list a namespace's files in their own index order.
#[derive(Debug, PartialEq)]
enum Found {
    Missing,
    Unique(String),
    Ambiguous,
    Namespace(Vec<String>),
}

fn found<P: AsRef<str>>(module: &Module<P>) -> Found {
    match module {
        Module::Missing => Found::Missing,
        Module::Unique(file) => Found::Unique(file.as_ref().to_string()),
        Module::Ambiguous => Found::Ambiguous,
        Module::Namespace(files) => {
            let mut files: Vec<String> = files.iter().map(|f| f.as_ref().to_string()).collect();
            files.sort_unstable();
            Found::Namespace(files)
        }
    }
}

fn unique(file: &str) -> Found {
    Found::Unique(file.to_string())
}

fn namespace(files: &[&str]) -> Found {
    Found::Namespace(files.iter().map(|f| f.to_string()).collect())
}

/// Pass 1's registrations that discovery reads: each file's declarations,
/// which also record the namespaces a file declares.
fn symbol_table(locals: &[LocalGraph]) -> SymbolTable {
    let mut table = SymbolTable::new();
    let mut id = 0;
    for local in locals {
        let path = local.file_path.to_string_lossy().replace('\\', "/");
        for node in &local.nodes {
            table.register_node(&path, &node.name, id, node.kind);
            id += 1;
        }
    }
    table.build_stem_index();
    table
}

struct Case<'c> {
    provider: &'c dyn LanguageProvider,
    language: Language,
    /// The files before the edit: the overlay's base graph.
    base: &'c [(&'c str, &'c str)],
    /// The edited or added files, as they are after the edit.
    dirty: &'c [(&'c str, &'c str)],
    /// Base files the edit deletes.
    removed: &'c [&'c str],
    /// `(importer, specifier, expected)`.
    rows: &'c [(&'c str, &'c str, Found)],
    /// A file whose parsed import sources both sides must also agree on.
    parsed_importer: Option<&'c str>,
}

fn check(case: Case<'_>) {
    let parse = |&(path, source): &(&str, &str)| {
        case.provider
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap()
    };
    let after: Vec<LocalGraph> = case
        .base
        .iter()
        .filter(|(path, _)| {
            !case.removed.contains(path) && case.dirty.iter().all(|(dirty, _)| dirty != path)
        })
        .chain(case.dirty)
        .map(parse)
        .collect();
    let table = symbol_table(&after);
    let resolver = Resolver::new(&table);

    let mut builder = GraphBuilder::new();
    for file in case.base {
        builder.add_graph(parse(file));
    }
    let base = builder.build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let inputs: Vec<OverlayFileInput> = case.dirty.iter().map(|file| parse(file).into()).collect();
    let dirty_files: FxHashSet<u32> = inputs
        .iter()
        .filter_map(|input| graph.file_idx_of(&input.rel_path))
        .collect();
    let dirty_base: FxHashSet<u32> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| dirty_files.contains(&node.file_idx.to_native()))
        .map(|(idx, _)| idx as u32)
        .collect();
    let scope = ImportScope::new(graph, &inputs, &dirty_base);

    let both = |importer: &str, specifier: &str| {
        let index =
            found(&resolver.import_module_file(Path::new(importer), specifier, case.language));
        let overlay = found(&scope.module(importer, specifier, case.language));
        assert_eq!(
            index, overlay,
            "{importer} imports {specifier:?}: the index and the overlay name different modules"
        );
        // The same specifier again answers from each side's memo.
        assert_eq!(
            found(&resolver.import_module_file(Path::new(importer), specifier, case.language)),
            index,
            "{importer} imports {specifier:?}: the index memo changed the answer"
        );
        assert_eq!(
            found(&scope.module(importer, specifier, case.language)),
            overlay,
            "{importer} imports {specifier:?}: the overlay memo changed the answer"
        );
        index
    };
    for &(importer, specifier, ref expected) in case.rows {
        assert_eq!(
            &both(importer, specifier),
            expected,
            "{importer} imports {specifier:?}"
        );
    }
    if let Some(importer) = case.parsed_importer {
        let local = after
            .iter()
            .find(|local| local.file_path.to_string_lossy() == importer)
            .expect("the parsed importer is one of the files");
        assert!(
            !local.imports.is_empty(),
            "{importer}: the parser recorded no import, so the check would prove nothing"
        );
        for import in &local.imports {
            both(importer, import.source.as_str());
        }
    }
}

const PYTHON: &[(&str, &str)] = &[
    ("pkg/__init__.py", "def init():\n    pass\n"),
    ("pkg/util.py", "def helper():\n    pass\n"),
    ("pkg/sub/__init__.py", "def sub_init():\n    pass\n"),
    ("pkg/sub/mod.py", "from ..util import helper\n\ndef run():\n    helper()\n"),
    ("src/lib/tool.py", "def tool():\n    pass\n"),
    ("a/dup/x.py", "def a_x():\n    pass\n"),
    ("b/dup/x.py", "def b_x():\n    pass\n"),
    (
        "main.py",
        "from pkg.util import helper\nimport pkg\nfrom lib.tool import tool\n\ndef main():\n    helper()\n",
    ),
];

#[test]
fn test_ladder_module_discovery_python_same_both_sides() {
    let provider = PythonProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::Python,
        base: PYTHON,
        dirty: &[],
        removed: &[],
        rows: &[
            ("pkg/sub/mod.py", "..util", unique("pkg/util.py")),
            ("pkg/sub/mod.py", "..", unique("pkg/__init__.py")),
            ("pkg/sub/mod.py", ".", unique("pkg/sub/__init__.py")),
            ("pkg/sub/mod.py", ".nope", Found::Missing),
            // Four dots climb above the repo root.
            ("pkg/sub/mod.py", "....", Found::Missing),
            ("main.py", "pkg/util", unique("pkg/util.py")),
            ("main.py", "pkg", unique("pkg/__init__.py")),
            ("main.py", "lib/tool", unique("src/lib/tool.py")),
            ("main.py", "dup/x", Found::Ambiguous),
            ("main.py", "util", Found::Missing),
            ("main.py", "", Found::Missing),
            ("main.py", "   ", Found::Missing),
        ],
        parsed_importer: Some("main.py"),
    });
}

#[test]
fn test_ladder_module_discovery_python_dirty_added_module_same_both_sides() {
    let provider = PythonProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::Python,
        base: &[
            ("pkg/__init__.py", "def init():\n    pass\n"),
            ("pkg/sub/mod.py", "def run():\n    pass\n"),
        ],
        dirty: &[("pkg/extra.py", "def extra():\n    pass\n")],
        removed: &[],
        rows: &[
            ("pkg/sub/mod.py", "..extra", unique("pkg/extra.py")),
            ("main.py", "pkg/extra", unique("pkg/extra.py")),
            ("main.py", "extra", Found::Missing),
        ],
        parsed_importer: None,
    });
}

#[test]
#[ignore = "overlay divergence: ImportScope indexes every base file, a removed one included, \
            so a deleted module stays discoverable until the next full index"]
fn test_ladder_module_discovery_python_dirty_removed_module_same_both_sides() {
    let provider = PythonProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::Python,
        base: &[
            ("pkg/__init__.py", "def init():\n    pass\n"),
            ("pkg/gone.py", "def gone():\n    pass\n"),
        ],
        dirty: &[],
        removed: &["pkg/gone.py"],
        rows: &[("main.py", "pkg/gone", Found::Missing)],
        parsed_importer: None,
    });
}

#[test]
fn test_ladder_module_discovery_typescript_same_both_sides() {
    let provider = TypeScriptProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::TypeScript,
        base: &[
            ("src/util.ts", "export function f() {}\n"),
            ("src/lib/index.ts", "export function g() {}\n"),
            ("src/one/x.ts", "export function one() {}\n"),
            ("src/two/x.ts", "export function two() {}\n"),
            (
                "src/app.ts",
                "import { f } from './util';\nimport { g } from './lib';\nexport function app() { f(); g(); }\n",
            ),
        ],
        dirty: &[],
        removed: &[],
        rows: &[
            ("src/app.ts", "./util", unique("src/util.ts")),
            ("src/app.ts", "./lib", unique("src/lib/index.ts")),
            ("src/app.ts", "./nope", Found::Missing),
            ("src/app.ts", "util", unique("src/util.ts")),
            ("src/app.ts", "x", Found::Ambiguous),
            ("src/app.ts", "", Found::Missing),
            ("src/app.ts", "   ", Found::Missing),
        ],
        parsed_importer: Some("src/app.ts"),
    });
}

#[test]
fn test_ladder_module_discovery_go_same_both_sides() {
    let provider = GoProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::Go,
        base: &[
            ("pkg/util.go", "package pkg\n\nfunc Helper() {}\n"),
            ("a/x.go", "package a\n\nfunc A() {}\n"),
            ("b/x.go", "package b\n\nfunc B() {}\n"),
            (
                "main.go",
                "package main\n\nimport \"pkg/util\"\n\nfunc main() { util.Helper() }\n",
            ),
        ],
        dirty: &[],
        removed: &[],
        rows: &[
            ("main.go", "pkg/util", unique("pkg/util.go")),
            ("main.go", "pkg/nope", Found::Missing),
            ("main.go", "util", unique("pkg/util.go")),
            ("main.go", "x", Found::Ambiguous),
            ("main.go", "", Found::Missing),
            ("main.go", "   ", Found::Missing),
        ],
        parsed_importer: Some("main.go"),
    });
}

#[test]
fn test_ladder_module_discovery_rust_same_both_sides() {
    let provider = RustProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::Rust,
        base: &[
            ("src/util.rs", "pub fn helper() {}\n"),
            ("src/net/mod.rs", "pub fn connect() {}\n"),
            (
                "src/main.rs",
                "mod net;\nmod util;\nuse crate::util::helper;\n\nfn main() {\n    helper();\n}\n",
            ),
        ],
        dirty: &[],
        removed: &[],
        rows: &[
            ("src/main.rs", "crate::util", unique("src/util.rs")),
            // A directory module.
            ("src/main.rs", "crate::net", unique("src/net/mod.rs")),
            ("src/main.rs", "crate::nope", Found::Missing),
            ("src/main.rs", "", Found::Missing),
            ("src/main.rs", "   ", Found::Missing),
        ],
        parsed_importer: Some("src/main.rs"),
    });
}

const PHP: &[(&str, &str)] = &[
    (
        "src/Util/Helper.php",
        "<?php\nnamespace App\\Util;\n\nclass Helper {\n    public function run() {}\n}\n",
    ),
    (
        "src/Util/Other.php",
        "<?php\nnamespace App\\Util;\n\nclass Other {\n    public function go() {}\n}\n",
    ),
    ("lib/Acme/Tool.php", "<?php\n\nclass Tool {\n    public function use_it() {}\n}\n"),
    ("x/Lib/Thing.php", "<?php\n\nclass Thing {}\n"),
    ("y/Lib/Thing.php", "<?php\n\nclass Thing {}\n"),
    (
        "src/main.php",
        "<?php\nuse App\\Util\\Helper;\nuse Acme\\Tool;\n\nfunction main() {\n    Helper::run();\n}\n",
    ),
];

#[test]
fn test_ladder_module_discovery_php_same_both_sides() {
    let provider = PhpProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::Php,
        base: PHP,
        dirty: &[],
        removed: &[],
        rows: &[
            // A namespace prefix: the files of the namespace that declare the member.
            (
                "src/main.php",
                "App\\Util\\Helper",
                namespace(&["src/Util/Helper.php"]),
            ),
            (
                "src/main.php",
                "\\App\\Util\\Helper",
                namespace(&["src/Util/Helper.php"]),
            ),
            (
                "src/main.php",
                "App\\Util",
                namespace(&["src/Util/Helper.php", "src/Util/Other.php"]),
            ),
            ("src/main.php", "App\\Util\\Nope", namespace(&[])),
            // A conventionally placed file.
            ("src/main.php", "Acme\\Tool", unique("lib/Acme/Tool.php")),
            ("src/main.php", "Lib\\Thing", Found::Ambiguous),
            // A namespace prefix that matches no file.
            ("src/main.php", "Vendor\\Pkg\\Thing", Found::Missing),
            ("src/main.php", "", Found::Missing),
            ("src/main.php", "   ", Found::Missing),
        ],
        parsed_importer: Some("src/main.php"),
    });
}

#[test]
fn test_ladder_module_discovery_php_dirty_namespace_change_same_both_sides() {
    let provider = PhpProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::Php,
        base: &[(
            "App/Old/Thing.php",
            "<?php\nnamespace App\\Old;\n\nclass Thing {}\n",
        )],
        dirty: &[(
            "App/Old/Thing.php",
            "<?php\nnamespace Other;\n\nclass Thing {}\n",
        )],
        removed: &[],
        rows: &[
            // The placed file now declares another namespace: a namesake.
            ("src/main.php", "App\\Old\\Thing", Found::Missing),
            (
                "src/main.php",
                "Other\\Thing",
                namespace(&["App/Old/Thing.php"]),
            ),
        ],
        parsed_importer: None,
    });
}

#[test]
fn test_ladder_module_discovery_csharp_same_both_sides() {
    let provider = CSharpProvider::new().unwrap();
    check(Case {
        provider: &provider,
        language: Language::CSharp,
        base: &[
            (
                "src/Models/User.cs",
                "namespace App.Models {\n    public class User {}\n}\n",
            ),
            ("src/A/Dup.cs", "public class DupA {}\n"),
            ("src/B/Dup.cs", "public class DupB {}\n"),
            (
                "src/Program.cs",
                "using App.Models;\n\npublic class Program {\n    public static void Main() {}\n}\n",
            ),
        ],
        dirty: &[],
        removed: &[],
        rows: &[
            ("src/Program.cs", "./Models/User", unique("src/Models/User.cs")),
            ("src/Program.cs", "Models/User", unique("src/Models/User.cs")),
            // C# names namespaces, not files: no layout matches one.
            ("src/Program.cs", "App.Models", Found::Missing),
            ("src/Program.cs", "Dup", Found::Ambiguous),
            ("src/Program.cs", "", Found::Missing),
            ("src/Program.cs", "   ", Found::Missing),
        ],
        parsed_importer: Some("src/Program.cs"),
    });
}
