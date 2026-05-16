//! Port of upstream `extractResponseShapes` + `extractPHPResponseShapes`
//! (`gitnexus/src/core/ingestion/route-extractors/response-shapes.ts`).
//!
//! Pure regex + state-machine scan over a route handler's source.
//! Detects payload emission, extracts top-level keys, and classifies
//! by HTTP status:
//! - JS/TS: every `.json(...)` call body; status detected via `.status(N)`
//!   chain (lookback) or second-arg `{ status: N }` (lookahead).
//! - PHP: every `json_encode([...])` / `json_encode(array(...))` call body;
//!   status detected via `http_response_code` / `header('HTTP/...')` /
//!   `header('Status: ...')` in the preceding ~300 bytes, with
//!   exit/die-boundary respected.
//!
//! Status classification (same as upstream):
//! - `status >= 400` → `error_keys`
//! - anything else (incl. `undefined`/`200`/`2xx`) → `response_keys`
//!
//! Returns deduped, sorted vecs. Empty payload bodies are skipped (the
//! call produced no extractable keys).

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// Language hint for the extractor — controls which regex/scanner set runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    JavaScript,
    TypeScript,
    Php,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseShape {
    pub response_keys: Vec<String>,
    pub error_keys: Vec<String>,
}

/// Top-level dispatch. `Lang::JavaScript` and `Lang::TypeScript` share
/// the same scanner (TS is a superset for these patterns); `Lang::Php`
/// routes to the PHP-specific scanner. Returns `Default::default()` on
/// non-matching input.
pub fn extract(content: &str, lang: Lang) -> ResponseShape {
    let (mut succ, mut err) = match lang {
        Lang::JavaScript | Lang::TypeScript => extract_js(content),
        Lang::Php => extract_php(content),
    };
    dedupe_sort(&mut succ);
    dedupe_sort(&mut err);
    ResponseShape {
        response_keys: succ,
        error_keys: err,
    }
}

fn dedupe_sort(v: &mut Vec<String>) {
    let set: HashSet<String> = v.drain(..).collect();
    *v = set.into_iter().collect();
    v.sort();
}

// ─── JS/TS scanner ───────────────────────────────────────────────────────

static JSON_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.json\s*\(").expect("JSON_CALL compiles"));

static STATUS_CHAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.status\s*\(\s*(\d{3})\s*\)\s*$").expect("STATUS_CHAIN compiles")
});

static SECOND_ARG_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*,\s*\{[^}]*status\s*:\s*(\d{3})").expect("SECOND_ARG_STATUS compiles")
});

static NEW_RESPONSE_STRINGIFY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"new\s+Response\s*\(\s*JSON\s*\.stringify\s*$")
        .expect("NEW_RESPONSE_STRINGIFY compiles")
});

static NEW_RESPONSE_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\)\s*,\s*\{[^}]*status\s*:\s*(\d{3})").expect("NEW_RESPONSE_STATUS compiles")
});

fn extract_js(content: &str) -> (Vec<String>, Vec<String>) {
    let mut success = Vec::new();
    let mut errors = Vec::new();
    let bytes = content.as_bytes();

    for m in JSON_CALL.find_iter(content) {
        let match_pos = m.start();
        let start_idx = m.end();
        // Advance to first `{` or `)` — payload must start with `{`.
        let mut i = start_idx;
        while i < bytes.len() && bytes[i] != b'{' && bytes[i] != b')' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'{' {
            continue;
        }

        let (call_keys, closing_brace) = scan_object_keys(content, i);
        if call_keys.is_empty() {
            continue;
        }
        let status = detect_js_status_code(content, match_pos, closing_brace);
        if matches!(status, Some(s) if s >= 400) {
            errors.extend(call_keys);
        } else {
            success.extend(call_keys);
        }
    }

    (success, errors)
}

