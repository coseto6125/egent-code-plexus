//! A `PathLiteral` uid hashes its bound node's display owner
//! (`record_nodes::path_literal_node`). A nested function has no owner
//! (R8), so two identical literals in same-name nested functions of two
//! parents differ by the parents' names, in the index and in the overlay
//! replay alike. The languages are the four that scope nested functions
//! (`lexical_scope_kinds`): Python, JavaScript, TypeScript and Rust.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use strict_parity::{Edit, Scenario, Snapshot};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// `(case, path, source)`: one `session_meta.json` literal under each of two
/// parents, in a nested function of the same name.
const NESTED: &[(&str, &str, &str)] = &[
    (
        "python",
        "app/load.py",
        "def first():\n    def inner():\n        return open(\"session_meta.json\")\n    return inner\n\n\ndef second():\n    def inner():\n        return open(\"session_meta.json\")\n    return inner\n",
    ),
    (
        "javascript",
        "src/load.js",
        "const fs = require(\"fs\");\nfunction first() {\n    function inner() {\n        return fs.readFileSync(\"session_meta.json\", \"utf-8\");\n    }\n    return inner;\n}\nfunction second() {\n    function inner() {\n        return fs.readFileSync(\"session_meta.json\", \"utf-8\");\n    }\n    return inner;\n}\n",
    ),
    (
        "typescript",
        "src/load.ts",
        "import * as fs from \"fs\";\nfunction first(): () => string {\n    function inner(): string {\n        return fs.readFileSync(\"session_meta.json\", \"utf-8\");\n    }\n    return inner;\n}\nfunction second(): () => string {\n    function inner(): string {\n        return fs.readFileSync(\"session_meta.json\", \"utf-8\");\n    }\n    return inner;\n}\n",
    ),
    (
        "rust",
        "src/load.rs",
        "use std::fs;\nfn first() -> String {\n    fn inner() -> std::io::Result<String> {\n        fs::read_to_string(\"session_meta.json\")\n    }\n    inner().unwrap()\n}\nfn second() -> String {\n    fn inner() -> std::io::Result<String> {\n        fs::read_to_string(\"session_meta.json\")\n    }\n    inner().unwrap()\n}\n",
    ),
];

/// A file-level literal beside one in a nested function, and a literal in a
/// Rust argument closure of each of two functions (a `let` closure is no
/// node, R6, so its literal binds the enclosing top-level fn).
const MIXED: &[(&str, &str, &str)] = &[
    (
        "python file level beside nested",
        "app/load.py",
        "CONFIG = open(\"session_meta.json\")\n\n\ndef first():\n    def inner():\n        return open(\"session_meta.json\")\n    return inner\n",
    ),
    (
        "rust closures",
        "src/load.rs",
        "use std::fs;\nfn run<T>(f: impl Fn() -> T) -> T {\n    f()\n}\nfn first() -> std::io::Result<String> {\n    run(|| fs::read_to_string(\"session_meta.json\"))\n}\nfn second() -> std::io::Result<String> {\n    run(|| fs::read_to_string(\"session_meta.json\"))\n}\n",
    ),
];

/// The `PathLiteral` identity lines of `snap` for `path`, sorted.
fn literals(snap: &Snapshot, path: &str) -> Vec<String> {
    let prefix = format!("PathLiteral {path}:");
    let mut lines: Vec<String> = snap
        .identity
        .iter()
        .filter(|line| line.starts_with(&prefix))
        .cloned()
        .collect();
    lines.sort();
    lines
}

/// The `UsesPathLiteral` edge lines of `snap`, sorted.
fn literal_edges(snap: &Snapshot) -> Vec<String> {
    let mut lines: Vec<String> = snap
        .edge
        .iter()
        .filter(|line| line.starts_with("UsesPathLiteral "))
        .cloned()
        .collect();
    lines.sort();
    lines
}

/// The uid part (`#…`) of an identity line.
fn uid(line: &str) -> &str {
    line.rsplit_once(" #").map_or("", |(_, uid)| uid)
}

/// The full index of the edited tree holds two literal nodes of `path` with
/// distinct uids, and the overlay of the edit holds the same nodes and edges.
fn assert_two_uids(case: &str, path: &str, source: &str) {
    let edited = format!("\n{source}");
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files: &[(path, source)],
        edits: &[Edit::Write(path, &edited)],
    });
    let (full, overlay) = observed.snapshots();
    let full_literals = literals(&full, path);
    assert_eq!(full_literals.len(), 2, "{case}: {:#?}", full.identity);
    assert_ne!(
        uid(&full_literals[0]),
        uid(&full_literals[1]),
        "{case}: two literals under different parents share a uid: {full_literals:#?}"
    );
    assert_eq!(
        literals(&overlay, path),
        full_literals,
        "{case}: overlay nodes"
    );
    let full_edges = literal_edges(&full);
    assert_eq!(full_edges.len(), 2, "{case}: {full_edges:#?}");
    assert_eq!(literal_edges(&overlay), full_edges, "{case}: overlay edges");
}

#[test]
fn test_path_literal_node_nested_functions_of_two_parents_distinct_uids_index_and_overlay() {
    for &(case, path, source) in NESTED {
        assert_two_uids(case, path, source);
    }
}

#[test]
fn test_path_literal_node_file_level_and_closure_literals_distinct_uids_index_and_overlay() {
    for &(case, path, source) in MIXED {
        assert_two_uids(case, path, source);
    }
}
