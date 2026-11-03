//! One test binary for this crate's integration tests: one link
//! instead of one per file. A new `tests/<name>.rs` needs `mod <name>;` here.

mod common;

mod group_tools;
mod peers_tools;
mod schema_tools;
mod server_smoke_test;
mod spawn_test;
mod telemetry_test;

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
            !root.lines().any(|l| l == format!("mod {stem};"))
                && !manifest.contains(&format!("path = \"tests/{stem}.rs\""))
        })
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "add `mod <name>;` to tests/it.rs for: {missing:?}"
    );
}
