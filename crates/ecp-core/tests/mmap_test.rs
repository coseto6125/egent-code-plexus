use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::graph_fixture::GraphFixture;
use memmap2::Mmap;
use rkyv::rancor::Error;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

#[test]
fn test_mmap_graph_access() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("graph.bin");

    // 1. Create and Serialize Graph
    let mut fx = GraphFixture::new();
    let f = fx.func("test.ts", "mmap_func");
    fx.span(f, (1, 0, 10, 0));
    fx.assembly_mut().fingerprint = [1; 32];

    // Use rkyv::to_bytes for rkyv 0.8.x
    let bytes = fx.into_bytes();

    let mut file = File::create(&file_path).unwrap();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();

    // 2. Mmap and Read (Zero-Copy)
    let file = File::open(&file_path).unwrap();
    let mmap = unsafe { Mmap::map(&file).unwrap() };

    let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&mmap).unwrap();

    assert_eq!(archived.fingerprint, [1; 32]);

    let first_node = &archived.nodes[0];
    assert_eq!(first_node.kind, ecp_core::graph::NodeKind::Function);

    let resolved_name = first_node.name.resolve(&archived.string_pool);
    assert_eq!(resolved_name, "mmap_func");

    // `StrRef::default()` (offset=0, len=0) must survive the rkyv round-trip
    // and resolve to the empty string — the sentinel for "top-level symbol,
    // no owner class". A regression here would silently break
    // `commands::rename::run`'s bare-name filter (which compares len == 0).
    assert_eq!(first_node.owner_class.resolve(&archived.string_pool), "");
}
