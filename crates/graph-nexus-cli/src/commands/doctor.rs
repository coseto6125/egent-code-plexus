//! `gnx doctor` — surface what gnx supports as an explicit contract.
//!
//! Goal: avoid LLMs assuming "graph didn't return X => X doesn't exist".
//! Emits a hardcoded framework-coverage table (the patterns graph-nexus-analyzer
//! actually understands), the blind-spot catalog (patterns gnx records
//! but cannot resolve), confidence thresholds (pulled from the
//! authoritative const sources to avoid duplicate truth), and basic
//! graph.bin health (exists / size).
//!
//! Two output formats: `compact` (YAML-ish, default; LLM-readable) and
//! `json`. Both expose the same schema.

use crate::engine::Engine;
use crate::graph_path;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_analyzer::framework_confidence as fc;
use graph_nexus_core::{GnxError, HIGH_TRUST_CONFIDENCE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Output format: compact (YAML-ish) or json.
    #[arg(long, default_value = "compact")]
    pub format: String,
}

struct FrameworkPattern {
    language: &'static str,
    framework: &'static str,
    pattern: &'static str,
    reason_tag: &'static str,
    confidence: f32,
}

const FRAMEWORK_COVERAGE: &[FrameworkPattern] = &[
    // Python
    FrameworkPattern {
        language: "Python",
        framework: "FastAPI",
        pattern: r#"Depends(<ident>)"#,
        reason_tag: "fastapi-depends",
        confidence: fc::FASTAPI_DEPENDS,
    },
    FrameworkPattern {
        language: "Python",
        framework: "FastAPI",
        pattern: r#"@app.<method>("/path")"#,
        reason_tag: "fastapi-route-<method>",
        confidence: fc::FASTAPI_ROUTE,
    },
    FrameworkPattern {
        language: "Python",
        framework: "Django",
        pattern: r#"urlpatterns = [path("/x", handler)]"#,
        reason_tag: "django-url-path",
        confidence: fc::DJANGO_URL,
    },
    FrameworkPattern {
        language: "Python",
        framework: "Django",
        pattern: r#"@receiver(<signal>)"#,
        reason_tag: "django-signal-receiver",
        confidence: fc::DJANGO_SIGNAL,
    },
    FrameworkPattern {
        language: "Python",
        framework: "Django",
        pattern: r#"<signal>.connect(<ident>)"#,
        reason_tag: "django-signal-connect",
        confidence: fc::DJANGO_SIGNAL,
    },
    FrameworkPattern {
        language: "Python",
        framework: "Celery",
        pattern: r#"@shared_task / @app.task / @celery.task"#,
        reason_tag: "celery-task",
        confidence: fc::CELERY_TASK,
    },
    FrameworkPattern {
        language: "Python",
        framework: "(reflection)",
        pattern: r#"getattr(self, <ident>)()"#,
        reason_tag: "reflection-getattr-fanout",
        confidence: fc::FANOUT_BASE,
    },
    // Rust
    FrameworkPattern {
        language: "Rust",
        framework: "Axum",
        pattern: r#"Router::route(_, METHOD(handler))"#,
        reason_tag: "axum-route-handler",
        confidence: fc::AXUM_ROUTE,
    },
    FrameworkPattern {
        language: "Rust",
        framework: "Actix",
        pattern: r#"#[get/post/...]("/path")"#,
        reason_tag: "actix-route-<method>",
        confidence: fc::ACTIX_ROUTE,
    },
    // TypeScript
    FrameworkPattern {
        language: "TypeScript",
        framework: "Express",
        pattern: r#"app.METHOD(path, handler)"#,
        reason_tag: "express-route-handler",
        confidence: fc::EXPRESS_ROUTE,
    },
    FrameworkPattern {
        language: "TypeScript",
        framework: "NestJS",
        pattern: r#"@Controller + @Get/Post/... methods"#,
        reason_tag: "nestjs-route-handler",
        confidence: fc::NESTJS_ROUTE,
    },
    // Java
    FrameworkPattern {
        language: "Java",
        framework: "Spring",
        pattern: r#"@Autowired field/setter"#,
        reason_tag: "spring-autowired",
        confidence: fc::SPRING_AUTOWIRED,
    },
    FrameworkPattern {
        language: "Java",
        framework: "Spring",
        pattern: r#"@RestController + @GetMapping methods"#,
        reason_tag: "spring-route-handler",
        confidence: fc::SPRING_ROUTE,
    },
];

