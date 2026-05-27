use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn invocation_appends_one_cli_telemetry_line() {
    let tmp = std::env::temp_dir().join(format!("ecp-gain-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = Command::new(ecp_bin())
        .args(["find", "definitely_no_such_symbol_xyz"])
        .current_dir(&tmp)
        .env("HOME", &tmp)
        .env_remove("ECP_NO_TELEMETRY")
        .output()
        .unwrap();
    let _ = out; // command may fail (no graph) — we only assert telemetry wrote
    let tel_root = tmp.join(".ecp/telemetry");
    let mut found = false;
    if let Ok(entries) = std::fs::read_dir(&tel_root) {
        for e in entries.flatten() {
            let f = e.path().join("cli-calls.jsonl");
            if f.exists() {
                let body = std::fs::read_to_string(&f).unwrap();
                assert!(body.lines().count() >= 1, "expected >=1 telemetry line");
                assert!(body.contains(r#""source":"cli""#));
                found = true;
            }
        }
    }
    assert!(found, "no cli-calls.jsonl written under {tel_root:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn gain_json_aggregates_a_fixture() {
    let tmp = std::env::temp_dir().join(format!("ecp-gain-json-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/myrepo__deadbeef");
    std::fs::create_dir_all(&tel).unwrap();
    let lines = [
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}"#,
        r#"{"ts":"2026-05-27T07:01:00Z","tool":"inspect","duration_ms":48,"ok":true,"source":"cli","error_kind":null}"#,
        r#"{"ts":"2026-05-27T07:02:00Z","tool":"cypher","duration_ms":9,"ok":false,"source":"cli","error_kind":"cypher-parse"}"#,
    ].join("\n");
    std::fs::write(tel.join("cli-calls.jsonl"), lines).unwrap();
    let out = Command::new(ecp_bin())
        .args([
            "gain",
            "--format",
            "json",
            "--telemetry-dir",
            tel.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["total"], 3);
    let by = v["by_command"].as_array().unwrap();
    let inspect = by.iter().find(|c| c["cmd"] == "inspect").unwrap();
    assert_eq!(inspect["count"], 2);
    assert_eq!(v["errors_by_kind"]["cypher-parse"], 1);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn gain_text_dashboard_is_plain_when_piped() {
    let tmp = std::env::temp_dir().join(format!("ecp-gain-txt-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__1");
    std::fs::create_dir_all(&tel).unwrap();
    std::fs::write(
        tel.join("cli-calls.jsonl"),
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}"#,
    ).unwrap();
    let out = Command::new(ecp_bin())
        .args(["gain", "--telemetry-dir", tel.to_str().unwrap()])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(!s.contains('\x1b'), "piped output must be color-free");
    assert!(s.contains("Usage"));
    assert!(s.contains("inspect"));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn gain_failures_lists_only_errors() {
    let tmp = std::env::temp_dir().join(format!("ecp-gain-fail-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__2");
    std::fs::create_dir_all(&tel).unwrap();
    let lines = [
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}"#,
        r#"{"ts":"2026-05-27T07:02:00Z","tool":"cypher","duration_ms":9,"ok":false,"source":"cli","error_kind":"cypher-parse"}"#,
    ].join("\n");
    std::fs::write(tel.join("cli-calls.jsonl"), lines).unwrap();
    let out = Command::new(ecp_bin())
        .args([
            "gain",
            "--failures",
            "--telemetry-dir",
            tel.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("cypher-parse"));
    assert!(
        !s.contains("inspect"),
        "failures view must omit successful commands"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
