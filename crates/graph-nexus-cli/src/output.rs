//! Output emission: consolidates the toon/json branching previously
//! duplicated across every command.

use graph_nexus_core::GnxError;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Toon,
    Json,
    Text,
}

impl OutputFormat {
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("json") => OutputFormat::Json,
            Some("text") => OutputFormat::Text,
            _ => OutputFormat::Toon, // default
        }
    }
}

/// Format `value` per `format` and return the rendered string. Does NOT
/// write to stdout — callers decide. Used by CLI `emit()` (which prints)
/// and by MCP daemon-mode dispatch (which wraps the string in
/// `ToolResult::text`).
pub fn emit_to_string(value: &Value, format: OutputFormat) -> Result<String, GnxError> {
    match format {
        OutputFormat::Toon => {
            let bytes = serde_json::to_vec(value)
                .map_err(|e| GnxError::Output(format!("json serialize: {e}")))?;
            _etoon::toon::encode(&bytes)
                .map_err(|e| GnxError::Output(format!("toon encode: {e}")))
        }
        OutputFormat::Json => serde_json::to_string(value)
            .map_err(|e| GnxError::Output(format!("json serialize: {e}"))),
        OutputFormat::Text => {
            if let Some(results) = value.get("results").and_then(|v| v.as_array()) {
                Ok(results
                    .iter()
                    .filter_map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"))
            } else {
                serde_json::to_string_pretty(value)
                    .map_err(|e| GnxError::Output(format!("json pretty: {e}")))
            }
        }
    }
}

/// Print `value` to stdout in the requested format. Thin wrapper over
/// [`emit_to_string`].
pub fn emit(value: &Value, format: OutputFormat) -> Result<(), GnxError> {
    println!("{}", emit_to_string(value, format)?);
    Ok(())
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
}
