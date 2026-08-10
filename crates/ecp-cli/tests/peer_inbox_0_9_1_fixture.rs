//! Reading an inbox that a previous version actually wrote.
//!
//! The fixture is 40 entries produced by ecp 0.9.1's watcher through its real
//! append path, captured before the schema changed. One field was edited by
//! hand: `symbol.file` on nine of bob's entries, to move them to a second file
//! — the shape that no synthetic fixture in this repo had, and the one that
//! made 0.9.2 drop concerns silently.
//!
//! It is committed rather than generated because a fixture written alongside
//! the new code proves only that the new code understands its own idea of the
//! old format. This proves the migration.

use ecp_cli::peer::render::render_payload;
use ecp_core::peer::inbox::InboxEntry;

fn entries() -> Vec<InboxEntry> {
    let raw = include_str!("fixtures/inbox-written-by-0.9.1.jsonl");
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("every 0.9.1 line must still decode"))
        .collect()
}

#[test]
fn every_line_written_by_0_9_1_still_decodes() {
    let all = entries();
    assert_eq!(all.len(), 40, "the fixture is 40 entries");
    for e in &all {
        match e {
            InboxEntry::DirtyEvent { file, symbol, .. } => {
                assert_eq!(file, "", "0.9.1 wrote no top-level file");
                assert!(symbol.is_some(), "0.9.1 wrote the path inside symbol");
            }
            other => panic!("fixture should be all dirty events: {other:?}"),
        }
        assert!(
            !e.event_file().is_empty(),
            "the path has to be recoverable from the entry"
        );
    }
}

/// The reason this fixture exists. 0.9.2 keyed dedupe on the absent top-level
/// field, so every entry keyed on the empty string: bob's nine `lib.py`
/// concerns and his nine `main.py` concerns collapsed into one, and the reader
/// was told about `main.py` while bob was also editing the file they had open.
#[test]
fn distinct_files_survive_the_drain_that_0_9_2_collapsed() {
    let (out, _) = render_payload(&entries());

    assert!(
        out.contains("HARD overlap (4 events)"),
        "three peers over two files is four concerns, not three: {out}"
    );
    for file in ["lib.py", "main.py"] {
        assert!(out.contains(file), "{file} must survive: {out}");
    }
    assert_eq!(
        out.matches("File:").count(),
        4,
        "each concern names its file: {out}"
    );
    assert!(
        !out.contains("File:   \n"),
        "no concern may render a blank file: {out}"
    );
}