struct BlindSpotKind {
    language: &'static str,
    kind: &'static str,
    pattern: &'static str,
}

const BLIND_SPOT_CATALOG: &[BlindSpotKind] = &[
    BlindSpotKind {
        language: "Python",
        kind: "python-eval",
        pattern: "eval(...)",
    },
    BlindSpotKind {
        language: "Python",
        kind: "python-exec",
        pattern: "exec(...)",
    },
    BlindSpotKind {
        language: "Python",
        kind: "python-compile",
        pattern: "compile(...)",
    },
    BlindSpotKind {
        language: "Python",
        kind: "python-dynamic-import",
        pattern: "importlib.import_module(...)",
    },
    BlindSpotKind {
        language: "Python",
        kind: "python-builtin-import",
        pattern: "__import__(...)",
    },
    BlindSpotKind {
        language: "Python",
        kind: "python-cross-getattr",
        pattern: "getattr(<not-self>, name)()",
    },
];

/// Tally live blind-spot sites from the loaded graph: aggregate by `kind`,
/// and surface the top-5 files by total occurrence count. Returns `null`
/// when the graph cannot be loaded (missing file, magic/version mismatch)
/// so doctor stays useful on a fresh checkout with no graph yet.
fn live_blind_spots(graph_path: &Path) -> serde_json::Value {
    let Ok(engine) = Engine::load(graph_path) else {
        return serde_json::Value::Null;
    };
    let Ok(graph) = engine.graph() else {
        return serde_json::Value::Null;
    };

    let mut by_kind: HashMap<&str, usize> = HashMap::new();
    let mut by_file: HashMap<&str, usize> = HashMap::new();
    for bs in graph.blind_spots.iter() {
        let kind = bs.kind.resolve(&graph.string_pool);
        let path = bs.file_path.resolve(&graph.string_pool);
        *by_kind.entry(kind).or_insert(0) += 1;
        *by_file.entry(path).or_insert(0) += 1;
    }

    // by_kind: sort by count desc, then kind asc for deterministic output.
    let mut kind_entries: Vec<(&str, usize)> = by_kind.into_iter().collect();
    kind_entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let by_kind_json: serde_json::Map<String, serde_json::Value> = kind_entries
        .iter()
        .map(|(k, n)| ((*k).to_string(), serde_json::json!(n)))
        .collect();

    // top_files: top-5 by count desc, then path asc.
    let mut file_entries: Vec<(&str, usize)> = by_file.into_iter().collect();
    file_entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let top_files: Vec<serde_json::Value> = file_entries
        .iter()
        .take(5)
        .map(|(path, count)| serde_json::json!({"file": path, "count": count}))
        .collect();

    serde_json::json!({
        "total": graph.blind_spots.len(),
        "by_kind": serde_json::Value::Object(by_kind_json),
        "top_files": top_files,
    })
}

/// Build the doctor payload as a JSON value (schema shared between
/// compact + json output paths).
fn build_payload(graph_path: &std::path::Path) -> serde_json::Value {
    let exists = graph_path.exists();
    let size_bytes = std::fs::metadata(graph_path).map(|m| m.len()).ok();
    let live_bs = live_blind_spots(graph_path);

    let framework_coverage: Vec<serde_json::Value> = FRAMEWORK_COVERAGE
        .iter()
        .map(|p| {
            serde_json::json!({
                "language": p.language,
                "framework": p.framework,
                "pattern": p.pattern,
                "reason_tag": p.reason_tag,
                "confidence": p.confidence,
            })
        })
        .collect();

    let blind_spot_catalog: Vec<serde_json::Value> = BLIND_SPOT_CATALOG
        .iter()
        .map(|b| {
            serde_json::json!({
                "language": b.language,
                "kind": b.kind,
                "pattern": b.pattern,
            })
        })
        .collect();

    serde_json::json!({
        "gnx_version": env!("CARGO_PKG_VERSION"),
        "graph": {
            "path": graph_path.display().to_string(),
            "exists": exists,
            "size_bytes": size_bytes,
        },
        "framework_coverage": framework_coverage,
        "blind_spot_catalog": blind_spot_catalog,
        "live_blind_spots": live_bs,
        "confidence_thresholds": {
            "high_trust_only": HIGH_TRUST_CONFIDENCE,
            "fanout_base": fc::FANOUT_BASE,
        },
    })
}