/// State-machine scan of a `{...}` payload starting at `open_pos` (the
/// `{` byte). Returns (top-level keys, closing-brace byte position).
/// Mirrors upstream's hand-rolled scanner: handles unquoted identifiers
/// and quoted string keys at depth 1, ignores nested object content,
/// respects string escapes.
fn scan_object_keys(content: &str, open_pos: usize) -> (Vec<String>, i64) {
    let bytes = content.as_bytes();
    let mut keys = Vec::new();
    let mut depth: i32 = 0;
    let mut key_start: Option<usize> = None;
    let mut in_string: Option<u8> = None;
    let mut closing: i64 = -1;
    let mut j = open_pos;

    while j < bytes.len() {
        let ch = bytes[j];

        if let Some(quote) = in_string {
            if ch == b'\\' {
                j += 2;
                continue;
            }
            if ch == quote {
                in_string = None;
            }
            j += 1;
            continue;
        }

        if ch == b'"' || ch == b'\'' || ch == b'`' {
            // Quoted string key at depth 1, before `:` — `{ 'foo': v }`.
            if depth == 1 && key_start.is_none() {
                let quote = ch;
                let str_start = j + 1;
                let mut s = str_start;
                let mut str_end: Option<usize> = None;
                while s < bytes.len() {
                    if bytes[s] == b'\\' {
                        s += 2;
                        continue;
                    }
                    if bytes[s] == quote {
                        str_end = Some(s);
                        break;
                    }
                    s += 1;
                }
                if let Some(end) = str_end {
                    // Scan forward for `:` skipping whitespace; only a
                    // following `:` confirms this is a key (not a string
                    // value like `{ x: 'foo' }`).
                    let mut p = end + 1;
                    while p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\n' | b'\r') {
                        p += 1;
                    }
                    if p < bytes.len() && bytes[p] == b':' {
                        if let Ok(key) = std::str::from_utf8(&bytes[str_start..end]) {
                            keys.push(key.to_string());
                        }
                    }
                    j = end + 1;
                    continue;
                }
            }
            in_string = Some(ch);
            j += 1;
            continue;
        }

        if ch == b'{' {
            depth += 1;
            j += 1;
            continue;
        }
        if ch == b'}' {
            depth -= 1;
            if depth == 0 {
                closing = j as i64;
                break;
            }
            j += 1;
            continue;
        }
        if depth != 1 {
            j += 1;
            continue;
        }

        let is_ident_start = ch.is_ascii_alphabetic() || ch == b'_' || ch == b'$';
        let is_ident_cont = ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'$';

        if key_start.is_none() && is_ident_start {
            key_start = Some(j);
            j += 1;
            continue;
        }
        if let Some(start) = key_start {
            if !is_ident_cont {
                if let Ok(key) = std::str::from_utf8(&bytes[start..j]) {
                    // Only commit if the next non-space char is `:`/`,`/`}`.
                    let rest_start = skip_ws(bytes, j);
                    if rest_start < bytes.len() && matches!(bytes[rest_start], b':' | b',' | b'}') {
                        keys.push(key.to_string());
                    }
                }
                key_start = None;
            }
        }
        j += 1;
    }

    (keys, closing)
}

fn skip_ws(bytes: &[u8], from: usize) -> usize {
    let mut p = from;
    while p < bytes.len() && bytes[p].is_ascii_whitespace() {
        p += 1;
    }
    p
}

/// Slice `s[start..end]` after clamping and expanding the byte offsets to the
/// nearest surrounding char boundaries. Used by status-code lookback/lookahead
/// heuristics that derive offsets via byte arithmetic and may land mid-codepoint
/// (e.g. box-drawing chars in a doc-comment banner adjacent to the match site).
fn safe_slice(s: &str, start: usize, end: usize) -> &str {
    let mut start = start.min(s.len());
    while start > 0 && !s.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = end.min(s.len());
    while end < s.len() && !s.is_char_boundary(end) {
        end += 1;
    }
    &s[start..end]
}

