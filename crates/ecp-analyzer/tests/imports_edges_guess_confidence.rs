//! `Imports` edges of steps 3c to 3f guess a target from a name. A guess
//! carries a confidence below `HEURISTIC_CEILING` and the reason
//! `post_process:imports:guess`; a resolved import keeps 0.9 and
//! `post_process:imports:module`.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType};
use std::path::Path;

const GUESS: &str = "post_process:imports:guess";
const MODULE: &str = "post_process:imports:module";

/// `(source file, target file, confidence, reason)` of every file-to-file
/// `Imports` edge, sorted.
fn file_imports(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
) -> Vec<(String, String, f32, String)> {
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .expect("parse_file"),
        );
    }
    let graph = builder.build();
    let pool = graph.string_pool.as_slice();
    let path_of = |node: u32| {
        let file = graph.nodes[node as usize].file_idx as usize;
        graph.files[file].path.resolve(pool).to_string()
    };
    let mut out: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Imports)
        .filter(|e| graph.nodes[e.source as usize].kind == NodeKind::File)
        .filter(|e| graph.nodes[e.target as usize].kind == NodeKind::File)
        .map(|e| {
            (
                path_of(e.source),
                path_of(e.target),
                e.confidence,
                e.reason.resolve(pool).to_string(),
            )
        })
        .collect();
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

fn edge<'a>(
    edges: &'a [(String, String, f32, String)],
    from: &str,
    to: &str,
) -> Option<&'a (String, String, f32, String)> {
    edges.iter().find(|e| e.0 == from && e.1 == to)
}

fn assert_guess(edges: &[(String, String, f32, String)], from: &str, to: &str) {
    let hit = edge(edges, from, to).unwrap_or_else(|| panic!("no edge {from} -> {to}: {edges:?}"));
    assert!(hit.2 < 0.7, "guess confidence {} in {edges:?}", hit.2);
    assert_eq!(hit.3, GUESS, "{edges:?}");
}

fn assert_resolved(edges: &[(String, String, f32, String)], from: &str, to: &str) {
    let hit = edge(edges, from, to).unwrap_or_else(|| panic!("no edge {from} -> {to}: {edges:?}"));
    assert_eq!(hit.2, 0.9, "{edges:?}");
    assert_eq!(hit.3, MODULE, "{edges:?}");
}

#[test]
fn test_imports_edges_basename_guess_confidence_below_ceiling_c() {
    let edges = file_imports(
        &CProvider::new().expect("CProvider"),
        &[
            (
                "src/main.c",
                "#include \"alpha.h\"\n#include \"util.h\"\nint main(void) { return 0; }\n",
            ),
            ("include/alpha.h", "int alpha(void);\n"),
            ("src/util.h", "int util(void);\n"),
        ],
    );
    assert_guess(&edges, "src/main.c", "include/alpha.h");
    assert_resolved(&edges, "src/main.c", "src/util.h");
}

#[test]
fn test_imports_edges_basename_guess_confidence_below_ceiling_cpp() {
    let edges = file_imports(
        &CppProvider::new().expect("CppProvider"),
        &[
            (
                "src/main.cpp",
                "#include \"alpha.hpp\"\n#include \"util.hpp\"\nint main() { return 0; }\n",
            ),
            ("include/alpha.hpp", "int alpha();\n"),
            ("src/util.hpp", "int util();\n"),
        ],
    );
    assert_guess(&edges, "src/main.cpp", "include/alpha.hpp");
    assert_resolved(&edges, "src/main.cpp", "src/util.hpp");
}

#[test]
fn test_imports_edges_basename_guess_confidence_below_ceiling_csharp() {
    let edges = file_imports(
        &CSharpProvider::new().expect("CSharpProvider"),
        &[
            ("app/Program.cs", "using Alpha;\nclass Program { }\n"),
            ("lib/Alpha/Impl.cs", "namespace Alpha { class Impl { } }\n"),
        ],
    );
    assert_guess(&edges, "app/Program.cs", "lib/Alpha/Impl.cs");
}

#[test]
fn test_imports_edges_basename_guess_confidence_below_ceiling_rust() {
    let edges = file_imports(
        &RustProvider::new().expect("RustProvider"),
        &[
            ("src/lib.rs", "mod app;\nmod util;\n"),
            (
                "src/app.rs",
                "use crate::model::Repo;\nuse crate::util::Helper;\npub fn run() {}\n",
            ),
            ("src/other/model.rs", "pub fn other() {}\n"),
            ("src/util.rs", "pub fn other() {}\n"),
        ],
    );
    assert_guess(&edges, "src/app.rs", "src/other/model.rs");
    assert!(
        edges
            .iter()
            .filter(|e| e.0 == "src/app.rs" && e.1 == "src/util.rs")
            .all(|e| e.3 != GUESS),
        "a module the tree resolves is not a guess: {edges:?}"
    );
}

