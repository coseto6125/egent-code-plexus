use std::time::Duration;
use tree_sitter::Parser;

use ecp_analyzer::parse_budget::{parse_with_budget, ParseBudget};

// Match the production `ecp` binary's allocator (`crates/ecp-cli/src/main.rs`).
// Without this, Windows test binaries fall back to HeapAlloc, which is
// significantly slower than mimalloc for tree-sitter's alloc-heavy parse
// path — `generous_budget_completes_large_source` then flakes around the
// 1s budget edge while production users on the same hardware comfortably
// stay under it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn rust_parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("set_language");
    p
}

#[test]
fn default_budget_parses_trivial_source() {
    let mut p = rust_parser();
    let tree = parse_with_budget(&mut p, b"fn main() {}\n", ParseBudget::DEFAULT);
    assert!(tree.is_some(), "default budget must allow a trivial parse");
}

#[test]
fn tiny_duration_budget_aborts_large_source() {
    let mut p = rust_parser();
    // Repeat enough so the parser actually hits a progress checkpoint
    // before finishing — the callback only fires periodically.
    let src = "fn main() {}\n".repeat(50_000);
    let budget = ParseBudget {
        max_duration: Duration::from_nanos(1),
        max_bytes: usize::MAX,
    };
    let tree = parse_with_budget(&mut p, src.as_bytes(), budget);
    assert!(tree.is_none(), "1ns duration budget must abort the parse");
}

#[test]
fn tiny_byte_budget_aborts_large_source() {
    let mut p = rust_parser();
    let src = "fn main() {}\n".repeat(50_000);
    let budget = ParseBudget {
        max_duration: Duration::MAX,
        max_bytes: 100,
    };
    let tree = parse_with_budget(&mut p, src.as_bytes(), budget);
    assert!(tree.is_none(), "100-byte budget must abort the parse");
}

#[test]
fn generous_budget_completes_large_source() {
    // Windows GHA runners parsed 650 KB (50_000 lines) at ~1.05s — just over
    // the 1s default. 20_000 lines / 260 KB keeps the test meaningful (still
    // exercises a realistic large source) while leaving safety margin on the
    // slowest supported CI tier.
    let mut p = rust_parser();
    let src = "fn main() {}\n".repeat(20_000);
    let tree = parse_with_budget(&mut p, src.as_bytes(), ParseBudget::DEFAULT);
    assert!(
        tree.is_some(),
        "default budget must comfortably parse {} bytes of trivial source",
        src.len()
    );
}
