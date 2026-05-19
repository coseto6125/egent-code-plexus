//! Regression test for UTF-8 char-boundary panics in `response_shapes`.
//!
//! `detect_js_status_code` and `detect_php_status_code` derive slice bounds
//! via byte arithmetic (`saturating_sub(N)` / `+ N`). When multi-byte
//! codepoints (e.g. box-drawing chars `─` = 3 bytes, `★` = 3 bytes) appear
//! in a doc-comment banner that sits inside the lookback/lookahead window,
//! the arithmetic offsets can land mid-codepoint, causing a panic.
//!
//! These tests construct inputs where the window boundary falls exactly
//! inside a 3-byte codepoint and assert no panic + correct extraction.
//!
//! Scope note: this is a localised safety fix in a single module; the
//! 14-language coverage rule (CLAUDE.md) applies to parser/graph primitives,
//! not to this module-level UTF-8 guard.

use graph_nexus_analyzer::fetch_shape::response_shapes::{extract, Lang};

/// Build a string whose prefix is `prefix_len` bytes of box-drawing chars
/// (`─`, 3 bytes each), followed by `suffix`. May overshoot `prefix_len` by
/// up to 2 bytes — that overshoot is the point, since it forces a downstream
/// `saturating_sub(N)` boundary to land mid-codepoint.
fn with_box_drawing_prefix(prefix_len: usize, suffix: &str) -> String {
    // Fill with '─' (3 bytes) groups. We may overshoot by up to 2 bytes,
    // which is the point: the window boundary will land inside a codepoint.
    let mut s = String::new();
    while s.len() < prefix_len {
        s.push('─');
    }
    s.push_str(suffix);
    s
}

// ── JS / TS ──────────────────────────────────────────────────────────────────

#[test]
fn js_lookback_window_straddles_multibyte_no_panic() {
    // Place `res.status(200).json({id})` after ~210 bytes of box-drawing
    // chars so the 200-byte lookback for STATUS_CHAIN starts mid-codepoint.
    let src = with_box_drawing_prefix(210, "res.status(200).json({ id })");
    let shape = extract(&src, Lang::JavaScript);
    assert_eq!(shape.response_keys, vec!["id"]);
    assert!(shape.error_keys.is_empty());
}

#[test]
fn js_lookahead_window_straddles_multibyte_no_panic() {
    // Place a starred doc-comment *after* the closing brace so the 150-byte
    // lookahead for SECOND_ARG_STATUS starts mid-codepoint.
    // `★` is 3 bytes (U+2605).
    let stars = "★".repeat(55); // 165 bytes
    let src = format!("res.json({{ msg }}, {{ status: 401 }}){stars}");
    let shape = extract(&src, Lang::JavaScript);
    assert_eq!(shape.error_keys, vec!["msg"]);
}

#[test]
fn ts_lookback_window_straddles_multibyte_no_panic() {
    // Same as the JS lookback test but with Lang::TypeScript to confirm
    // both aliases go through the same fixed path.
    let src = with_box_drawing_prefix(210, "res.status(500).json({ error })");
    let shape = extract(&src, Lang::TypeScript);
    assert_eq!(shape.error_keys, vec!["error"]);
    assert!(shape.response_keys.is_empty());
}

#[test]
fn js_new_response_ext_lookback_straddles_multibyte_no_panic() {
    // The NEW_RESPONSE_STRINGIFY lookback is 300 bytes; place content so
    // the ext_start offset falls mid-codepoint.
    let src = with_box_drawing_prefix(310, "res.json({ data })");
    let shape = extract(&src, Lang::JavaScript);
    assert_eq!(shape.response_keys, vec!["data"]);
}

// ── PHP ──────────────────────────────────────────────────────────────────────

#[test]
fn php_lookback_window_straddles_multibyte_no_panic() {
    // Place `http_response_code(400); echo json_encode(...)` after ~310 bytes
    // of box-drawing chars so the 300-byte lookback starts mid-codepoint.
    let src = with_box_drawing_prefix(
        310,
        r#"http_response_code(400); echo json_encode(['err' => 1]);"#,
    );
    let shape = extract(&src, Lang::Php);
    assert_eq!(shape.error_keys, vec!["err"]);
    assert!(shape.response_keys.is_empty());
}

#[test]
fn php_no_status_with_multibyte_prefix_no_panic() {
    let src = with_box_drawing_prefix(310, r#"echo json_encode(['ok' => 1]);"#);
    let shape = extract(&src, Lang::Php);
    assert_eq!(shape.response_keys, vec!["ok"]);
}
