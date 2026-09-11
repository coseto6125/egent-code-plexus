//! Keep every locked LRU implementation on the panic-safety fix.

use std::path::Path;

#[test]
fn test_locked_lru_versions_include_panic_safety_fix() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/ecp-cli sits two levels under the workspace root");
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("read Cargo.lock");
    let doc: toml::Table = toml::from_str(&lock).expect("parse Cargo.lock");
    let packages = doc["package"].as_array().expect("Cargo.lock packages");
    let mut checked = 0;
    for package in packages {
        if package["name"].as_str() != Some("lru") {
            continue;
        }
        let version = package["version"].as_str().expect("package version");
        let parts: Vec<u64> = version
            .split('.')
            .map(|part| part.parse().expect("stable numeric LRU version"))
            .collect();
        assert_eq!(parts.len(), 3, "unexpected lru version: {version}");
        // https://rustsec.org/advisories/RUSTSEC-2026-0253.html
        assert!(
            parts.as_slice() >= [0, 18, 2].as_slice(),
            "lru {version} lacks the RUSTSEC-2026-0253 panic-safety fix"
        );
        checked += 1;
    }
    assert!(checked > 0, "no locked lru; this guard may now be obsolete");
}