/// Render the payload in compact YAML-ish format. The framework_coverage
/// and blind_spot_catalog tables print one row per line (CSV-style) so
/// LLMs can scan the contract without parsing nested YAML.
fn render_compact(value: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "gnx_version: {}\n",
        value["gnx_version"].as_str().unwrap_or("?")
    ));
    out.push_str("graph:\n");
    out.push_str(&format!(
        "  path: {}\n",
        value["graph"]["path"].as_str().unwrap_or("?")
    ));
    out.push_str(&format!(
        "  exists: {}\n",
        value["graph"]["exists"].as_bool().unwrap_or(false)
    ));
    match value["graph"]["size_bytes"].as_u64() {
        Some(n) => out.push_str(&format!("  size_bytes: {n}\n")),
        None => out.push_str("  size_bytes: null\n"),
    }
    out.push('\n');

    let fc_arr = value["framework_coverage"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    out.push_str(&format!(
        "framework_coverage[{}]{{language,framework,pattern,reason_tag,confidence}}:\n",
        fc_arr.len()
    ));
    for row in &fc_arr {
        out.push_str(&format!(
            "  {},{},{},{},{:.2}\n",
            row["language"].as_str().unwrap_or(""),
            row["framework"].as_str().unwrap_or(""),
            row["pattern"].as_str().unwrap_or(""),
            row["reason_tag"].as_str().unwrap_or(""),
            row["confidence"].as_f64().unwrap_or(0.0),
        ));
    }
    out.push('\n');

    let bs_arr = value["blind_spot_catalog"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    out.push_str(&format!(
        "blind_spot_catalog[{}]{{language,kind,pattern}}:\n",
        bs_arr.len()
    ));
    for row in &bs_arr {
        out.push_str(&format!(
            "  {},{},{}\n",
            row["language"].as_str().unwrap_or(""),
            row["kind"].as_str().unwrap_or(""),
            row["pattern"].as_str().unwrap_or(""),
        ));
    }
    out.push('\n');

    // live_blind_spots — only shown when graph.bin was loadable. On a
    // fresh checkout with no graph, the section is silently skipped so
    // doctor remains useful before the first `gnx analyze`.
    let lbs = &value["live_blind_spots"];
    if lbs.is_object() {
        out.push_str("live_blind_spots:\n");
        out.push_str(&format!(
            "  total: {}\n",
            lbs["total"].as_u64().unwrap_or(0)
        ));
        out.push_str("  by_kind:\n");
        if let Some(by_kind) = lbs["by_kind"].as_object() {
            // JSON map is alphabetical (BTreeMap); re-sort to count-desc
            // for LLM readability (most-frequent kind first).
            let mut entries: Vec<(&String, u64)> = by_kind
                .iter()
                .map(|(k, v)| (k, v.as_u64().unwrap_or(0)))
                .collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            for (kind, count) in entries {
                out.push_str(&format!("    {kind}: {count}\n"));
            }
        }
        out.push_str("  top_files:\n");
        if let Some(rows) = lbs["top_files"].as_array() {
            for row in rows {
                out.push_str(&format!(
                    "    - file: {}\n      count: {}\n",
                    row["file"].as_str().unwrap_or(""),
                    row["count"].as_u64().unwrap_or(0),
                ));
            }
        }
        out.push('\n');
    }

    out.push_str("confidence_thresholds:\n");
    out.push_str(&format!(
        "  high_trust_only: {:.2}  # edges below this filtered by --high-trust-only flag\n",
        value["confidence_thresholds"]["high_trust_only"]
            .as_f64()
            .unwrap_or(0.0)
    ));
    out.push_str(&format!(
        "  fanout_base: {:.2}  # divided by sqrt(N) for getattr fan-out\n",
        value["confidence_thresholds"]["fanout_base"]
            .as_f64()
            .unwrap_or(0.0)
    ));
    out
}

pub fn run(args: DoctorArgs, graph_arg: &Path) -> Result<(), GnxError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let graph_path = graph_path::resolve(graph_arg, &cwd);
    let payload = build_payload(&graph_path);

    match args.format.as_str() {
        "json" => emit(&payload, OutputFormat::Json),
        _ => {
            // compact YAML-ish — print directly, bypassing emit() because the
            // shape is non-JSON.
            print!("{}", render_compact(&payload));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framework_coverage_table_has_all_documented_patterns() {
        // Sanity guard against accidental deletion of rows.
        assert_eq!(FRAMEWORK_COVERAGE.len(), 13);
        let tags: Vec<&str> = FRAMEWORK_COVERAGE.iter().map(|p| p.reason_tag).collect();
        assert!(tags.contains(&"fastapi-depends"));
        assert!(tags.contains(&"fastapi-route-<method>"));
        assert!(tags.contains(&"django-url-path"));
        assert!(tags.contains(&"django-signal-receiver"));
        assert!(tags.contains(&"django-signal-connect"));
        assert!(tags.contains(&"celery-task"));
        assert!(tags.contains(&"reflection-getattr-fanout"));
        assert!(tags.contains(&"axum-route-handler"));
        assert!(tags.contains(&"actix-route-<method>"));
        assert!(tags.contains(&"express-route-handler"));
        assert!(tags.contains(&"nestjs-route-handler"));
        assert!(tags.contains(&"spring-autowired"));
        assert!(tags.contains(&"spring-route-handler"));
    }

    #[test]
    fn blind_spot_catalog_has_all_six_python_kinds() {
        assert_eq!(BLIND_SPOT_CATALOG.len(), 6);
        let kinds: Vec<&str> = BLIND_SPOT_CATALOG.iter().map(|b| b.kind).collect();
        for expected in [
            "python-eval",
            "python-exec",
            "python-compile",
            "python-dynamic-import",
            "python-builtin-import",
            "python-cross-getattr",
        ] {
            assert!(kinds.contains(&expected), "missing kind: {expected}");
        }
    }

    #[test]
    fn build_payload_uses_authoritative_threshold_constants() {
        let v = build_payload(std::path::Path::new("/tmp/does/not/exist"));
        assert_eq!(
            v["confidence_thresholds"]["high_trust_only"]
                .as_f64()
                .unwrap() as f32,
            HIGH_TRUST_CONFIDENCE
        );
        assert_eq!(
            v["confidence_thresholds"]["fanout_base"].as_f64().unwrap() as f32,
            fc::FANOUT_BASE
        );
        assert_eq!(v["graph"]["exists"], false);
    }

    #[test]
    fn build_payload_emits_null_live_blind_spots_when_graph_missing() {
        let v = build_payload(std::path::Path::new("/tmp/does/not/exist"));
        assert!(
            v["live_blind_spots"].is_null(),
            "expected null live_blind_spots when graph.bin absent, got: {}",
            v["live_blind_spots"]
        );
    }

    #[test]
    fn render_compact_emits_required_section_headers() {
        let v = build_payload(std::path::Path::new("/tmp/x"));
        let s = render_compact(&v);
        assert!(s.contains("framework_coverage[13]"));
        assert!(s.contains("blind_spot_catalog[6]"));
        assert!(s.contains("confidence_thresholds:"));
        assert!(s.contains("fastapi-depends"));
        assert!(s.contains("python-eval"));
    }

    #[test]
    fn render_compact_skips_live_blind_spots_section_when_null() {
        // No graph.bin at this path => live_blind_spots is null => the
        // section must be silently omitted so doctor stays useful on a
        // fresh checkout before the first `gnx analyze`.
        let v = build_payload(std::path::Path::new("/tmp/does/not/exist"));
        let s = render_compact(&v);
        assert!(
            !s.contains("live_blind_spots"),
            "compact output should omit live_blind_spots when null; got:\n{s}"
        );
    }
}
