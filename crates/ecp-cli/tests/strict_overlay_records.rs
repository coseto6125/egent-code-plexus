//! The record outputs of the incremental overlay (F7a-2): for a dirty file,
//! the overlay view rebuilds its path literals, SQL table references,
//! transaction scopes, schema fields and event topics as a fresh full index
//! of the edited tree has them. Each output runs over every language whose
//! parser emits its record, through the production index, fragment write
//! and read (`common/strict_parity.rs`).
//!
//! Each check first asserts that the full index has an edge of the output,
//! so a fixture that emits nothing fails instead of passing empty, and that
//! the edit changes the output's lines, so the overlay cannot pass by
//! keeping the base index's lines.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::analyzer::record_nodes::PATH_LITERAL_SINK_REASONS;
use ecp_core::cypher::{execute, parse, Value};
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::OverlayView;
use std::path::Path;
use strict_parity::{snapshot, Edit, Scenario, Snapshot};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// One record output: the node kinds it makes, and the edge kinds it makes
/// with the kind of node each one ends at (`HasProperty` also ends at class
/// members, which the overlay keeps as indexed).
struct Output {
    name: &'static str,
    nodes: &'static [&'static str],
    edges: &'static [(&'static str, &'static str)],
}

const PATH_LITERALS: Output = Output {
    name: "path literals",
    nodes: &["PathLiteral"],
    edges: &[("UsesPathLiteral", "PathLiteral")],
};

const SQL: Output = Output {
    name: "SQL",
    nodes: &[],
    edges: &[("QueriesTable", "Class")],
};

const TX_SCOPES: Output = Output {
    name: "transaction scopes",
    nodes: &["TransactionScope"],
    edges: &[("OpensTxScope", "TransactionScope")],
};

const SCHEMA_FIELDS: Output = Output {
    name: "schema fields",
    nodes: &["SchemaField"],
    edges: &[("HasProperty", "SchemaField")],
};

const EVENT_TOPICS: Output = Output {
    name: "event topics",
    nodes: &["EventTopic"],
    edges: &[("Publishes", "EventTopic"), ("Subscribes", "EventTopic")],
};

impl Output {
    /// The output's lines of `snap`, each prefixed with its section, sorted.
    fn lines(&self, snap: &Snapshot) -> Vec<String> {
        let node = |line: &str| {
            self.nodes
                .iter()
                .any(|k| line.starts_with(&format!("{k} ")))
        };
        let edge = |line: &str| {
            self.edges.iter().any(|(rel, target)| {
                line.starts_with(&format!("{rel} ")) && line.contains(&format!(" -> {target} "))
            })
        };
        let mut out: Vec<String> = (snap.identity.iter().filter(|l| node(l)))
            .map(|l| format!("identity {l}"))
            .chain((snap.parent.iter().filter(|l| node(l))).map(|l| format!("parent {l}")))
            .chain((snap.edge.iter().filter(|l| edge(l))).map(|l| format!("edge {l}")))
            .collect();
        out.sort();
        out
    }
}

/// `source` one line lower: every span moves, no token changes. A PHP file
/// keeps `<?php` on its first line, so the blank line goes after it.
fn shifted(source: &str) -> String {
    match source.strip_prefix("<?php\n") {
        Some(rest) => format!("<?php\n\n{rest}"),
        None => format!("\n{source}"),
    }
}

/// The overlay's lines of `output` after `edits` are the full index's.
/// `present` says whether the edited tree still holds the output: then the
/// full index must have an edge of it, else it must have none while the
/// base index has one. Either way the edit must change the lines.
fn assert_replayed(
    output: &Output,
    case: &str,
    files: &[(&str, &str)],
    edits: &[Edit<'_>],
    present: bool,
) {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files,
        edits,
    });
    let (full, overlay) = observed.snapshots();
    let base = snapshot(&MergedGraph::new(observed.base(), None));
    let name = output.name;
    let full_lines = output.lines(&full);
    let base_lines = output.lines(&base);
    let has_edge = |lines: &[String]| lines.iter().any(|l| l.starts_with("edge "));
    if present {
        assert!(
            has_edge(&full_lines),
            "{case}: the full index makes no {name} edge, so the fixture tests nothing"
        );
    } else {
        assert!(!has_edge(&full_lines), "{case}: {full_lines:#?}");
        assert!(has_edge(&base_lines), "{case}: the base has no {name} edge");
    }
    assert_ne!(
        base_lines, full_lines,
        "{case}: the edit changes no {name} line, so the test cannot fail"
    );
    assert_eq!(output.lines(&overlay), full_lines, "{case}: {name}");
}

