use gnx_core::graph::{ArchivedZeroCopyGraph, Node, NodeKind, ZeroCopyGraph};
use gnx_core::pool::StringPool;
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
    let mut pool = StringPool::new();
    let name_ref = pool.add("mmap_func");
    let uid_ref = pool.add("Function:test.ts:mmap_func");

    let graph = ZeroCopyGraph {
        magic: *b"GNX-RS\0\0",
        fingerprint: [1; 32],
        string_pool: pool.bytes,
        files: vec![],
        nodes: vec![Node {
            uid: uid_ref,
            name: name_ref,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (1, 0, 10, 0),
        }],
        edges: vec![],
        out_offsets: vec![0, 0],
        in_offsets: vec![0, 0],
        in_edge_idx: vec![],
        name_index: vec![0],
    };

    // Use rkyv::to_bytes for rkyv 0.8.x
    let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();

    let mut file = File::create(&file_path).unwrap();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();

    // 2. Mmap and Read (Zero-Copy)
    let file = File::open(&file_path).unwrap();
    let mmap = unsafe { Mmap::map(&file).unwrap() };

    let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&mmap).unwrap();

    assert_eq!(archived.fingerprint, [1; 32]);

    let first_node = &archived.nodes[0];
    assert_eq!(first_node.kind, gnx_core::graph::NodeKind::Function);

    let resolved_name = first_node.name.resolve(&archived.string_pool);
    assert_eq!(resolved_name, "mmap_func");
}
