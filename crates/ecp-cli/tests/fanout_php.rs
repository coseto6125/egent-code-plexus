//! Integration test: a PHP static method reached only through a const-map
//! dispatch (`$m = self::MAP[$type] ?? null; Rows::$m($p)`) keeps its caller
//! in `ecp impact`, tagged as a low-confidence fan-out, instead of looking
//! dead.

mod common;

use common::{ecp_bin, init_and_analyze, write};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

const ROWS: &str = r#"<?php
namespace App;

final class Rows
{
    public static function hero(array $p): array { return $p; }
    public static function story(array $p): array { return $p; }
}
"#;

const API: &str = r#"<?php
namespace App;

class Api
{
    protected const MAP = ['H' => 'hero', 'S' => 'story'];

    public function dispatch(string $type, array $p): ?array
    {
        $m = self::MAP[$type] ?? null;
        return Rows::$m($p);
    }
}
"#;

fn impact_upstream(repo: &Path, target: &str) -> Value {
    let out = Command::new(ecp_bin())
        .args([
            "impact",
            "--target",
            target,
            "--direction",
            "upstream",
            "--repo",
            ".",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("impact failed to spawn");
    assert!(
        out.status.success(),
        "impact failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact json")
}

#[test]
fn test_impact_const_map_dispatch_lists_dispatcher_as_low_confidence_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(repo, "src/Rows.php", ROWS);
    write(repo, "src/Api.php", API);
    init_and_analyze(repo);

    let json = impact_upstream(repo, "hero");
    let callers = json["impact"].as_array().cloned().unwrap_or_default();
    let dispatcher = callers
        .iter()
        .find(|c| c["name"] == "dispatch")
        .unwrap_or_else(|| panic!("dispatch must call hero through the map: {json}"));
    assert_eq!(dispatcher["viaReason"], "php-const-map-dispatch", "{json}");
    let confidence = dispatcher["viaConfidence"].as_f64().unwrap();
    assert!(
        confidence > 0.0 && confidence < 0.7,
        "a fan-out edge stays below the high-trust bar: {confidence}"
    );
    assert_eq!(dispatcher["filePath"], "src/Api.php", "{json}");
}