/// Each `(case, path, source)` with its source moved one line down.
fn check_shifted(output: &Output, fixtures: &[(&str, &str, &str)], support: &[(&str, &str)]) {
    for &(case, path, source) in fixtures {
        let files: Vec<(&str, &str)> = support.iter().copied().chain([(path, source)]).collect();
        let edited = shifted(source);
        assert_replayed(output, case, &files, &[Edit::Write(path, &edited)], true);
    }
}

// ── path literals ───────────────────────────────────────────────────────────

/// One file per language whose path-literal walker binds a literal inside a
/// function (`ecp-analyzer/tests/<lang>_path_literals.rs`).
const PATH_LITERAL_FIXTURES: &[(&str, &str, &str)] = &[
    ("c", "src/load.c", "\n#include <stdio.h>\n\nFILE *load(void) {\n    return fopen(\"session_meta.json\", \"r\");\n}\n"),
    ("csharp", "src/Loader.cs", "\nusing System.IO;\nclass Loader {\n    string Load() {\n        return File.ReadAllText(\"session_meta.json\");\n    }\n}\n"),
    ("cpp", "src/load.cpp", "\n#include <fstream>\n\nstd::ifstream load() {\n    return std::ifstream(\"session_meta.json\");\n}\n"),
    ("dart", "lib/load.dart", "\nimport 'dart:io';\n\nFuture<String> load() async {\n  return await File('session_meta.json').readAsString();\n}\n"),
    ("go", "load.go", "\npackage main\n\nimport \"os\"\n\nfunc Load() ([]byte, error) {\n    return os.ReadFile(\"session_meta.json\")\n}\n"),
    ("java", "src/Loader.java", "\nimport java.nio.file.Files;\nimport java.nio.file.Paths;\nclass Loader {\n    String load() throws Exception {\n        return Files.readString(Paths.get(\"session_meta.json\"));\n    }\n}\n"),
    ("javascript", "src/load.js", "\nconst fs = require(\"fs\");\nfunction load() {\n    return fs.readFileSync(\"session_meta.json\", \"utf-8\");\n}\n"),
    ("kotlin", "src/Load.kt", "\nimport java.io.File\nfun load(): String {\n    return File(\"session_meta.json\").readText()\n}\n"),
    ("php", "src/load.php", "<?php\nfunction load() {\n    return file_get_contents(\"session_meta.json\");\n}\n"),
    ("python", "app/load.py", "\ndef load():\n    with open(\"session_meta.json\") as f:\n        return f.read()\n"),
    ("ruby", "lib/loader.rb", "\nclass Loader\n  def load\n    File.read(\"session_meta.json\")\n  end\nend\n"),
    ("rust", "src/load.rs", "\nuse std::fs;\nfn load() -> std::io::Result<String> {\n    fs::read_to_string(\"session_meta.json\")\n}\n"),
    ("swift", "Sources/App/Load.swift", "\nimport Foundation\n\nfunc load() throws -> String {\n    return try String(contentsOfFile: \"session_meta.json\")\n}\n"),
    ("typescript", "src/load.ts", "\nimport * as fs from \"fs\";\nfunction load(): string {\n    return fs.readFileSync(\"session_meta.json\", \"utf-8\");\n}\n"),
];

#[test]
fn test_overlay_view_dirty_file_path_literals_match_reindex() {
    check_shifted(&PATH_LITERALS, PATH_LITERAL_FIXTURES, &[]);
}

