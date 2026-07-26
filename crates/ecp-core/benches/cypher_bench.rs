use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use ecp_core::cypher::{execute, parse};
use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use std::hint::black_box;
use std::path::Path;

const FANOUT: usize = 1_024;

/// Star graph: one caller, `fanout` callees, one `Calls` edge each — built
/// through the same assembler the indexer uses, so the query under
/// measurement takes production's index paths (kind CSR, name index) rather
/// than the empty-index fallbacks.
fn build_fanout_graph(fanout: usize) -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let caller = fx.func("src/compact.rs", "caller");
    fx.span(caller, (0, 0, 6, 0));
    for index in 0..fanout {
        let callee = fx.func("src/compact.rs", &format!("callee_{index:04}"));
        fx.span(callee, (index as u32 + 1, 0, index as u32 + 2, 0));
        fx.edge(caller, callee, RelType::Calls);
    }
    fx.into_bytes()
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