/// Locate the HTTP status code surrounding a `.json(` match.
/// Looks ~200 bytes back for an Express-style `.status(N)` chain, then
/// ~150 bytes forward past the closing brace for a Next-style
/// `, { status: N }` second arg, then ~300 bytes back for the
/// `new Response(JSON.stringify({...}), { status: N })` pattern.
fn detect_js_status_code(content: &str, json_pos: usize, closing: i64) -> Option<u16> {
    let lookback_start = json_pos.saturating_sub(200);
    let before = safe_slice(content, lookback_start, json_pos);
    if let Some(c) = STATUS_CHAIN.captures(before) {
        if let Ok(n) = c[1].parse() {
            return Some(n);
        }
    }
    if closing > 0 {
        let after_start = (closing as usize) + 1;
        let after_end = (after_start + 150).min(content.len());
        if let Some(c) = SECOND_ARG_STATUS.captures(safe_slice(content, after_start, after_end)) {
            if let Ok(n) = c[1].parse() {
                return Some(n);
            }
        }
    }
    let ext_start = json_pos.saturating_sub(300);
    let ext_before = safe_slice(content, ext_start, json_pos);
    if NEW_RESPONSE_STRINGIFY.is_match(ext_before) && closing > 0 {
        let after_start = (closing as usize) + 1;
        let after_end = (after_start + 200).min(content.len());
        if let Some(c) = NEW_RESPONSE_STATUS.captures(safe_slice(content, after_start, after_end)) {
            if let Ok(n) = c[1].parse() {
                return Some(n);
            }
        }
    }
    None
}

// ─── PHP scanner ─────────────────────────────────────────────────────────

static JSON_ENCODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"json_encode\s*\(").expect("JSON_ENCODE compiles"));

/// PHP array key pattern. Rust's regex engine has no backreferences,
/// so upstream's `(['"])(\w+)\1` becomes a character class on both
/// sides. PHP source uses matching quotes per literal (`'foo'` or
/// `"foo"`); mismatched openers would be a parse error in real PHP,
/// so the relaxed form has no observable false positives in practice.
static PHP_ARRAY_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]\s*=>"#).expect("PHP_ARRAY_KEY compiles")
});

static EXIT_DIE_BOUNDARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:exit|die)\s*(?:\([^)]*\))?\s*;").expect("EXIT_DIE_BOUNDARY compiles")
});

static HTTP_RESPONSE_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"http_response_code\s*\(\s*(\d{3})\s*\)").expect("HTTP_RESPONSE_CODE compiles")
});

static HEADER_HTTP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"header\s*\(\s*['"]HTTP/[\d.]+\s+(\d{3})"#).expect("HEADER_HTTP compiles")
});

static HEADER_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"header\s*\(\s*['"]Status:\s*(\d{3})"#).expect("HEADER_STATUS compiles")
});

fn extract_php(content: &str) -> (Vec<String>, Vec<String>) {
    let mut success = Vec::new();
    let mut errors = Vec::new();
    let bytes = content.as_bytes();

    for m in JSON_ENCODE.find_iter(content) {
        let match_pos = m.start();
        let start_idx = m.end();
        // Skip whitespace.
        let mut i = start_idx;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            continue;
        }
        let (array_start, array_end) = if bytes[i] == b'[' {
            let end = find_matching_bracket(content, i, b'[', b']');
            if end < 0 {
                continue;
            }
            (i, end as usize)
        } else if content[i..].starts_with("array(") {
            let paren = i + 5; // points to '('
            let end = find_matching_bracket(content, paren, b'(', b')');
            if end < 0 {
                continue;
            }
            (paren, end as usize)
        } else {
            continue;
        };
        // safety: array_start points to '[' or '(' (ASCII), so +1 is a char
        // boundary; array_end points to ']' or ')' (ASCII), also a boundary.
        let array_content = &content[array_start + 1..array_end];
        let call_keys = extract_php_array_keys(array_content);
        if call_keys.is_empty() {
            continue;
        }
        let status = detect_php_status_code(content, match_pos);
        if matches!(status, Some(s) if s >= 400) {
            errors.extend(call_keys);
        } else {
            success.extend(call_keys);
        }
    }

    (success, errors)
}

/// State-machine matching bracket finder. `open_pos` points at the open
/// bracket byte. Returns the matching close-bracket byte index, or -1.
fn find_matching_bracket(content: &str, open_pos: usize, open: u8, close: u8) -> i64 {
    let bytes = content.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string: Option<u8> = None;
    let mut j = open_pos;
    while j < bytes.len() {
        let ch = bytes[j];
        if let Some(quote) = in_string {
            if ch == b'\\' {
                j += 2;
                continue;
            }
            if ch == quote {
                in_string = None;
            }
            j += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' {
            in_string = Some(ch);
            j += 1;
            continue;
        }
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return j as i64;
            }
        }
        j += 1;
    }
    -1
}