/// The PHP dirty-file scenario of `strict_overlay_parity.rs`: the edit puts
/// a blank line above `require_once`, so its literal moves from line 3 to
/// line 4. The overlay shows it at line 4 with its one `File` edge, and two
/// refreshes of the session build one view.
#[test]
fn test_overlay_view_dirty_php_path_literal_line_updated() {
    let lib = "<?php\n\nfunction helper()\n{\n    return 1;\n}\n";
    let main = "<?php\n\nrequire_once __DIR__ . '/Lib.php';\n\nfunction run()\n{\n    return helper();\n}\n";
    let lib_edited = format!("{lib}\nfunction extra()\n{{\n    return helper();\n}}\n");
    let main_edited = main.replacen('\n', "\n\n", 1);
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files: &[("src/Lib.php", lib), ("src/main.php", main)],
        edits: &[
            Edit::Write("src/Lib.php", &lib_edited),
            Edit::Write("src/main.php", &main_edited),
        ],
    });
    let (full, overlay) = observed.snapshots();
    let literal = |snap: &Snapshot| -> Vec<String> {
        snap.identity
            .iter()
            .filter(|l| l.starts_with("PathLiteral src/main.php:"))
            .cloned()
            .collect()
    };
    let full_literal = literal(&full);
    assert_eq!(full_literal.len(), 1, "{:#?}", full.identity);
    assert!(
        full_literal[0].starts_with("PathLiteral src/main.php:4 /Lib.php #"),
        "{full_literal:?}"
    );
    assert_eq!(literal(&overlay), full_literal);
    assert_eq!(PATH_LITERALS.lines(&overlay), PATH_LITERALS.lines(&full));
    let lines = observed.lines();
    assert!(
        lines.iter().all(|l| !l.contains("PathLiteral")),
        "{lines:#?}"
    );

    let first = observed.view().expect("a dirty tree has a view");
    let second = observed.view().expect("a dirty tree has a view");
    assert_eq!(first, second, "two refreshes build one view");
    assert_eq!(first.to_cache_bytes(), second.to_cache_bytes());
}

/// A renamed file is a new path: its literal's edge starts at the new
/// `File` node, as a reindex gives it.
#[test]
fn test_overlay_view_renamed_file_path_literal_sources_new_file_node() {
    let (_, path, source) = PATH_LITERAL_FIXTURES[13];
    assert_replayed(
        &PATH_LITERALS,
        "typescript rename",
        &[(path, source)],
        &[Edit::Rename(path, "src/moved.ts")],
        true,
    );
}

/// An emptied and a removed file lose their literals and edges.
#[test]
fn test_overlay_view_emptied_or_removed_file_drops_path_literals() {
    let (_, path, source) = PATH_LITERAL_FIXTURES[13];
    let keep = ("src/keep.ts", "export const k = 1;\n");
    for (case, edit) in [
        ("emptied", Edit::Write(path, "")),
        ("removed", Edit::Delete(path)),
    ] {
        assert_replayed(
            &PATH_LITERALS,
            case,
            &[(path, source), keep],
            &[edit],
            false,
        );
    }
}

/// Every reason the extractors render is in the list the cached view
/// stores reasons by. A reason missing from it would keep the view out of
/// the cache.
#[test]
fn test_path_literal_sink_reasons_list_every_rendered_reason() {
    use ecp_analyzer::path_literal::{sink_reason, SinkConfidence, SinkKind};
    let kinds = [
        SinkKind::Read,
        SinkKind::Write,
        SinkKind::OpenRead,
        SinkKind::OpenWrite,
        SinkKind::Join,
        SinkKind::ExtChange,
        SinkKind::Free,
    ];
    let rendered: Vec<String> = kinds
        .into_iter()
        .flat_map(|kind| {
            [SinkConfidence::High, SinkConfidence::Medium].map(|conf| sink_reason(kind, conf))
        })
        .collect();
    assert_eq!(rendered, PATH_LITERAL_SINK_REASONS);
}

// ── SQL ─────────────────────────────────────────────────────────────────────

