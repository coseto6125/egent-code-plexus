//! BlindSpot detection for Go `const ( ... iota )` enum imitation (FU-2026-05-23-011).
//!
//! Go has no first-class enum syntax. The community-canonical imitation is:
//!   type Status int
//!   const ( StatusActive Status = iota; StatusInactive; StatusPending )
//!
//! These emit `NodeKind::Const` (correct — Go const is not first-class enum).
//! Without a BlindSpot, LLMs querying `MATCH (n:EnumVariant)` on Go codebases
//! get empty results despite enum-like discriminant sets being everywhere.
//!
//! The heuristic: const_declaration block at source-file scope with ≥2 const_spec
//! children where at least one spec's value subtree contains the identifier `iota`.

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_go(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = GoProvider::new().expect("GoProvider::new");
    provider
        .parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse_file")
}

fn iota_blind_spot_count(g: &ecp_core::analyzer::types::LocalGraph) -> usize {
    g.blind_spots
        .iter()
        .filter(|b| b.kind == "go-iota-const-block")
        .count()
}

// ── Positive: basic iota (no type annotation) ──────────────────────────────

#[test]
fn basic_iota_emits_one_blind_spot() {
    let src = r#"
package main

const (
    Active = iota
    Inactive
    Pending
)
"#;
    let g = parse_go(src);
    assert_eq!(
        iota_blind_spot_count(&g),
        1,
        "basic iota block must emit exactly 1 BlindSpot; got: {:?}",
        g.blind_spots
    );
}

// ── Positive: typed iota (Status int pattern) ──────────────────────────────

#[test]
fn typed_iota_emits_one_blind_spot() {
    let src = r#"
package main

type Status int

const (
    StatusActive   Status = iota
    StatusInactive
    StatusPending
)
"#;
    let g = parse_go(src);
    assert_eq!(
        iota_blind_spot_count(&g),
        1,
        "typed iota block must emit exactly 1 BlindSpot; got: {:?}",
        g.blind_spots
    );
}

// ── Positive: bitmask iota (1 << iota) ─────────────────────────────────────

#[test]
fn bitmask_iota_emits_one_blind_spot() {
    let src = r#"
package main

const (
    FlagRead    = 1 << iota
    FlagWrite
    FlagExecute
)
"#;
    let g = parse_go(src);
    assert_eq!(
        iota_blind_spot_count(&g),
        1,
        "bitmask iota block (1 << iota) must emit exactly 1 BlindSpot; got: {:?}",
        g.blind_spots
    );
}

// ── Negative: const block without iota ────────────────────────────────────

#[test]
fn const_block_without_iota_emits_no_blind_spot() {
    let src = r#"
package main

const (
    MaxRetries = 3
    Timeout    = 30
)
"#;
    let g = parse_go(src);
    assert_eq!(
        iota_blind_spot_count(&g),
        0,
        "non-iota const block must NOT emit BlindSpot; got: {:?}",
        g.blind_spots
    );
}

// ── Negative: single-entry iota block (< 2 specs) ─────────────────────────

#[test]
fn single_entry_iota_emits_no_blind_spot() {
    let src = r#"
package main

const (
    OnlyOne = iota
)
"#;
    let g = parse_go(src);
    assert_eq!(
        iota_blind_spot_count(&g),
        0,
        "single-entry iota block must NOT emit BlindSpot (< 2 specs); got: {:?}",
        g.blind_spots
    );
}

// ── Negative: single-line const (not block form) ──────────────────────────

#[test]
fn single_line_const_emits_no_blind_spot() {
    let src = "package main\n\nconst Pi = 3.14\n";
    let g = parse_go(src);
    assert_eq!(
        iota_blind_spot_count(&g),
        0,
        "single-line const must NOT emit BlindSpot; got: {:?}",
        g.blind_spots
    );
}

// ── Extra: multiple iota blocks in one file → one BlindSpot per block ─────

#[test]
fn two_iota_blocks_emit_two_blind_spots() {
    let src = r#"
package main

const (
    A = iota
    B
)

const (
    X = iota
    Y
    Z
)
"#;
    let g = parse_go(src);
    assert_eq!(
        iota_blind_spot_count(&g),
        2,
        "two separate iota blocks must emit 2 BlindSpots; got: {:?}",
        g.blind_spots
    );
}

// ── Extra: iota block mixed with non-iota block → only iota block flagged ──

#[test]
fn mixed_file_only_flags_iota_blocks() {
    let src = r#"
package main

const (
    MaxRetries = 3
    Timeout    = 30
)

const (
    StatusActive   = iota
    StatusInactive
)
"#;
    let g = parse_go(src);
    assert_eq!(
        iota_blind_spot_count(&g),
        1,
        "only iota block must be flagged; non-iota block skipped; got: {:?}",
        g.blind_spots
    );
}
