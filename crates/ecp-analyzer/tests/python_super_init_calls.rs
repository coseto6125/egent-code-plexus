//! `super().__init__()` calls the base class's constructor, never another
//! class's constructor that happens to sit earlier in the same file
//! (FU-2026-10-04-def235b1c412).

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::RelType;
use std::path::Path;

/// `(caller owner, target owner)` for every Calls edge into an `__init__`.
fn init_edges(files: &[(&str, &str)]) -> Vec<(String, String)> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let py = PythonProvider::new().expect("PythonProvider::new");
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    for (rel, src) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, src).unwrap();
        builder.add_graph(
            py.parse_file(Path::new(rel), src.as_bytes())
                .expect("parse_file"),
        );
    }
    let graph = builder.build();
    let pool = graph.string_pool.as_slice();
    let owner = |i: u32| {
        let n = &graph.nodes[i as usize];
        format!("{}.{}", n.owner_class.resolve(pool), n.name.resolve(pool))
    };
    let mut out: Vec<(String, String)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == "__init__")
        .map(|e| (owner(e.source), owner(e.target)))
        .collect();
    out.sort();
    out
}

#[test]
fn test_super_init_binds_the_base_class_not_the_first_same_file_init() {
    let files = [(
        "client.py",
        "class _KeyPool:\n    def __init__(self):\n        pass\n\n\
         class LLMClient:\n    def __init__(self):\n        pass\n\n\
         class OpenAIClient(LLMClient):\n    def __init__(self):\n        super().__init__()\n",
    )];
    assert_eq!(
        init_edges(&files),
        vec![(
            "OpenAIClient.__init__".to_string(),
            "LLMClient.__init__".to_string()
        )]
    );
}

#[test]
fn test_super_init_binds_an_imported_base_class() {
    let files = [
        (
            "base.py",
            "class LLMClient:\n    def __init__(self):\n        pass\n",
        ),
        (
            "client.py",
            "from base import LLMClient\n\n\
             class _KeyPool:\n    def __init__(self):\n        pass\n\n\
             class OpenAIClient(LLMClient):\n    def __init__(self):\n        super().__init__()\n",
        ),
    ];
    assert_eq!(
        init_edges(&files),
        vec![(
            "OpenAIClient.__init__".to_string(),
            "LLMClient.__init__".to_string()
        )]
    );
}

/// A base class outside the project has no node: the call emits no edge
/// rather than a guess.
#[test]
fn test_super_init_with_an_external_base_emits_no_edge() {
    let files = [(
        "client.py",
        "import threading\n\n\
         class _KeyPool:\n    def __init__(self):\n        pass\n\n\
         class Worker(threading.Thread):\n    def __init__(self):\n        super().__init__()\n",
    )];
    assert_eq!(init_edges(&files), Vec::<(String, String)>::new());
}
