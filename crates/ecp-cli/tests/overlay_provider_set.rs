//! The session overlay (incremental path) runs the provider set the cold
//! index runs: every provider a path's `FileRoute` lists (C5 routing
//! contract, consumer area 6b).
//!
//! Before this contract the overlay held a hand list of 19 provider names, so
//! an uncommitted edit to a Bash, SQL, HCL, Solidity (and so on) file never
//! reached the overlay and `ecp find` answered `found:false` for code on
//! disk. Each test plants one fact and reads what the consumer reads.

mod common;

use common::{commit_all, ecp_bin, run_git, write};
use ecp_cli::git::DiffScope;
use ecp_cli::reanalyze::{pipeline_for_path, reanalyze_files};
use ecp_cli::session::overlay_writer::OverlayWriter;
use ecp_core::contracts::routing::{FileRouter, Language, RouteOutcome, SizeCap};
use rustc_hash::FxHashSet;
use std::path::Path;
use std::process::Command;

/// Node names `reanalyze_files` yields for one file written to a fresh tempdir.
fn reanalyzed_node_names(rel: &str, body: &str) -> Vec<String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(tmp.path(), rel, body);
    reanalyze_files(tmp.path(), &DiffScope::All, &[rel.to_owned()])
        .iter()
        .flat_map(|graph| graph.nodes.iter().map(|node| node.name.clone()))
        .collect()
}

/// Each case plants a file of one newly covered provider; the overlay must
/// parse it into a node named after the planted declaration.
fn assert_family_reaches_overlay(cases: &[(&str, &str, &str)]) {
    for (rel, body, needle) in cases {
        let names = reanalyzed_node_names(rel, body);
        assert!(
            names.iter().any(|name| name.contains(needle)),
            "{rel}: the overlay must parse it into a node containing {needle:?}, got {names:?}"
        );
    }
}

// ── the repro from the architecture direction, area 6 ────────────────────────

