//! `ecp impact --literal` scans PathLiteral nodes only, and only path-shaped
//! strings become PathLiteral. A bare `site_count: 0` for any other value reads
//! as "unused", so the payload carries a `note` for it.

mod common;

use common::{ecp_bin, init_and_analyze, write};
use std::path::Path;
use std::process::Command;

fn literal(repo: &Path, value: &str) -> serde_json::Value {
    let out = Command::new(ecp_bin())
        .args([
            "impact",
            "--literal",
            value,
            "--repo",
            ".",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "a.py",
        "def f():\n    return open('data/x.json')\n",
    );
    init_and_analyze(tmp.path());
    tmp
}

#[test]
fn test_impact_literal_non_path_value_carries_note() {
    let tmp = repo();
    let json = literal(tmp.path(), "descriptionBG");
    assert_eq!(json["site_count"], 0, "{json}");
    let note = json["note"].as_str().unwrap_or_else(|| panic!("{json}"));
    // Contract: the note says the value is not path-shaped and that 0 sites is not proof of non-use.
    assert!(note.contains("not path-shaped"), "{note}");
    assert!(note.contains("does not mean unused"), "{note}");
    assert!(note.contains("grep"), "{note}");
}

#[test]
fn test_impact_literal_path_shaped_zero_sites_has_no_note() {
    let tmp = repo();
    let json = literal(tmp.path(), "missing/none.json");
    assert_eq!(json["site_count"], 0, "{json}");
    assert!(json.get("note").is_none(), "{json}");
}

#[test]
fn test_impact_literal_ext_change_value_with_sites_has_no_note() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "a.rs",
        "pub fn backup(p: &std::path::Path) -> std::path::PathBuf { p.with_extension(\"bak\") }\n",
    );
    init_and_analyze(tmp.path());
    let json = literal(tmp.path(), "bak");
    assert!(json["site_count"].as_u64().unwrap() >= 1, "{json}");
    assert!(json.get("note").is_none(), "{json}");
}

/// Stands for: a module-top literal's edge starts at the `File` node (slice
/// P6), so the site carries its sink, and `enclosing` stays null rather than
/// naming the file as if it were a symbol.
#[test]
fn test_impact_literal_module_top_site_has_sink_and_no_enclosing() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "a.py",
        "TOP = open('conf/top.json')\n\ndef f():\n    return open('conf/top.json')\n",
    );
    init_and_analyze(tmp.path());
    let json = literal(tmp.path(), "conf/top.json");
    let sites = json["sites"].as_array().unwrap_or_else(|| panic!("{json}"));
    let at = |line: u64| {
        sites
            .iter()
            .find(|s| s["line"] == line)
            .unwrap_or_else(|| panic!("no site at line {line}: {json}"))
    };
    let top = at(1);
    assert!(top["enclosing"].is_null(), "{json}");
    assert_ne!(top["sink_reason"], "", "{json}");
    assert_eq!(at(4)["enclosing"], "f", "{json}");
}
