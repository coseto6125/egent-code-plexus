//! Output emission: consolidates the toon/json branching previously
//! duplicated across every command.

use ecp_core::EcpError;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Default output: per-command LLM-tuned payload (token compression,
    /// rounded floats, trimmed timestamps, etc.) rendered through the toon
    /// encoder. Commands without command-specific tuning fall through to the
    /// same output as `Toon`.
    Llm,
    /// Neutral toon: the json payload encoded as toon, no lossy compression.
    /// Use when you want round-trippable structure but tighter than JSON.
    Toon,
    /// Full-fidelity JSON. Always round-trippable; pick this when piping to
    /// another tool / asserting in tests.
    Json,
    Text,
}

impl OutputFormat {
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("json") => OutputFormat::Json,
            Some("toon") => OutputFormat::Toon,
            Some("text") => OutputFormat::Text,
            _ => OutputFormat::Llm, // default
        }
    }
}

/// Format `value` per `format` and return the rendered string. Does NOT
/// write to stdout — callers decide. Used by CLI `emit()` (which prints)
/// and by MCP daemon-mode dispatch (which wraps the string in
/// `ToolResult::text`).
pub fn emit_to_string(value: &Value, format: OutputFormat) -> Result<String, EcpError> {
    match format {
        OutputFormat::Llm => {
            // Apply lossy compression (rounded floats, trimmed ISO timestamps)
            // before toon-encoding so the LLM-facing default is as token-cheap
            // as possible. Caller's `value` is left untouched.
            let mut v = value.clone();
            compress_for_llm(&mut v);
            factor_base_path(&mut v);
            let bytes = serde_json::to_vec(&v)
                .map_err(|e| EcpError::Output(format!("json serialize: {e}")))?;
            _etoon::toon::encode(&bytes).map_err(|e| EcpError::Output(format!("toon encode: {e}")))
        }
        OutputFormat::Toon => {
            let bytes = serde_json::to_vec(value)
                .map_err(|e| EcpError::Output(format!("json serialize: {e}")))?;
            _etoon::toon::encode(&bytes).map_err(|e| EcpError::Output(format!("toon encode: {e}")))
        }
        OutputFormat::Json => serde_json::to_string(value)
            .map_err(|e| EcpError::Output(format!("json serialize: {e}"))),
        OutputFormat::Text => {
            if let Some(results) = value.get("results").and_then(|v| v.as_array()) {
                Ok(results
                    .iter()
                    .filter_map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"))
            } else {
                serde_json::to_string_pretty(value)
                    .map_err(|e| EcpError::Output(format!("json pretty: {e}")))
            }
        }
    }
}

/// Print `value` to stdout in the requested format. Thin wrapper over
/// [`emit_to_string`].
pub fn emit(value: &Value, format: OutputFormat) -> Result<(), EcpError> {
    println!("{}", emit_to_string(value, format)?);
    Ok(())
}

/// Walk a payload and apply LLM-friendly lossy compression in-place:
///
/// - `f64` numbers → rounded to 4 decimals (integers untouched)
/// - strings whose first 11 bytes look RFC3339 (`YYYY-MM-DDT...`) →
///   sub-second precision dropped, `+00:00` rewritten to `Z`
///
/// Invoked automatically by `emit_to_string` when `format == Llm`. The
/// caller's value is cloned first; the cloning cost is on the (small)
/// payload size, not the runtime hot path.
pub fn compress_for_llm(v: &mut Value) {
    match v {
        Value::Object(map) => {
            // `uid` is a deterministic `<kind>:<filePath>:<name>` triple of
            // fields that already co-reside in every emit-row, so it's pure
            // redundancy. `handlerUid` looks similar but points at a
            // *different* node (the handler) than the row's own filePath/name
            // — keep it; the LLM has no other handle to link route → handler.
            map.remove("uid");
            for child in map.values_mut() {
                compress_for_llm(child);
            }
        }
        Value::Array(arr) => {
            for child in arr.iter_mut() {
                compress_for_llm(child);
            }
        }
        Value::Number(n) if n.is_f64() => {
            // Only round true f64s. `as_f64()` happily upcasts integers too,
            // and round-tripping them through `from_f64` would silently
            // promote `4922` to `4922.0` — surprising the consumer.
            if let Some(f) = n.as_f64() {
                let rounded = (f * 10000.0).round() / 10000.0;
                if let Some(new_n) = serde_json::Number::from_f64(rounded) {
                    *n = new_n;
                }
            }
        }
        Value::String(s) => {
            if is_anonymous_with_position(s) {
                *s = "<anonymous>".to_string();
                return;
            }
            // Cheap shape gate: only touch strings that look like RFC3339
            // (`YYYY-MM-DDT...`). Avoids walking unrelated string fields.
            let bytes = s.as_bytes();
            if bytes.len() >= 11 && bytes[4] == b'-' && bytes[7] == b'-' && bytes[10] == b'T' {
                let compact = compact_iso(s);
                if compact.len() < s.len() {
                    *s = compact;
                }
            }
        }
        _ => {}
    }
}

/// Net saving must clear this many bytes before hoisting is worth the extra
/// `base_path` line. `prefix_len * (n_paths - 1)` is the bytes removed from
/// rows; below this a short shared prefix (or a 2-row payload) isn't worth the
/// indirection.
const PATH_PREFIX_MIN_SAVING: usize = 48;

/// Hoist the directory prefix shared by every `filePath` in the payload into a
/// single top-level `base_path` field and rewrite each `filePath` to a relative
/// `relPath`, when the saving is worthwhile. Llm-only: keeps toon's flat
/// `[N]{cols}` table
/// intact (a nested dir-trie repeats the column names per row → measured
/// larger) while removing the dominant per-row path redundancy.
pub fn factor_base_path(v: &mut Value) {
    if !v.is_object() {
        return;
    }
    let mut paths: Vec<&str> = Vec::new();
    collect_file_paths(v, &mut paths);
    if paths.len() < 2 {
        return;
    }
    let prefix = common_dir_prefix(&paths);
    if prefix.is_empty() || prefix.len() * (paths.len() - 1) < PATH_PREFIX_MIN_SAVING {
        return;
    }
    drop(paths);
    relativise_file_paths(v, &prefix);
    if let Value::Object(map) = v {
        map.insert("base_path".to_string(), Value::String(prefix));
    }
}

fn collect_file_paths<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                match (k.as_str(), child) {
                    ("filePath", Value::String(s)) => out.push(s),
                    _ => collect_file_paths(child, out),
                }
            }
        }
        Value::Array(arr) => arr.iter().for_each(|c| collect_file_paths(c, out)),
        _ => {}
    }
}

