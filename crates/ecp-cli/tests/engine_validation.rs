//! `header_compatible` (auto_ensure's probe) and `Engine::load` both validate
//! the whole graph file with `rkyv::access`. The second walk over the same
//! bytes is skipped when the first already passed in this process; a file
//! that changed underneath (length or mtime) is validated again, so a
//! corrupt replacement can never ride on a stale pass.

use ecp_cli::engine::{header_compatible, test_counters::DEEP_VALIDATION_COUNT, Engine};
use ecp_core::graph::{GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use std::sync::atomic::Ordering;
use tempfile::tempdir;

fn make_graph() -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let entry = fx.func("src/main.ts", "entry");
    fx.span(entry, (1, 0, 5, 0));
    let mut g = fx.build();
    g.magic = GRAPH_MAGIC;
    g.version = GRAPH_FORMAT_VERSION;
    rkyv::to_bytes::<Error>(&g).unwrap().to_vec()
}

fn deep_validations() -> usize {
    DEEP_VALIDATION_COUNT.load(Ordering::Relaxed)
}

#[test]
fn test_engine_load_after_header_compatible_validates_once() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, make_graph()).unwrap();

    let before = deep_validations();
    assert!(header_compatible(&path));
    let engine = Engine::load(&path).expect("validated file loads");
    assert_eq!(
        deep_validations() - before,
        1,
        "header_compatible + Engine::load must walk the file once"
    );
    // The skipped walk still hands the caller a readable graph.
    assert_eq!(engine.graph().unwrap().nodes.len(), 1);
}

#[test]
fn test_engine_load_revalidates_when_file_changed_underneath() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    let bytes = make_graph();
    std::fs::write(&path, &bytes).unwrap();
    assert!(header_compatible(&path));

    // Same path, different length: the earlier pass must not be trusted.
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let Err(err) = Engine::load(&path) else {
        panic!("truncated file must be rejected");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    assert!(!header_compatible(&path));
}

#[test]
fn test_header_compatible_rejects_wrong_magic_without_caching_it() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    let mut bytes = make_graph();
    let good = bytes.clone();
    // rkyv places the root struct at the tail; corrupt the magic by flipping a
    // byte the validator reads, then restore the file and expect a re-walk.
    let magic_pos = bytes
        .windows(GRAPH_MAGIC.len())
        .rposition(|w| w == GRAPH_MAGIC)
        .expect("magic present");
    bytes[magic_pos] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    assert!(!header_compatible(&path));

    std::fs::write(&path, &good).unwrap();
    let before = deep_validations();
    Engine::load(&path).expect("restored file loads");
    assert_eq!(
        deep_validations() - before,
        1,
        "a rejected file is never cached"
    );
}