const SCHEMA_SQL: (&str, &str) = (
    "db/schema.sql",
    "CREATE TABLE channels (id BIGINT PRIMARY KEY, slug TEXT);\n",
);

/// One raw-SQL call per language, against the `channels` table that
/// [`SCHEMA_SQL`] declares.
const SQL_FIXTURES: &[(&str, &str, &str)] = &[
    ("c", "src/channel.c", "void list_channels(PGconn *conn) {\n    PQexec(conn, \"SELECT id FROM channels WHERE org_id = $1\");\n}\n"),
    ("csharp", "src/ChannelRepo.cs", "class ChannelRepo {\n  void ListChannels() {\n    conn.Query<Channel>(\"SELECT id FROM channels WHERE org_id = @orgId\");\n  }\n}\n"),
    ("cpp", "src/channel.cpp", "void listChannels(Conn& c) {\n    c.exec(\"SELECT id FROM channels WHERE org_id = $1\");\n}\n"),
    ("dart", "lib/channel_repo.dart", "void listChannels(db) {\n  db.query(\"SELECT id FROM channels WHERE org_id = ?\");\n}\n"),
    ("go", "store.go", "package store\nfunc ListChannels(db *sql.DB) {\n  db.Query(`SELECT id, slug FROM channels WHERE org_id = $1`)\n}\n"),
    ("java", "src/ChannelDao.java", "class ChannelDao {\n  void listChannels() {\n    jdbcTemplate.query(\"SELECT id FROM channels WHERE org_id = ?\", rowMapper);\n  }\n}\n"),
    ("javascript", "src/api.js", "async function listChannels(client) {\n  return client.query(\"SELECT id FROM channels WHERE org_id = $1\");\n}\n"),
    ("kotlin", "src/ChannelDao.kt", "class ChannelDao {\n  fun listChannels() {\n    db.query(\"SELECT id FROM channels WHERE org_id = ?\")\n  }\n}\n"),
    ("php", "src/ChannelRepo.php", "<?php\nfunction renameChannel($pdo) {\n  $pdo->prepare(\"UPDATE channels SET slug = :s WHERE id = :id\");\n}\n"),
    ("python", "app/api.py", "def list_channels(pool):\n    return pool.fetch(\"SELECT id, slug FROM channels WHERE org_id = $1\")\n"),
    ("ruby", "lib/channel_repo.rb", "def create_channel(conn)\n  conn.exec_params(\"INSERT INTO channels (slug) VALUES ($1)\", [slug])\nend\n"),
    ("rust", "src/store.rs", "fn list_channels(pool: &Pool) {\n    sqlx::query(\"SELECT id, slug FROM channels WHERE org_id = $1\");\n}\n"),
    ("swift", "Sources/App/ChannelRepo.swift", "func listChannels(db: Database) {\n    db.query(\"SELECT id FROM channels WHERE org_id = ?\")\n}\n"),
    ("typescript", "src/api.ts", "async function listChannels(db) {\n  return db.query(\"SELECT id, slug FROM channels WHERE org_id = $1\");\n}\n"),
];

#[test]
fn test_overlay_view_dirty_file_sql_table_edges_match_reindex() {
    check_shifted(&SQL, SQL_FIXTURES, &[SCHEMA_SQL]);
}

/// A dirty file that names a table no file declares gets no edge, as in a
/// reindex, beside the edge to the declared one.
#[test]
fn test_overlay_view_sql_absent_table_gets_no_edge() {
    let (_, path, source) = SQL_FIXTURES[13];
    let edited = format!(
        "\n{source}async function listGhosts(db) {{\n  return db.query(\"SELECT id FROM ghosts\");\n}}\n"
    );
    assert_replayed(
        &SQL,
        "typescript absent table",
        &[SCHEMA_SQL, (path, source)],
        &[Edit::Write(path, &edited)],
        true,
    );
}

// ── transaction scopes ──────────────────────────────────────────────────────

