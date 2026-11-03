//! A file whose name holds a `\` is a file apart from the `/` path on Unix:
//! the strict-parity helper keys it by its own name, so a dirty `x\y.py` is
//! neither reported dropped nor read back under `x/y.py`.
#![cfg(unix)]

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use strict_parity::{run, Edit, Scenario};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// Stands for: the helper compares OS paths without turning `\` into `/`,
/// so a Unix file named `x\y.py` is written and read back as that name.
#[test]
fn test_run_backslash_file_name_python_is_not_reported_dropped() {
    let observed = run(&Scenario {
        name: "backslash_path_python",
        owner: OWNER,
        files: &[("x\\y.py", "def f():\n    pass\n")],
        edits: &[Edit::Write("x\\y.py", "def f():\n    return 1\n")],
    });
    assert!(observed.dropped.is_empty(), "{:?}", observed.dropped);
}

/// Stands for: `rust_sources` keys a Unix file `x\y.rs` by that name, not
/// `x/y.rs`, which the view would read as another file.
#[test]
fn test_run_backslash_file_name_rust_source_keeps_its_own_key() {
    let observed = run(&Scenario {
        name: "backslash_path_rust",
        owner: OWNER,
        files: &[("x\\y.rs", "pub fn f() -> i32 {\n    1\n}\n")],
        edits: &[Edit::Write("x\\y.rs", "pub fn f() -> i32 {\n    2\n}\n")],
    });
    assert!(observed.sources.contains_key("x\\y.rs"));
    assert!(!observed.sources.contains_key("x/y.rs"));
    assert!(observed.dropped.is_empty(), "{:?}", observed.dropped);
}