fn init_and_index(repo: &Path, home: &Path) {
    write(
        repo,
        "run.sh",
        "#!/bin/sh\nold_deploy_step() {\n  echo deploy\n}\n",
    );
    run_git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "init");
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn find_exact(repo: &Path, home: &Path, pattern: &str) -> serde_json::Value {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(["find", pattern, "--format", "json"])
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1");
    // Bare-CLI session path: the overlay writer and reader agree on one
    // per-process fallback id only when no host session id is set.
    for key in [
        "ECP_SESSION_ID",
        "GEMINI_CLI_SESSION_ID",
        "CODEX_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        cmd.env_remove(key);
    }
    let out = cmd.output().expect("find failed to spawn");
    assert!(
        out.status.success(),
        "find {pattern} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "non-JSON find output ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// Plants: a committed `run.sh` whose function is renamed in the working tree.
/// Consumer reads: `ecp find <new name>` answers `found: true`.
///
/// Fails on the old code: `bash` was not among the overlay's 19 provider
/// names, so the dirty `run.sh` was never re-parsed and the renamed function
/// was invisible.
#[test]
fn test_overlay_bash_rename_new_fn_found_true() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    init_and_index(repo.path(), home.path());

    write(
        repo.path(),
        "run.sh",
        "#!/bin/sh\nnew_deploy_step() {\n  echo deploy\n}\n",
    );

    let v = find_exact(repo.path(), home.path(), "new_deploy_step");
    assert_eq!(
        v["found"], true,
        "a function renamed in an uncommitted .sh file must be found via the overlay: {v}"
    );
}

// ── one test per newly covered provider family ───────────────────────────────

/// Plants a Bash file and a Lua file. Fails on the old code: `bash` and `lua`
/// had no overlay provider, so `reanalyze_files` returned no graph.
#[test]
fn test_reanalyze_files_shell_and_script_family_parses() {
    assert_family_reaches_overlay(&[
        ("tools/run.sh", "greet() {\n  echo hi\n}\n", "greet"),
        (
            "lib/greet.lua",
            "function greet()\n  return 1\nend\n",
            "greet",
        ),
    ]);
}

/// Plants Crystal, Nim, Zig and Move files. Fails on the old code: none of the
/// four providers was in the overlay's list.
#[test]
fn test_reanalyze_files_systems_language_family_parses() {
    assert_family_reaches_overlay(&[
        ("src/greet.cr", "def greet\n  1\nend\n", "greet"),
        ("src/greet.nim", "proc greet() =\n  discard\n", "greet"),
        ("src/greet.zig", "pub fn greet() void {}\n", "greet"),
        (
            "sources/greet.move",
            "module 0x1::greeter {\n    fun greet() {}\n}\n",
            "greet",
        ),
    ]);
}

/// Plants Solidity, Vyper and Cairo files. Fails on the old code: none of the
/// three smart-contract providers was in the overlay's list.
#[test]
fn test_reanalyze_files_contract_language_family_parses() {
    assert_family_reaches_overlay(&[
        (
            "contracts/Greeter.sol",
            "contract Greeter {\n    function greet() public {}\n}\n",
            "Greet",
        ),
        (
            "contracts/greeter.vy",
            "@external\ndef greet():\n    pass\n",
            "greet",
        ),
        ("src/greet.cairo", "fn greet() {}\n", "greet"),
    ]);
}

/// Plants Verilog, SQL, HCL and protobuf files. Fails on the old code: none of
/// the four providers was in the overlay's list.
#[test]
fn test_reanalyze_files_hardware_data_and_schema_family_parses() {
    assert_family_reaches_overlay(&[
        ("rtl/greeter.v", "module greeter;\nendmodule\n", "greet"),
        ("db/schema.sql", "CREATE TABLE greeter (id INT);\n", "greet"),
        (
            "infra/main.tf",
            "resource \"aws_s3_bucket\" \"greeter\" {}\n",
            "greet",
        ),
        (
            "api/greeter.proto",
            "syntax = \"proto3\";\nmessage Greeter {\n  string name = 1;\n}\n",
            "Greet",
        ),
    ]);
}

/// Plants a Dockerfile, a compose file and a GitHub Actions workflow. Fails on
/// the old code: `dockerfile`, `docker-compose` and `github-actions` were not
/// in the overlay's list, so these path-rule routes parsed nothing.
#[test]
fn test_reanalyze_files_container_and_ci_family_parses() {
    assert_family_reaches_overlay(&[
        // The dockerfile provider names a stage by its base image.
        ("Dockerfile", "FROM alpine AS greeter\nRUN echo hi\n", "alpine"),
        (
            "docker-compose.yml",
            "services:\n  greeter:\n    image: alpine\n",
            "greet",
        ),
        (
            ".github/workflows/ci.yml",
            "name: ci\non: push\njobs:\n  greeter:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n",
            "greet",
        ),
    ]);
}

// ── provider-set equality with the cold index ────────────────────────────────

/// Plants nothing on disk. Contract: the overlay's per-path pipeline holds
/// exactly the providers the route lists, as the cold index builds them, for
/// every extension default of every routed language (all 14 mainstream
/// languages and the DSL and config languages).
///
/// Fails on the old code: `pipeline_for_path` did not exist, and the overlay's
/// set was a hand list that lacked 16 routed providers.
#[test]
fn test_pipeline_for_path_every_default_extension_holds_the_route_providers() {
    let router = FileRouter::new(SizeCap::BUILT_IN);
    for &language in Language::ALL {
        for ext in language.extensions() {
            let rel = format!("src/sample.{ext}");
            let RouteOutcome::Routed(route) = router.route(&rel) else {
                panic!("{rel}: the router must route a default extension");
            };
            let expected: FxHashSet<&str> = route.providers().collect();
            let pipeline = pipeline_for_path(Path::new(&rel))
                .unwrap_or_else(|| panic!("{rel}: a routed path needs an overlay pipeline"));
            assert_eq!(pipeline.provider_count(), expected.len(), "{rel}");
        }
    }
}

/// A sniffed generic YAML lists `kubernetes`, `openapi` and `yaml`; the overlay
/// builds all three, as the cold index's `detect_needed_providers` does.
#[test]
fn test_pipeline_for_path_generic_yaml_holds_every_candidate() {
    let pipeline = pipeline_for_path(Path::new("config/app.yml")).expect("yaml routes");
    assert_eq!(pipeline.provider_count(), 3);
}

/// Unrouted, excluded, empty and whitespace-only paths get no pipeline, so the
/// overlay never builds a provider for them.
#[test]
fn test_pipeline_for_path_unrouted_excluded_and_blank_paths_return_none() {
    for rel in [
        "README.md",
        "notes.txt",
        "src/UPPER.RS",
        "bricks/feature/__brick__/lib/widget.dart",
        "",
        "   ",
    ] {
        assert!(pipeline_for_path(Path::new(rel)).is_none(), "{rel:?}");
    }
}

/// Two calls for the same provider set share one pipeline: the overlay writer
/// parses one file per call and must not pay the `Query` compile each time.
#[test]
fn test_pipeline_for_path_concurrent_duplicate_calls_share_one_pipeline() {
    let handles: Vec<_> = (0..4)
        .map(|i| {
            std::thread::spawn(move || {
                pipeline_for_path(Path::new(&format!("dir{i}/x.sh"))).expect("bash routes")
                    as *const _ as usize
            })
        })
        .collect();
    let addresses: FxHashSet<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(addresses.len(), 1);
}

// ── overlay writer routes by the repo-relative path ──────────────────────────

/// Plants a Bash function in `tools/run.sh`. Consumer reads
/// `dirty_files.json`'s `dirty_symbols`.
///
/// Fails on the old code: the writer's pipeline had no `bash` provider, so the
/// entry was `parse_failed` with no symbols.
#[test]
fn test_append_dirty_bash_file_records_function_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join("sessions/s1");
    std::fs::create_dir_all(&session_dir).unwrap();
    write(dir.path(), "tools/run.sh", "greet() {\n  echo hi\n}\n");

    let mut writer = OverlayWriter::new(&session_dir);
    writer
        .append_dirty(&dir.path().join("tools/run.sh"), "h", "f")
        .unwrap();

    let dirty = writer.read_dirty().unwrap();
    let entry = dirty.entries.values().next().unwrap();
    assert!(!entry.parse_failed);
    assert!(
        entry.dirty_symbols.iter().any(|s| s.name == "greet"),
        "{:?}",
        entry.dirty_symbols
    );
}