/// One transactional callable per language whose detector emits a scope.
const TX_FIXTURES: &[(&str, &str, &str)] = &[
    ("python", "app/orders.py", "\nfrom django.db import transaction\n\n@transaction.atomic\ndef place_order():\n    pass\n\ndef list_orders():\n    pass\n"),
    ("java", "src/OrderService.java", "\nimport org.springframework.transaction.annotation.Transactional;\n\npublic class OrderService {\n    @Transactional\n    public void placeOrder() {}\n\n    public void listOrders() {}\n}\n"),
    ("kotlin", "src/OrderService.kt", "\nimport org.springframework.transaction.annotation.Transactional\n\nclass OrderService {\n    @Transactional\n    fun placeOrder() {}\n\n    fun listOrders() {}\n}\n"),
    ("csharp", "src/OrderService.cs", "\npublic class OrderService {\n    [Transactional]\n    public void PlaceOrder() {}\n\n    public void ListOrders() {}\n}\n"),
    ("php", "src/OrderService.php", "<?php\nclass OrderService {\n    #[Transactional]\n    public function placeOrder(): void {}\n\n    public function listOrders(): void {}\n}\n"),
    ("dart", "lib/user_dao.dart", "\nclass UserDao {\n  final AppDatabase db;\n  Future<void> createUser(String name) async {\n    await db.transaction(() async {\n      // insert user\n    });\n  }\n  Future<void> listUsers() async {}\n}\n"),
    ("ruby", "app/user_service.rb", "\nclass UserService\n  def create_user(attrs)\n    User.transaction do\n      user = User.create!(attrs)\n      AuditLog.record(user.id)\n    end\n  end\nend\n"),
    ("typescript", "src/user.service.ts", "\nexport class UserService {\n  @Transactional()\n  async createUser(data: string): Promise<void> { }\n\n  async listUsers(): Promise<void> { }\n}\n"),
    ("rust", "src/users.rs", "\n#[transaction]\npub async fn create_user(pool: &PgPool, data: UserDto) -> Result<User, Error> {\n    // ...\n}\n\npub fn list_users() {}\n"),
    ("go", "db/users.go", "package db\n\nimport \"database/sql\"\n\nfunc createUser(db *sql.DB) error {\n    tx, err := db.Begin()\n    if err != nil {\n        return err\n    }\n    defer tx.Rollback()\n    return tx.Commit()\n}\n\nfunc listUsers(db *sql.DB) {}\n"),
];

#[test]
fn test_overlay_view_dirty_file_tx_scopes_match_reindex() {
    check_shifted(&TX_SCOPES, TX_FIXTURES, &[]);
}

// ── schema fields ───────────────────────────────────────────────────────────

/// One schema type per language whose detector emits fields. The OpenAPI
/// detector is left out: its YAML routing is not verified here.
const SCHEMA_FIXTURES: &[(&str, &str, &str)] = &[
    ("python", "app/models.py", "from pydantic import BaseModel\n\nclass Item(BaseModel):\n    name: str\n    price: float\n"),
    ("typescript", "src/types.ts", "interface Product { name: string; price: number; }\n"),
    ("protobuf", "proto/user.proto", "\nsyntax = \"proto3\";\nmessage User {\n    string email = 1;\n}\n"),
];

#[test]
fn test_overlay_view_dirty_file_schema_fields_match_reindex() {
    check_shifted(&SCHEMA_FIELDS, SCHEMA_FIXTURES, &[]);
}

// ── event topics ────────────────────────────────────────────────────────────