/// Longest prefix shared by all paths, trimmed back to the last `/` so the
/// result is a whole directory boundary (`/` is ASCII → always a char
/// boundary, no mid-component cut).
fn common_dir_prefix(paths: &[&str]) -> String {
    let first = paths[0].as_bytes();
    let mut end = first.len();
    for p in &paths[1..] {
        let pb = p.as_bytes();
        let mut i = 0;
        while i < end && i < pb.len() && first[i] == pb[i] {
            i += 1;
        }
        end = i;
    }
    match first[..end].iter().rposition(|&b| b == b'/') {
        Some(slash) => String::from_utf8_lossy(&first[..=slash]).into_owned(),
        None => String::new(),
    }
}

/// Strip `prefix` from each `filePath` and rename the key to `relPath`, so the
/// field name itself signals the value is now a fragment to prepend `base_path`
/// to (rather than a full path the consumer can use verbatim).
fn relativise_file_paths(v: &mut Value, prefix: &str) {
    match v {
        Value::Object(map) => {
            let relative = match map.get("filePath") {
                Some(Value::String(s)) => s.strip_prefix(prefix).map(str::to_owned),
                _ => None,
            };
            if let Some(rel) = relative {
                map.remove("filePath");
                map.insert("relPath".to_string(), Value::String(rel));
            }
            for child in map.values_mut() {
                relativise_file_paths(child, prefix);
            }
        }
        Value::Array(arr) => arr
            .iter_mut()
            .for_each(|c| relativise_file_paths(c, prefix)),
        _ => {}
    }
}