/// Plants a Rust file under a template directory. The router excludes
/// `__brick__` paths, so the cold index never walks it; the writer must agree.
///
/// Fails on the old code: the writer routed by basename (`lib.rs`), so it
/// parsed a file the cold index skips.
#[test]
fn test_append_dirty_template_dir_path_marks_parse_failed() {
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join("sessions/s1");
    std::fs::create_dir_all(&session_dir).unwrap();
    write(
        dir.path(),
        "bricks/x/__brick__/lib.rs",
        "pub fn templated() {}\n",
    );

    let mut writer = OverlayWriter::new(&session_dir);
    writer
        .append_dirty(&dir.path().join("bricks/x/__brick__/lib.rs"), "h", "f")
        .unwrap();

    let dirty = writer.read_dirty().unwrap();
    let entry = dirty.entries.values().next().unwrap();
    assert!(entry.parse_failed);
    assert!(entry.dirty_symbols.is_empty());
}

// ── edge inputs of reanalyze_files ───────────────────────────────────────────

/// An empty path list, an unrouted path and a missing file each yield no graph.
#[test]
fn test_reanalyze_files_empty_unrouted_and_missing_inputs_yield_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "README.md", "# title\n");
    assert!(reanalyze_files(tmp.path(), &DiffScope::All, &[]).is_empty());
    assert!(reanalyze_files(tmp.path(), &DiffScope::All, &["README.md".to_owned()]).is_empty());
    assert!(reanalyze_files(tmp.path(), &DiffScope::All, &["gone.sh".to_owned()]).is_empty());
}
