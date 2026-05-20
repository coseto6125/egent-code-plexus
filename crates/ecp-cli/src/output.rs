//! Output emission: consolidates the toon/json branching previously
//! duplicated across every command.

use ecp_core::EcpError;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
