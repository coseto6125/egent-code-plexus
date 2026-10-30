//! Parser-extraction proof for architecture phase 3: a driver move changes
//! no raw record, so the `LocalGraph` dump of a corpus is byte-identical
//! before and after the move.
//!
//! Run once per binary and diff the two files:
//!
//! ```text
//! ECP_DUMP_ROOT=<corpus dir> ECP_DUMP_OUT=<file> \
//!   cargo test -p egent-code-plexus --test local_graph_dump -- --ignored
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;

#[test]
#[ignore = "tool: needs ECP_DUMP_ROOT and ECP_DUMP_OUT"]
fn test_dump_local_graphs_corpus_writes_path_sorted_debug() {
    let root = PathBuf::from(std::env::var("ECP_DUMP_ROOT").expect("ECP_DUMP_ROOT"));
    let out = PathBuf::from(std::env::var("ECP_DUMP_OUT").expect("ECP_DUMP_OUT"));
    let pairs: Vec<(PathBuf, PathBuf)> = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .build()
        .map(|e| e.unwrap_or_else(|err| panic!("corpus walk: {err}")))
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| {
            let abs = e.into_path();
            let rel = abs.strip_prefix(&root).unwrap().to_path_buf();
            (abs, rel)
        })
        .collect();
    let mut graphs = ecp_cli::reanalyze::make_pipeline().analyze(pairs);
    assert!(
        !graphs.is_empty(),
        "no file under {} parsed",
        root.display()
    );
    // Both binaries share the size cap, so `file-too-large` still compares
    // like with like; a file the parser never saw proves nothing.
    let unparsed: Vec<_> = graphs
        .iter()
        .filter(|g| {
            g.nodes.is_empty()
                && g.blind_spots
                    .iter()
                    .any(|b| matches!(b.kind.as_str(), "parse-failed" | "file-unreadable"))
        })
        .map(|g| g.file_path.display().to_string())
        .collect();
    assert!(unparsed.is_empty(), "not parsed: {unparsed:?}");
    graphs.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    let mut dump = String::new();
    for g in &graphs {
        writeln!(dump, "{g:#?}").unwrap();
    }
    std::fs::write(&out, dump).unwrap();
}