/// Extract `'key' => ...` top-level entries from a PHP array body.
/// Walks the body bytewise, tracking depth so nested arrays' keys
/// (`['x' => ['y' => 1]]` — only `x` counts) are skipped. Mirrors
/// upstream `extractPHPArrayKeys`.
fn extract_php_array_keys(array_content: &str) -> Vec<String> {
    let bytes = array_content.as_bytes();
    let mut keys = Vec::new();
    let mut depth: i32 = 0;
    let mut in_string: Option<u8> = None;
    let mut top_ranges: Vec<(usize, usize)> = Vec::new();
    let mut range_start: usize = 0;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(quote) = in_string {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' {
            in_string = Some(ch);
            i += 1;
            continue;
        }
        if ch == b'[' || ch == b'(' || ch == b'{' {
            if depth == 0 {
                top_ranges.push((range_start, i));
            }
            depth += 1;
        } else if ch == b']' || ch == b')' || ch == b'}' {
            depth -= 1;
            if depth == 0 {
                range_start = i + 1;
            }
        }
        i += 1;
    }
    if depth == 0 {
        top_ranges.push((range_start, bytes.len()));
    }
    for (s, e) in top_ranges {
        let segment = &array_content[s..e];
        for c in PHP_ARRAY_KEY.captures_iter(segment) {
            keys.push(c[1].to_string());
        }
    }
    keys
}

/// PHP status code detection — port of upstream `detectPHPStatusCode`.
/// Looks ~300 bytes back from the `json_encode` position, trimming
/// anything before the last `exit;` / `die();` boundary so unrelated
/// earlier control-flow doesn't contaminate the lookup.
fn detect_php_status_code(content: &str, json_encode_pos: usize) -> Option<u16> {
    let lookback_start = json_encode_pos.saturating_sub(300);
    let mut before = safe_slice(content, lookback_start, json_encode_pos);
    let boundary_end = find_last_exit_boundary(before);
    if boundary_end >= 0 {
        before = &before[boundary_end as usize..];
    }
    last_match_group(before, &HTTP_RESPONSE_CODE)
        .or_else(|| last_match_group(before, &HEADER_HTTP))
        .or_else(|| last_match_group(before, &HEADER_STATUS))
}

fn find_last_exit_boundary(text: &str) -> i64 {
    let mut last_end: i64 = -1;
    for m in EXIT_DIE_BOUNDARY.find_iter(text) {
        last_end = m.end() as i64;
    }
    last_end
}

