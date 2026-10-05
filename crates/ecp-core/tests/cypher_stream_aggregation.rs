use ecp_core::cypher::{self, Value};
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
use ecp_core::session::{OverlayFileInput, OverlaySymbol, OverlayView};
use std::path::Path;

fn fixture() -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let a = fx.func("src/clean.rs", "a");
    let b = fx.func("src/dirty.rs", "b");
    let c = fx.func("src/dirty.rs", "c");
    fx.edge(a, b, RelType::Calls);
    fx.edge(a, c, RelType::References);
    fx.edge(b, c, RelType::Calls);
    fx.edge(a, b, RelType::Calls);
    fx.into_bytes()
}

#[test]
fn test_execute_aggregate_queries_return_pinned_rows() {
    let bytes = fixture();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let queries = [
        "MATCH ()-[r]->() RETURN count(r)",
        "MATCH ()-[r]->() WHERE type(r) = 'References' RETURN count(r)",
        "MATCH (a)-[r]->(b) RETURN type(r), count(*)",
        "MATCH (a:Function) OPTIONAL MATCH (a)-[r:Calls]->(b) RETURN a.name, count(r), count(*)",
        "MATCH (a)-[r]->(b) RETURN count(DISTINCT b), collect(DISTINCT b.name)",
        "MATCH (a)-[r]->(b) WITH type(r) AS t, count(*) AS n WHERE n > 1 RETURN t, n",
        "MATCH (a:Function {name:'missing'}) WITH count(*) AS n RETURN n",
        "MATCH (a)-[r]->(b) WITH b AS x RETURN count(DISTINCT x)",
        "MATCH (a:Function) MATCH (a)-[r]->(b) RETURN a.name, count(r)",
        "MATCH (a)-[r]->(b) RETURN type(r) AS t, count(*) AS n ORDER BY n DESC SKIP 1 LIMIT 1",
        "MATCH (a:Function {name:'missing'}) RETURN count(*), sum(a.startLine), collect(a.name)",
        "MATCH (a)-[:Calls]->(b)-[:Calls]->(c) RETURN collect(a.name), count(*)",
        "MATCH (a)-[:Calls*0..2]->(b) RETURN collect(b.name), count(*)",
        "MATCH (a:Function {name:'b'}) OPTIONAL MATCH (c)-[:Calls]->(a) RETURN count(c)",
        "MATCH (a:Function), (b:Function) RETURN count(a), count(b)",
        "MATCH (a)-[r]->(b) RETURN b, count(*)",
    ];
    // Pinned results independently protect aggregate value semantics.
    use Value::{Int, List, Str};
    let text = |s: &str| Str(s.into());
    let expected = [
        vec![vec![Int(4)]],
        vec![vec![Int(1)]],
        vec![
            vec![text("Calls"), Int(3)],
            vec![text("References"), Int(1)],
        ],
        vec![
            vec![text("a"), Int(2), Int(2)],
            vec![text("b"), Int(1), Int(1)],
            vec![text("c"), Int(0), Int(1)],
        ],
        vec![vec![Int(2), List(vec![text("b"), text("c")])]],
        vec![vec![text("Calls"), Int(3)]],
        vec![],
        vec![vec![Int(2)]],
        vec![vec![text("a"), Int(3)], vec![text("b"), Int(1)]],
        vec![vec![text("References"), Int(1)]],
        vec![vec![Int(0), Int(0), List(vec![])]],
        vec![vec![List(vec![text("a"), text("a")]), Int(2)]],
        vec![vec![
            List(vec![
                text("a"),
                text("b"),
                text("c"),
                text("b"),
                text("c"),
                text("c"),
            ]),
            Int(6),
        ]],
        vec![vec![Int(2)]],
        vec![vec![Int(3), Int(3)]],
        vec![
            vec![text("b"), text("Function"), text("src/dirty.rs"), Int(2)],
            vec![text("c"), text("Function"), text("src/dirty.rs"), Int(2)],
        ],
    ];
    assert_eq!(queries.len(), expected.len());
    for (query, expected) in queries.into_iter().zip(expected) {
        let result =
            cypher::execute(&cypher::parse(query).unwrap(), graph, None, Path::new(".")).unwrap();
        assert_eq!(result.rows, expected, "{query}");
    }
}

#[test]
fn test_execute_aggregate_over_overlay_counts_virtual_and_base_edges() {
    let bytes = fixture();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let view = OverlayView::build(
        graph,
        &[OverlayFileInput {
            rel_path: "src/dirty.rs".into(),
            symbols: vec![
                OverlaySymbol {
                    name: "b".into(),
                    kind: NodeKind::Function,
                    owner_class: None,
                    start_line: 1,
                    end_line: 2,
                    start_column: 0,
                    end_column: 0,
                    calls: vec!["a".into()],
                },
                OverlaySymbol {
                    name: "virtual".into(),
                    kind: NodeKind::Function,
                    owner_class: None,
                    start_line: 3,
                    end_line: 4,
                    start_column: 0,
                    end_column: 0,
                    calls: vec!["a".into()],
                },
            ],
            imports: vec![],
        }],
    )
    .unwrap();
    let query = "MATCH (a)-[r]->(b) RETURN type(r), count(*), count(DISTINCT a)";
    let result = cypher::execute(
        &cypher::parse(query).unwrap(),
        graph,
        Some(&view),
        Path::new("."),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Str("Calls".into()),
            Value::Int(4),
            Value::Int(3)
        ]]
    );
}

#[test]
fn test_execute_aggregate_groups_rows_by_file_path() {
    let mut fx = GraphFixture::new();
    // Each extension supplies one distinct file-path group; parser language support is not under test.
    for extension in [
        "ts", "js", "py", "java", "kt", "cs", "go", "rs", "php", "rb", "swift", "c", "cpp", "dart",
    ] {
        let path = format!("src/sample.{extension}");
        let caller = fx.func(&path, "caller");
        let target = fx.func(&path, "target");
        fx.edge(caller, target, RelType::Calls);
    }
    let bytes = fx.into_bytes();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let query = "MATCH (a)-[r:Calls]->(b) RETURN a.filePath, count(r), collect(b.name)";
    let result =
        cypher::execute(&cypher::parse(query).unwrap(), graph, None, Path::new(".")).unwrap();
    assert_eq!(result.rows.len(), 14);
    for row in &result.rows {
        assert_eq!(
            &row[1..],
            &[
                Value::Int(1),
                Value::List(vec![Value::Str("target".into())])
            ]
        );
    }
}

#[test]
fn test_execute_later_where_error_precedes_projection_error() {
    let bytes = fixture();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let query = "MATCH (a:Function), (b:Function) WHERE EXISTS { (a)-[:Calls*0..1]->(x) } RETURN missing, count(*)";
    let error =
        cypher::execute(&cypher::parse(query).unwrap(), graph, None, Path::new(".")).unwrap_err();
    assert!(error.to_string().contains("needs at least one"));
}
