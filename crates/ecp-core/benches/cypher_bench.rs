use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use ecp_core::cypher::{execute, parse};
use ecp_core::graph::{Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::pool::{StrRef, StringPool};
use std::hint::black_box;
use std::path::Path;

const FANOUT: usize = 1_024;

fn build_fanout_graph(fanout: usize) -> Vec<u8> {
    let mut pool = StringPool::new();
    let caller_name = pool.add("caller");
    let file_path = pool.add("src/compact.rs");
    let reason = pool.add("ast-call");

    let mut nodes = Vec::with_capacity(fanout + 1);
    nodes.push(Node {
        uid: ecp_core::uid::compute(NodeKind::Function, "src/compact.rs", None, "caller"),
        name: caller_name,
        file_idx: 0,
        kind: NodeKind::Function,
        span: (0, 0, 6, 0),
        community_id: 0,
        owner_class: StrRef::default(),
        content_hash: 0,
    });

    let mut edges = Vec::with_capacity(fanout);
    for index in 0..fanout {
        let name = format!("callee_{index:04}");
        let name_ref = pool.add(&name);
        nodes.push(Node {
            uid: ecp_core::uid::compute(NodeKind::Function, "src/compact.rs", None, &name),
            name: name_ref,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (index as u32 + 1, 0, index as u32 + 2, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        });
        edges.push(Edge {
            source: 0,
            target: index as u32 + 1,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason,
        });
    }

    let mut in_offsets = Vec::with_capacity(fanout + 2);
    in_offsets.extend([0, 0]);
    in_offsets.extend(1..=fanout as u32);

    let graph = ZeroCopyGraph {
        string_pool: pool.bytes,
        files: vec![File {
            path: file_path,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        }],
        nodes,
        edges,
        out_offsets: std::iter::once(0)
            .chain(std::iter::repeat_n(fanout as u32, fanout + 1))
            .collect(),
        in_offsets,
        in_edge_idx: (0..fanout as u32).collect(),
        process_start: fanout as u32 + 1,
        ..ZeroCopyGraph::default()
    };
    rkyv::to_bytes::<rkyv::rancor::Error>(&graph)
        .expect("archive benchmark graph")
        .to_vec()
}

fn benchmark_cypher_short_string_projection(criterion: &mut Criterion) {
    let bytes = build_fanout_graph(FANOUT);
    let graph = rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
        .expect("access benchmark graph");
    let query = parse(
        "MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, b.name, b.kind, b.filePath, r.rel_type",
    )
    .expect("parse benchmark query");
    let expected_rows = execute(&query, graph, None, Path::new("."))
        .expect("execute benchmark query")
        .rows
        .len();
    assert_eq!(expected_rows, FANOUT);

    let mut group = criterion.benchmark_group("cypher_short_string_projection");
    group.throughput(Throughput::Elements(FANOUT as u64));
    group.bench_function("fanout_1024", |bencher| {
        bencher.iter(|| {
            black_box(
                execute(black_box(&query), graph, None, Path::new("."))
                    .expect("execute benchmark query"),
            )
        });
    });
    group.finish();
}

criterion_group!(benches, benchmark_cypher_short_string_projection);
criterion_main!(benches);
