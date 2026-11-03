//! One test binary for this crate's integration tests: one link
//! instead of one per file. A new `tests/<name>.rs` needs `mod <name>;` here.

mod atomic_write;
mod atomic_write_concurrent;
mod commit_dirname;
mod commit_meta;
mod concurrency_registry_writers;
mod concurrency_string_pool_intern;
mod config_group;
mod cypher_conformance;
mod cypher_stream_aggregation;
mod cypher_uid_migration;
mod cypher_where_label_test;
mod daemon;
mod file_category_routing_agree;
mod graph_schema;
mod mmap_test;
mod peer_concern;
mod peer_inbox;
mod peer_registry;
mod peer_retention;
mod registry_audit;
mod registry_lifecycle;
mod registry_lock;
mod registry_path;
mod registry_store;
mod repo_meta;
mod session_meta;
mod session_overlay_symbols;
mod uid_canonical;
mod uid_collision_hint_render;

/// Stands for: a test file outside every target never runs, and nothing
/// reports it. Each `tests/*.rs` is a `mod` here or its own `[[test]]`.
#[test]
fn test_it_every_test_file_is_registered_in_one_target() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = include_str!("it.rs");
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    let mut missing: Vec<String> = std::fs::read_dir(dir.join("tests"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".rs").map(str::to_owned))
        .filter(|stem| stem != "it")
        .filter(|stem| {
            !root
                .lines()
                .any(|l| l.trim_start_matches("pub ") == format!("mod {stem};"))
                && !manifest
                    .lines()
                    .any(|l| l.trim() == format!("path = \"tests/{stem}.rs\""))
        })
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "add `mod <name>;` to tests/it.rs for: {missing:?}"
    );
}