fn last_match_group(text: &str, pat: &Regex) -> Option<u16> {
    pat.captures_iter(text)
        .last()
        .and_then(|c| c[1].parse().ok())
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_default() {
        let s = extract("", Lang::JavaScript);
        assert!(s.response_keys.is_empty());
        assert!(s.error_keys.is_empty());
    }

    #[test]
    fn js_success_status_200_json() {
        let src = "res.status(200).json({ id, name })";
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["id", "name"]);
        assert!(s.error_keys.is_empty());
    }

    #[test]
    fn js_error_status_400_json() {
        let src = r#"res.status(400).json({ error: "bad" })"#;
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.error_keys, vec!["error"]);
        assert!(s.response_keys.is_empty());
    }

    #[test]
    fn js_no_status_treated_as_success() {
        let src = "res.json({ ok })";
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["ok"]);
    }

    #[test]
    fn js_response_json_static() {
        let src = "return Response.json({ data })";
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["data"]);
    }

    #[test]
    fn js_new_response_jsonstringify_not_captured() {
        // Upstream's scanner triggers only on `.json(` matches; the
        // `new Response(JSON.stringify({...}))` form contains no
        // `.json(` token, so no keys are extracted. Documented
        // upstream limitation — porting faithfully.
        let src = "return new Response(JSON.stringify({ items }))";
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s, ResponseShape::default());
    }

    #[test]
    fn js_shorthand_and_explicit_keys_mixed() {
        let src = "res.json({ id, name: u.name, age: 10 })";
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["age", "id", "name"]);
    }

    #[test]
    fn js_nested_object_top_level_only() {
        let src = "res.json({ user: { id, name } })";
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["user"]);
    }

    #[test]
    fn js_quoted_string_key() {
        // Numeric values used so the scanner only captures the
        // quoted keys (upstream's scanner over-captures bare value
        // identifiers — that's a separate, documented behavior).
        let src = r#"res.json({ "courses": 1, 'flag': 2 })"#;
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["courses", "flag"]);
    }

    #[test]
    fn js_bare_identifier_value_is_overcaptured() {
        // Upstream's scanner does not distinguish key vs value
        // position; a bare identifier value like `data` in
        // `{ id: data }` is captured as if it were a key. This
        // makes shape_check more permissive (fewer false mismatches
        // when handlers do explicit aliasing). Documented quirk —
        // matching upstream exactly.
        let src = r#"res.json({ id: data })"#;
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["data", "id"]);
    }

    #[test]
    fn js_spread_contributes_identifier_name() {
        // Upstream regex captures the spread variable's name as if it
        // were a key, since the scanner just looks at identifier chars
        // followed by `,`. Matching upstream behavior exactly.
        let src = "res.json({ ...rest, id })";
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["id", "rest"]);
    }

    #[test]
    fn js_multiple_calls_both_paths() {
        let src = r#"
            if (err) res.status(400).json({ error: "x" });
            else res.status(200).json({ ok, total });
        "#;
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["ok", "total"]);
        assert_eq!(s.error_keys, vec!["error"]);
    }

    #[test]
    fn js_status_500_routes_to_error() {
        let src = r#"res.status(500).json({ message: "boom" })"#;
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.error_keys, vec!["message"]);
    }

    #[test]
    fn js_second_arg_status_response_json() {
        let src = "return Response.json({ msg }, { status: 401 })";
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.error_keys, vec!["msg"]);
    }

    #[test]
    fn js_dedupe_across_calls() {
        let src = r#"
            res.status(200).json({ id });
            res.status(200).json({ id, name });
        "#;
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s.response_keys, vec!["id", "name"]);
    }

    #[test]
    fn js_ts_alias_uses_same_scanner() {
        let src = "res.json({ a, b })";
        let s_js = extract(src, Lang::JavaScript);
        let s_ts = extract(src, Lang::TypeScript);
        assert_eq!(s_js, s_ts);
    }

    // ─── PHP ───

    #[test]
    fn php_json_encode_success_array_bracket() {
        let src = r#"echo json_encode(['name' => $n, 'id' => $i]);"#;
        let s = extract(src, Lang::Php);
        assert_eq!(s.response_keys, vec!["id", "name"]);
    }

    #[test]
    fn php_json_encode_legacy_array_form() {
        let src = "echo json_encode(array('a' => 1, 'b' => 2));";
        let s = extract(src, Lang::Php);
        assert_eq!(s.response_keys, vec!["a", "b"]);
    }

    #[test]
    fn php_status_400_routes_to_error() {
        let src = r#"http_response_code(400); echo json_encode(['code' => 'fail']);"#;
        let s = extract(src, Lang::Php);
        assert_eq!(s.error_keys, vec!["code"]);
    }

    #[test]
    fn php_header_http_status_400() {
        let src = r#"header('HTTP/1.1 400 Bad'); echo json_encode(['msg' => 'x']);"#;
        let s = extract(src, Lang::Php);
        assert_eq!(s.error_keys, vec!["msg"]);
    }

    #[test]
    fn php_nested_array_top_level_only() {
        let src = r#"echo json_encode(['outer' => ['inner' => 1], 'flat' => 2]);"#;
        let s = extract(src, Lang::Php);
        assert_eq!(s.response_keys, vec!["flat", "outer"]);
    }

    #[test]
    fn php_exit_boundary_isolates_status() {
        let src = r#"
            http_response_code(500); exit;
            http_response_code(200); echo json_encode(['ok' => 1]);
        "#;
        let s = extract(src, Lang::Php);
        // exit boundary trims the 500 lookup; remaining lookup sees 200.
        assert_eq!(s.response_keys, vec!["ok"]);
    }

    #[test]
    fn lang_php_ignores_js_pattern() {
        let src = "res.json({ a, b })";
        let s = extract(src, Lang::Php);
        assert_eq!(s, ResponseShape::default());
    }

    #[test]
    fn lang_js_ignores_php_pattern() {
        let src = r#"echo json_encode(['a' => 1]);"#;
        let s = extract(src, Lang::JavaScript);
        assert_eq!(s, ResponseShape::default());
    }
}
