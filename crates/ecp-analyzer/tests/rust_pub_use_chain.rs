use ecp_analyzer::rust::module_tree::RustWorkspaceModTree;
use std::fs;
use tempfile::TempDir;

fn make_tree(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (rel, content) in files {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }
    dir
}

// ── 1. Single-hop `pub use internal::Widget` ────────────────────────────────

#[test]
fn single_hop_pub_use_resolves_to_definition_file() {
    let dir = make_tree(&[
        ("Cargo.toml", "[package]\nname = \"crate-a\"\n"),
        ("src/lib.rs", "mod internal;\npub use internal::Widget;\n"),
        ("src/internal.rs", "pub struct Widget {}\n"),
    ]);
    let tree = RustWorkspaceModTree::build(dir.path());

    // `crate-a::Widget` is re-exported from lib.rs → must resolve to internal.rs.
    let r = tree.resolve_fqn("crate_a::Widget", "src/lib.rs", dir.path());
    let r = r.expect("single-hop pub use should resolve");
    assert!(
        r.file.ends_with("internal.rs"),
        "expected internal.rs, got {}",
        r.file
    );
    assert_eq!(r.item_name, "Widget");
}

// ── 2. Multi-hop re-export (≥2 hops) ────────────────────────────────────────

#[test]
fn multi_hop_pub_use_resolves_to_original_definition() {
    // crate-a/src/deep/nested.rs  → defines W
    // crate-a/src/deep/mod.rs     → pub use nested::W
    // crate-a/src/lib.rs          → pub mod deep; pub use deep::W
    // Resolution of `crate-a::W` must reach nested.rs.
    let dir = make_tree(&[
        ("Cargo.toml", "[package]\nname = \"crate-a\"\n"),
        ("src/lib.rs", "pub mod deep;\npub use deep::W;\n"),
        ("src/deep/mod.rs", "pub mod nested;\npub use nested::W;\n"),
        ("src/deep/nested.rs", "pub struct W {}\n"),
    ]);
    let tree = RustWorkspaceModTree::build(dir.path());

    let r = tree.resolve_fqn("crate_a::W", "src/lib.rs", dir.path());
    let r = r.expect("multi-hop pub use should resolve");
    assert!(
        r.file.ends_with("nested.rs"),
        "expected nested.rs, got {}",
        r.file
    );
    assert_eq!(r.item_name, "W");
}

// ── 3. Renamed re-export (`pub use path::Widget as Gadget`) ─────────────────

#[test]
fn renamed_pub_use_resolves_to_original_definition() {
    // crate-a/src/internal.rs → defines Widget
    // crate-a/src/lib.rs      → pub use internal::Widget as Gadget
    // Consumer uses `crate_a::Gadget` → should land on Widget in internal.rs.
    let dir = make_tree(&[
        ("Cargo.toml", "[package]\nname = \"crate-a\"\n"),
        (
            "src/lib.rs",
            "mod internal;\npub use internal::Widget as Gadget;\n",
        ),
        ("src/internal.rs", "pub struct Widget {}\n"),
    ]);
    let tree = RustWorkspaceModTree::build(dir.path());

    let r = tree.resolve_fqn("crate_a::Gadget", "src/lib.rs", dir.path());
    let r = r.expect("renamed pub use should resolve");
    assert!(
        r.file.ends_with("internal.rs"),
        "expected internal.rs, got {}",
        r.file
    );
    // item_name reflects the original exported name at the source, not alias.
    assert_eq!(r.item_name, "Widget");
}

// ── 4. Glob re-export (`pub use foo::*`) ─────────────────────────────────────

#[test]
fn glob_pub_use_resolves_symbol_from_target_module() {
    // crate-a/src/utils.rs → defines Util
    // crate-a/src/lib.rs   → pub mod utils; pub use utils::*
    // Resolving `crate_a::Util` should land on utils.rs.
    let dir = make_tree(&[
        ("Cargo.toml", "[package]\nname = \"crate-a\"\n"),
        ("src/lib.rs", "pub mod utils;\npub use utils::*;\n"),
        ("src/utils.rs", "pub fn Util() {}\n"),
    ]);
    let tree = RustWorkspaceModTree::build(dir.path());

    // The glob re-export doesn't enumerate specific items, so the symbol
    // `Util` must be resolved by walking through the `utils::*` glob entry
    // and looking up `Util` in utils.rs.
    let r = tree.resolve_fqn("crate_a::Util", "src/lib.rs", dir.path());
    let r = r.expect("glob pub use should resolve symbol");
    assert!(
        r.file.ends_with("utils.rs"),
        "expected utils.rs, got {}",
        r.file
    );
    assert_eq!(r.item_name, "Util");
}

// ── 5. Cycle detection — must not infinite-loop ───────────────────────────────

#[test]
fn cycle_in_pub_use_chain_does_not_infinite_loop() {
    // Both modules re-export from each other — a cycle.
    // a.rs: pub use b::Foo;
    // b.rs: pub use a::Foo;
    // lib.rs: pub mod a; pub mod b; pub use a::Foo;
    // Expected: resolves without panic, returns None or lands on a.rs/b.rs
    // (whichever has the deepest non-reexport hit before MAX_DEPTH fires).
    let dir = make_tree(&[
        ("Cargo.toml", "[package]\nname = \"cyc\"\n"),
        ("src/lib.rs", "pub mod a;\npub mod b;\npub use a::Foo;\n"),
        ("src/a.rs", "pub use crate::b::Foo;\n"),
        ("src/b.rs", "pub use crate::a::Foo;\n"),
    ]);
    let tree = RustWorkspaceModTree::build(dir.path());

    // Must not panic or infinite-loop; return value is either None or Some.
    let _r = tree.resolve_fqn("cyc::Foo", "src/lib.rs", dir.path());
    // No assertion on value — cycle terminates by max-depth or visited-set.
}

// ── 6. Direct-path resolution still works (regression) ───────────────────────

#[test]
fn direct_fqn_without_pub_use_still_resolves() {
    let dir = make_tree(&[
        ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
        ("src/lib.rs", "pub mod foo;\n"),
        ("src/foo.rs", "pub fn bar() {}\n"),
    ]);
    let tree = RustWorkspaceModTree::build(dir.path());
    let r = tree.resolve_fqn("crate::foo::bar", "src/lib.rs", dir.path());
    assert!(r.is_some(), "direct FQN resolution must still work");
    let r = r.unwrap();
    assert!(
        r.file.ends_with("foo.rs"),
        "expected foo.rs, got {}",
        r.file
    );
    assert_eq!(r.item_name, "bar");
}

// ── 7. pub(crate) use — restricted visibility still indexed ──────────────────

#[test]
fn pub_crate_use_is_indexed_for_intra_crate_resolution() {
    let dir = make_tree(&[
        ("Cargo.toml", "[package]\nname = \"mycrate\"\n"),
        (
            "src/lib.rs",
            "mod internal;\npub(crate) use internal::Helper;\n",
        ),
        ("src/internal.rs", "pub struct Helper {}\n"),
    ]);
    let tree = RustWorkspaceModTree::build(dir.path());

    let r = tree.resolve_fqn("crate::Helper", "src/lib.rs", dir.path());
    let r = r.expect("pub(crate) use should be indexed");
    assert!(
        r.file.ends_with("internal.rs"),
        "expected internal.rs, got {}",
        r.file
    );
}
