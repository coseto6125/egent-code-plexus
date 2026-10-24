//! A rejected graph.bin must tell the agent a command that exists
//! (`ecp analyze` does not; the rebuild verb is `ecp admin index --force`).

use ecp_cli::engine::Engine;
use ecp_core::graph::{GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use tempfile::tempdir;

#[test]
fn test_engine_load_unknown_version_names_admin_index_force() {
    let mut fx = GraphFixture::new();
    fx.func("src/main.ts", "entry");
    let mut g = fx.build();
    g.magic = GRAPH_MAGIC;
    g.version = GRAPH_FORMAT_VERSION.wrapping_add(99);
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, rkyv::to_bytes::<Error>(&g).unwrap()).unwrap();
    let Err(err) = Engine::load(&path) else {
        panic!("unknown version must be rejected");
    };
    let msg = err.to_string();
    assert!(msg.contains("ecp admin index --force"), "{msg}");
    assert!(!msg.contains("ecp analyze"), "{msg}");
}