/// `<anonymous:line:col>` carries a position only to keep the node's uid
/// distinct (uid hashes name without span). The position duplicates the row's
/// own `line` column, so the Llm rendering drops it back to bare `<anonymous>`.
fn is_anonymous_with_position(s: &str) -> bool {
    let Some(inner) = s
        .strip_prefix("<anonymous:")
        .and_then(|rest| rest.strip_suffix('>'))
    else {
        return false;
    };
    match inner.split_once(':') {
        Some((line, col)) => {
            !line.is_empty()
                && !col.is_empty()
                && line.bytes().all(|b| b.is_ascii_digit())
                && col.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// Trim a chrono-style ISO-8601 timestamp into its shortest valid form.
/// Drops sub-second precision (`.NNNNNN...`) and rewrites `+00:00` to `Z`.
fn compact_iso(ts: &str) -> String {
    let trimmed_frac = match ts.find('.') {
        Some(dot) => {
            let after_dot = &ts[dot + 1..];
            let tz_at = after_dot
                .find(['+', '-', 'Z'])
                .map(|i| dot + 1 + i)
                .unwrap_or(ts.len());
            format!("{}{}", &ts[..dot], &ts[tz_at..])
        }
        None => ts.to_string(),
    };
    if let Some(stripped) = trimmed_frac.strip_suffix("+00:00") {
        format!("{stripped}Z")
    } else {
        trimmed_frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn emit_to_string_json_returns_serialized_value() {
        let value = json!({"status": "success", "results": []});
        let out = emit_to_string(&value, OutputFormat::Json).expect("ok");
        assert!(out.contains("\"status\":\"success\""));
        assert!(out.contains("\"results\":[]"));
        // No trailing newline — caller is responsible for println! if they want stdout.
        assert!(!out.ends_with('\n'));
    }

    #[test]
    fn emit_to_string_text_extracts_results_array_lines() {
        let value = json!({"results": ["line one", "line two"]});
        let out = emit_to_string(&value, OutputFormat::Text).expect("ok");
        assert_eq!(out, "line one\nline two");
    }

    #[test]
    fn emit_to_string_toon_produces_encoded_output() {
        let value = json!({"k": "v"});
        let out = emit_to_string(&value, OutputFormat::Toon).expect("ok");
        // TOON output is non-empty and not raw JSON.
        assert!(!out.is_empty());
        assert!(!out.starts_with('{'));
    }

    #[test]
    fn emit_to_string_text_falls_back_to_pretty_json_when_no_results_field() {
        let value = json!({"status": "success"});
        let out = emit_to_string(&value, OutputFormat::Text).expect("ok");
        // Pretty-JSON fallback retains the JSON shape (indented).
        assert!(out.contains("\"status\""));
        assert!(out.contains("\"success\""));
    }

    #[test]
    fn emit_to_string_text_silently_drops_non_string_results_entries() {
        let value = json!({"results": [123, "hello", true, "world"]});
        let out = emit_to_string(&value, OutputFormat::Text).expect("ok");
        // Non-string entries are filtered out — only strings remain, joined by \n.
        assert_eq!(out, "hello\nworld");
    }

    #[test]
    fn compress_for_llm_rounds_floats_and_trims_iso() {
        let mut v = json!({
            "supported": [
                { "confidence": 0.6000000238418579_f64, "tag": "fastapi-depends" },
                { "confidence": 0.5_f64, "tag": "reflection-getattr-fanout" }
            ],
            "freshness": {
                "indexed_at": "2026-05-16T15:19:58.224238152+00:00",
                "current_head_short": "b6343a7"
            },
            "integer_kept": 4922,
            "non_iso_string": "egent-code-plexus"
        });
        compress_for_llm(&mut v);
        assert_eq!(v["supported"][0]["confidence"], json!(0.6));
        assert_eq!(v["supported"][1]["confidence"], json!(0.5));
        assert_eq!(v["freshness"]["indexed_at"], json!("2026-05-16T15:19:58Z"));
        // Non-ISO strings, integers, and non-timestamp ids stay untouched.
        assert_eq!(v["freshness"]["current_head_short"], json!("b6343a7"));
        assert_eq!(v["integer_kept"], json!(4922));
        assert_eq!(v["non_iso_string"], json!("egent-code-plexus"));
    }

    #[test]
    fn factor_base_path_hoists_common_dir_and_relativises() {
        let mut v = json!({
            "impact": [
                { "filePath": "crates/ecp-analyzer/src/go/parser.rs", "line": 1 },
                { "filePath": "crates/ecp-analyzer/src/rust/parser.rs", "line": 2 },
                { "filePath": "crates/ecp-analyzer/src/php/parser.rs", "line": 3 }
            ]
        });
        factor_base_path(&mut v);
        assert_eq!(v["base_path"], json!("crates/ecp-analyzer/src/"));
        assert_eq!(v["impact"][0]["relPath"], json!("go/parser.rs"));
        assert_eq!(v["impact"][2]["relPath"], json!("php/parser.rs"));
        assert!(
            v["impact"][0].get("filePath").is_none(),
            "filePath renamed to relPath once relativised"
        );
    }

    #[test]
    fn factor_base_path_spans_nested_arrays_with_one_global_prefix() {
        let mut v = json!({
            "incoming": {
                "calls": [
                    { "filePath": "crates/ecp-analyzer/src/go/parser.rs" },
                    { "filePath": "crates/ecp-analyzer/src/rust/parser.rs" }
                ],
                "imports": [
                    { "filePath": "crates/ecp-analyzer/src/php/parser.rs" },
                    { "filePath": "crates/ecp-analyzer/src/java/parser.rs" }
                ]
            }
        });
        factor_base_path(&mut v);
        assert_eq!(v["base_path"], json!("crates/ecp-analyzer/src/"));
        assert_eq!(v["incoming"]["calls"][0]["relPath"], json!("go/parser.rs"));
        assert_eq!(
            v["incoming"]["imports"][1]["relPath"],
            json!("java/parser.rs")
        );
    }

    #[test]
    fn factor_base_path_skips_when_saving_below_threshold() {
        // Two short paths: prefix "a/" (2) * (2-1) = 2 bytes saved < threshold.
        let mut v = json!({ "results": [{ "filePath": "a/b.rs" }, { "filePath": "a/c.rs" }] });
        factor_base_path(&mut v);
        assert!(
            v.get("base_path").is_none(),
            "trivial prefix must not be hoisted"
        );
        assert_eq!(
            v["results"][0]["filePath"],
            json!("a/b.rs"),
            "filePath untouched"
        );
    }

    #[test]
    fn compress_for_llm_truncates_anonymous_position_suffix() {
        let mut v = json!({
            "impact": [
                { "name": "<anonymous:52:18>" },
                { "name": "real_fn" },
                { "name": "<anonymous:3:0>" }
            ]
        });
        compress_for_llm(&mut v);
        assert_eq!(v["impact"][0]["name"], json!("<anonymous>"));
        assert_eq!(v["impact"][1]["name"], json!("real_fn"));
        assert_eq!(v["impact"][2]["name"], json!("<anonymous>"));
    }

    #[test]
    fn output_format_parse_routes_each_flag() {
        assert_eq!(OutputFormat::parse(None), OutputFormat::Llm);
        assert_eq!(OutputFormat::parse(Some("")), OutputFormat::Llm); // unknown falls through
        assert_eq!(OutputFormat::parse(Some("toon")), OutputFormat::Toon);
        assert_eq!(OutputFormat::parse(Some("json")), OutputFormat::Json);
        assert_eq!(OutputFormat::parse(Some("text")), OutputFormat::Text);
    }

    #[test]
    fn compact_iso_handles_iso_variants() {
        // Standard chrono-style UTC with subsecond
        assert_eq!(
            compact_iso("2026-05-16T15:33:00.410827148+00:00"),
            "2026-05-16T15:33:00Z"
        );
        // Already compact (no `.`, ends `Z`) → identity
        assert_eq!(compact_iso("2026-05-16T15:33:00Z"), "2026-05-16T15:33:00Z");
        // No subsecond, +00:00 offset → just rewrite offset to Z
        assert_eq!(
            compact_iso("2026-05-16T15:33:00+00:00"),
            "2026-05-16T15:33:00Z"
        );
        // Non-UTC offset (e.g. +08:00) → keep the offset, drop only subsecond
        assert_eq!(
            compact_iso("2026-05-16T15:33:00.123+08:00"),
            "2026-05-16T15:33:00+08:00"
        );
        // Negative offset
        assert_eq!(
            compact_iso("2026-05-16T15:33:00.999-05:00"),
            "2026-05-16T15:33:00-05:00"
        );
    }

    #[test]
    fn compress_for_llm_strips_uid_but_keeps_handler_uid() {
        let mut v = json!({
            "results": [
                {
                    "kind": "Route",
                    "filePath": "src/api.py",
                    "name": "GET /users",
                    "uid": "Route:src/api.py:GET /users",
                    "handlerUid": "Function:src/handlers.py:list_users"
                }
            ]
        });
        compress_for_llm(&mut v);
        let row = &v["results"][0];
        assert!(
            row.get("uid").is_none(),
            "uid should be stripped (derivable)"
        );
        assert!(
            row.get("handlerUid").is_some(),
            "handlerUid links to a different node — must stay"
        );
        assert_eq!(row["kind"], json!("Route"));
        assert_eq!(row["filePath"], json!("src/api.py"));
        assert_eq!(row["name"], json!("GET /users"));
    }

    #[test]
    fn emit_to_string_llm_applies_compression_toon_does_not() {
        let value = json!({
            "rows": [{ "confidence": 0.6000000238418579_f64, "indexed_at": "2026-05-16T15:19:58.224238152+00:00" }]
        });
        let llm = emit_to_string(&value, OutputFormat::Llm).expect("llm");
        let toon = emit_to_string(&value, OutputFormat::Toon).expect("toon");
        assert!(llm.contains("0.6") && !llm.contains("0.6000"));
        assert!(toon.contains("0.6000000238418579"));
        assert!(llm.contains("2026-05-16T15:19:58Z"));
        assert!(toon.contains("2026-05-16T15:19:58.224238152+00:00"));
    }
}
