//! Regression tests for `Engine::load` header validation.
//!
//! Without these guards, a `graph.bin` with corrupted magic bytes or a
//! mismatched on-disk format version would pass `rkyv::access`'s
//! structural check and segfault (or silently misinterpret data) the
//! moment a field is dereferenced.

use ecp_cli::engine::Engine;
use ecp_core::graph::{GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use tempfile::tempdir;

/// One-node graph whose header fields are caller-chosen — the point of these
/// tests is a *rejected* header, which the fixture builder always gets right.
fn make_graph(magic: [u8; 8], version: u32) -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let entry = fx.func("src/main.ts", "entry");
    fx.span(entry, (1, 0, 5, 0));
    let mut g = fx.build();
    g.magic = magic;
    g.version = version;
    rkyv::to_bytes::<Error>(&g).unwrap().to_vec()
}

#[test]
fn engine_load_accepts_well_formed_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, make_graph(GRAPH_MAGIC, GRAPH_FORMAT_VERSION)).unwrap();
    Engine::load(&path).expect("well-formed header must load");
}

fn expect_invalid_data(path: &std::path::Path, expected_fragment: &str) {
    let Err(err) = Engine::load(path) else {
        panic!("expected InvalidData, got Ok");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    assert!(
        err.to_string().contains(expected_fragment),
        "error message should contain {expected_fragment:?}: {err}"
    );
}

#[test]
fn engine_load_rejects_wrong_magic() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, make_graph(*b"XXXXXXXX", GRAPH_FORMAT_VERSION)).unwrap();
    expect_invalid_data(&path, "bad magic");
}

#[test]
fn engine_load_rejects_unknown_version() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(
        &path,
        make_graph(GRAPH_MAGIC, GRAPH_FORMAT_VERSION.wrapping_add(99)),
    )
    .unwrap();
    expect_invalid_data(&path, "incompatible format version");
}

#[test]
fn engine_load_rejects_garbage_bytes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, b"not an rkyv archive").unwrap();
    let Err(err) = Engine::load(&path) else {
        panic!("garbage bytes must be rejected");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
}