/// One Kafka publish of `orders` per language whose detector emits topics.
const EVENT_FIXTURES: &[(&str, &str, &str)] = &[
    ("go", "producer.go", "\npackage main\n\nimport kafka \"github.com/segmentio/kafka-go\"\n\nfunc publishOrder(writer *kafka.Writer, ctx interface{}) {\n    writer.WriteMessages(ctx, kafka.Message{\n        Topic: \"orders\",\n        Value: []byte(\"hello\"),\n    })\n}\n"),
    ("java", "src/OrderService.java", "\nimport org.apache.kafka.clients.producer.KafkaProducer;\nimport org.apache.kafka.clients.producer.ProducerRecord;\n\npublic class OrderService {\n    public void publishOrder(KafkaProducer<String, String> producer, String data) {\n        producer.send(new ProducerRecord<>(\"orders\", data));\n    }\n}\n"),
    ("javascript", "src/producer.js", "\nimport { Kafka } from 'kafkajs';\n\nasync function publishOrder(data) {\n    const producer = kafka.producer();\n    await producer.send({ topic: 'orders', messages: [{ value: JSON.stringify(data) }] });\n}\n"),
    ("kotlin", "src/Producer.kt", "\nimport org.apache.kafka.clients.producer.KafkaProducer\nimport org.apache.kafka.clients.producer.ProducerRecord\n\nfun publishOrder(producer: KafkaProducer<String, String>, data: String) {\n    producer.send(ProducerRecord(\"orders\", data))\n}\n"),
    ("python", "app/producer.py", "\nfrom kafka import KafkaProducer\n\ndef publish_order(data):\n    p = KafkaProducer(bootstrap_servers=\"localhost:9092\")\n    p.send(\"orders\", b\"x\")\n"),
    ("rust", "src/producer.rs", "\nuse rdkafka::producer::{FutureProducer, FutureRecord};\n\nasync fn publish_order(producer: &FutureProducer) {\n    producer.send(\n        FutureRecord::to(\"orders\").payload(\"hello\"),\n        std::time::Duration::from_secs(0),\n    ).await;\n}\n"),
    ("typescript", "src/producer.ts", "\nimport { Kafka } from 'kafkajs';\n\nfunction publishOrder(data) {\n    const producer = kafka.producer();\n    producer.send({ topic: 'orders', messages: [{ value: JSON.stringify(data) }] });\n}\n"),
];

#[test]
fn test_overlay_view_dirty_file_event_topics_match_reindex() {
    check_shifted(&EVENT_TOPICS, EVENT_FIXTURES, &[]);
}

/// A new file that adds the only publisher of a topic gets the topic node
/// and its `Publishes` edge, as a reindex gives them.
#[test]
fn test_overlay_view_new_file_only_publisher_of_topic_matches_reindex() {
    let (_, path, source) = EVENT_FIXTURES[6];
    let keep = ("src/keep.ts", "export const k = 1;\n");
    assert_replayed(
        &EVENT_TOPICS,
        "typescript new publisher",
        &[keep],
        &[Edit::Write(path, source)],
        true,
    );
}

// ── function meta in cypher ─────────────────────────────────────────────────

/// The names column of `query` over `graph` with `view`, sorted.
fn names(graph: &ArchivedZeroCopyGraph, view: Option<&OverlayView>, query: &str) -> Vec<String> {
    let parsed = parse(query).unwrap_or_else(|err| panic!("parse {query:?}: {err:?}"));
    let result = execute(&parsed, graph, view, Path::new(".")).expect("query runs");
    let mut out: Vec<String> = result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::Str(s) => s.to_string(),
            other => panic!("expected a name, got {other:?}"),
        })
        .collect();
    out.sort();
    out
}

/// A new async function in a dirty TS file is async to cypher over the
/// overlay, as over a reindex.
#[test]
fn test_cypher_is_async_finds_new_async_function_in_dirty_file() {
    let lib = "export function helper(): number {\n  return 1;\n}\n";
    let main = "import { helper } from './lib';\n\nexport function run(): number {\n  return helper();\n}\n";
    let edited = format!(
        "{lib}\nexport async function fresh(): Promise<number> {{\n  return helper();\n}}\n"
    );
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files: &[("src/lib.ts", lib), ("src/main.ts", main)],
        edits: &[Edit::Write("src/lib.ts", &edited)],
    });
    let query = "MATCH (f:Function) WHERE f.isAsync RETURN f.name";
    let full = names(observed.full(), None, query);
    assert_eq!(full, ["fresh"], "the reindex reads `fresh` as async");
    let view = observed.view().expect("a dirty tree has a view");
    assert_eq!(names(observed.base(), Some(&view), query), full);
}