#[test]
fn test_imports_edges_basename_guess_confidence_below_ceiling_typescript() {
    let edges = file_imports(
        &TypeScriptProvider::new().expect("TypeScriptProvider"),
        &[
            (
                "src/main.ts",
                "import \"shared/alpha\";\nimport \"./util\";\n",
            ),
            ("pkg/shared/alpha.ts", "export const alpha = 1;\n"),
            ("src/util.ts", "export const util = 1;\n"),
        ],
    );
    assert_guess(&edges, "src/main.ts", "pkg/shared/alpha.ts");
    assert_resolved(&edges, "src/main.ts", "src/util.ts");
}

#[test]
fn test_imports_edges_two_files_with_one_basename_emits_no_guess() {
    let edges = file_imports(
        &CProvider::new().expect("CProvider"),
        &[
            (
                "src/main.c",
                "#include \"alpha.h\"\nint main(void) { return 0; }\n",
            ),
            ("include/a/alpha.h", "int a(void);\n"),
            ("include/b/alpha.h", "int b(void);\n"),
        ],
    );
    assert!(
        edge(&edges, "src/main.c", "include/a/alpha.h").is_none()
            && edge(&edges, "src/main.c", "include/b/alpha.h").is_none(),
        "{edges:?}"
    );
}

#[test]
fn test_imports_edges_include_without_matching_file_emits_nothing() {
    let edges = file_imports(
        &CProvider::new().expect("CProvider"),
        &[(
            "src/main.c",
            "#include \"missing.h\"\nint main(void) { return 0; }\n",
        )],
    );
    assert!(edges.is_empty(), "{edges:?}");
}

/// The Rust module-path part: the same target for `crate::`, `self::`,
/// `super::` and a nested module as before the step read `rust_paths.rs`.
/// Each fixture places the module where the tree does not (under `elsewhere/`),
/// so the edge comes from step 3e's name match.
fn rust_guess_targets(caller: &str, source: &str, extra: &[(&str, &str)]) -> Vec<String> {
    let mut files = vec![("src/lib.rs", "mod a;\n"), (caller, source)];
    files.extend_from_slice(extra);
    let edges = file_imports(&RustProvider::new().expect("RustProvider"), &files);
    edges
        .into_iter()
        .filter(|e| e.0 == caller && e.3 == GUESS)
        .map(|e| e.1)
        .collect()
}

#[test]
fn test_imports_edges_rust_crate_path_guesses_last_module() {
    assert_eq!(
        rust_guess_targets(
            "src/a.rs",
            "use crate::x::y::Foo;\n",
            &[("elsewhere/y.rs", "pub fn other() {}\n")],
        ),
        vec!["elsewhere/y.rs".to_string()]
    );
}

#[test]
fn test_imports_edges_rust_crate_root_item_guesses_item_as_module() {
    assert_eq!(
        rust_guess_targets(
            "src/a.rs",
            "use crate::y;\n",
            &[("elsewhere/y.rs", "pub fn f() {}\n")],
        ),
        vec!["elsewhere/y.rs".to_string()]
    );
}

#[test]
fn test_imports_edges_rust_self_path_guesses_last_module() {
    assert_eq!(
        rust_guess_targets(
            "src/a.rs",
            "use self::y::Foo;\n",
            &[("elsewhere/y.rs", "pub fn other() {}\n")],
        ),
        vec!["elsewhere/y.rs".to_string()]
    );
}

#[test]
fn test_imports_edges_rust_super_path_emits_no_guess() {
    assert!(rust_guess_targets(
        "src/a/b.rs",
        "use super::y::Foo;\n",
        &[("elsewhere/y.rs", "pub fn other() {}\n")],
    )
    .is_empty());
}

#[test]
fn test_imports_edges_rust_external_crate_path_emits_no_guess() {
    assert!(rust_guess_targets(
        "src/a.rs",
        "use std::y::Foo;\n",
        &[("elsewhere/y.rs", "pub fn other() {}\n")],
    )
    .is_empty());
}

#[test]
fn test_imports_edges_rust_file_without_src_ancestor_keeps_last_segment_guess() {
    // `tests/` files have no `src/` root for `rust_module_path_base`; the
    // step keeps the plain last segment, as before.
    assert_eq!(
        rust_guess_targets(
            "tests/it.rs",
            "use crate::x::y::Foo;\n",
            &[("elsewhere/y.rs", "pub fn other() {}\n")],
        ),
        vec!["elsewhere/y.rs".to_string()]
    );
}
