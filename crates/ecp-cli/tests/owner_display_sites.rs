//! Slice R4b: the CLI prints an owner through the display owner and the shared
//! resolver keeps lexically scoped nodes as candidates (design A §4.5, §4.4).
//!
//! No language opts in yet, so a real build writes an empty lexical-parent
//! table and the display owner of every node equals its stored owner. The
//! scoped-node tests plant a row through the table API, as
//! `lexical_parent_table.rs` does.

use ecp_analyzer::java::JavaProvider;
use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_cli::read_context::{resolve_target, target_request};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::contracts::identity::TargetResolution;
use ecp_core::graph::{ArchivedZeroCopyGraph, LexicalParentFact, ZeroCopyGraph};
use ecp_core::graph_fixture::GraphFixture;
use ecp_core::session::merged::MergedGraph;
use std::path::Path;

const SAMPLES: [(&str, &str); 4] = [
    (
        "app/models.py",
        "class Account:\n    def deposit(self):\n        pass\n\n\ndef top():\n    pass\n",
    ),
    (
        "web/cart.ts",
        "class Cart {\n  add() {}\n  total() { return 1; }\n}\n",
    ),
    (
        "src/lib.rs",
        "struct A;\nimpl A {\n    fn new() -> A {\n        A\n    }\n}\nfn free() {}\n",
    ),
    (
        "src/Calc.java",
        "public class Calc {\n    public int add(int a) { return a; }\n}\n",
    ),
];

fn provider_for(path: &str) -> Box<dyn LanguageProvider> {
    match path.rsplit('.').next() {
        Some("py") => Box::new(PythonProvider::new().unwrap()),
        Some("ts") => Box::new(TypeScriptProvider::new().unwrap()),
        Some("rs") => Box::new(RustProvider::new().unwrap()),
        Some("java") => Box::new(JavaProvider::new().unwrap()),
        other => panic!("no provider for {other:?}"),
    }
}

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for &(path, source) in files {
        builder.add_graph(
            provider_for(path)
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

fn archive(graph: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(graph).unwrap()
}

fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap()
}

/// Stands for: with no language opted in (empty lexical-parent table), every
/// owner string a command prints is the owner string it printed before the
/// display owner existed. A method keeps `Account`, `Cart`, `A`, `Calc`; a
/// free function keeps no owner.
#[test]
fn test_display_owner_m1_m2_unchanged_on_sample_repo() {
    let graph = build(&SAMPLES);
    assert!(graph.lexical_parents.is_empty(), "no language opts in yet");
    let bytes = archive(&graph);
    let archived = access(&bytes);
    let merged = MergedGraph::new(archived, None);
    let pool = archived.string_pool.as_slice();

    let mut printed: Vec<(String, String)> = Vec::new();
    for idx in 0..archived.nodes.len() as u32 {
        let stored = archived.nodes[idx as usize].owner_class.resolve(pool);
        let node = merged.node(idx).expect("base index");
        assert_eq!(
            merged.display_owner(idx).as_deref(),
            (!stored.is_empty()).then_some(stored),
            "node {idx}: the printed owner is the stored owner"
        );
        assert_eq!(
            merged.display_owner(idx).as_deref(),
            node.owner_class(&merged),
            "node {idx}: the display and the matching accessor agree"
        );
        if let Some(owner) = merged.display_owner(idx) {
            printed.push((node.name(&merged).to_string(), owner.into_owned()));
        }
    }
    // Setup guard: the sample really holds owned methods of the four languages,
    // so the loop above compared non-empty owners.
    for (name, owner) in [
        ("deposit", "Account"),
        ("add", "Cart"),
        ("new", "A"),
        ("add", "Calc"),
    ] {
        assert!(
            printed.iter().any(|(n, o)| n == name && o == owner),
            "setup: `{owner}.{name}` is printed; got {printed:?}"
        );
    }
    assert!(
        !printed.iter().any(|(n, _)| n == "top" || n == "free"),
        "a free function prints no owner; got {printed:?}"
    );
}

/// Stands for: a scoped node prints its lexical path, through the merged
/// accessor the cypher and inspect sites call; a virtual or out-of-range
/// index prints no invented path.
#[test]
fn test_merged_display_owner_planted_row_prints_lexical_path() {
    let mut fx = GraphFixture::new();
    let new = fx.method("src/a.rs", "A", "new");
    let pad = fx.func("src/a.rs", "pad");
    fx.assembly_mut().lexical_parents = vec![LexicalParentFact {
        node_idx: pad,
        parent_idx: new,
    }];
    let bytes = archive(&fx.build());
    let archived = access(&bytes);
    let merged = MergedGraph::new(archived, None);

    assert_eq!(merged.display_owner(new).as_deref(), Some("A"));
    assert_eq!(merged.display_owner(pad).as_deref(), Some("A::new"));
    assert_eq!(merged.display_owner(u32::MAX), None);
}

/// A top-level `helper` and a `helper` nested in `outer`, the nesting planted
/// as a lexical-parent row.
fn helper_graph(nested_only: bool) -> (rkyv::util::AlignedVec, u32, u32) {
    let mut fx = GraphFixture::new();
    let outer = fx.func("src/a.rs", "outer");
    let nested = fx.func("src/a.rs", "helper");
    let top = if nested_only {
        nested
    } else {
        fx.func("src/a.rs", "helper")
    };
    fx.assembly_mut().lexical_parents = vec![LexicalParentFact {
        node_idx: nested,
        parent_idx: outer,
    }];
    (archive(&fx.build()), top, nested)
}

/// Stands for: a bare name shared by a top-level function and one nested in a
/// function stays ambiguous through the shared resolver, so `impact` and
/// `inspect` list both and never pick one silently.
#[test]
fn test_resolve_target_bare_name_with_scoped_node_stays_ambiguous() {
    let (bytes, top, nested) = helper_graph(false);
    assert_ne!(top, nested, "setup: two `helper` nodes");
    let archived = access(&bytes);
    let merged = MergedGraph::new(archived, None);
    assert!(
        archived.is_lexically_scoped(nested),
        "setup: nested is scoped"
    );
    assert!(!archived.is_lexically_scoped(top), "setup: top is not");

    for text in ["helper", "::helper"] {
        let request = target_request(text, None, None).unwrap();
        assert!(
            matches!(
                resolve_target(&merged, &request),
                TargetResolution::Ambiguous(_)
            ),
            "{text} lists both declarations"
        );
    }
}
