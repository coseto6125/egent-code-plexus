//! `ecp admin index --dump-resolver <out>` writes a non-empty resolver JSONL
//! whose lines deserialize as binding decisions. Pins the CLI wiring (the
//! GraphBuilder-level round-trip is covered in builder.rs unit tests).

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn admin_index_dump_resolver_writes_jsonl() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(
        repo.join("src/a.ts"),
        "import { helper } from \"./b\";\nexport function main() { helper(); }\n",
    )
    .expect("write a.ts");
    std::fs::write(
        repo.join("src/b.ts"),
        "export function helper() { return 1; }\n",
    )
    .expect("write b.ts");

    // Commit the fixture so it's a real tree the analyzer can walk.
    for args in [vec!["init", "-q", "-b", "main"], vec!["add", "-A"]] {
        assert!(Command::new("git")
            .args(&args)
            .current_dir(repo)
            .output()
            .unwrap()
            .status
            .success());
    }
    assert!(Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "v1"
        ])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success());

    let dump = repo.join("dump.jsonl");
    let out = Command::new(ecp_bin())
        .args([
            "admin",
            "index",
            "--repo",
            repo.to_str().unwrap(),
            "--dump-resolver",
            dump.to_str().unwrap(),
        ])
        .env("HOME", repo)
        .output()
        .expect("run admin index --dump-resolver");

    assert!(
        out.status.success(),
        "admin index --dump-resolver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body =
        std::fs::read_to_string(&dump).unwrap_or_else(|e| panic!("dump file not written: {e}"));
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "dump JSONL is empty");
    for line in &lines {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSONL line {line:?}: {e}"));
        assert!(v.get("src_file").is_some(), "missing src_file: {line}");
        assert!(v.get("name").is_some(), "missing name: {line}");
    }
}
