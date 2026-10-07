//! FU-2026-10-08-2986d0434f4b: the ambiguity caveat on `ecp impact` describes
//! the INDEX's view of the name. The resolver suppressed bare calls to `dupfn`
//! because two definitions existed when the graph was built, so an uncommitted
//! rename of one definition must not make the caveat vanish: the surviving
//! definition's caller set is still a lower bound.
//!
//! The caveat logic is language-agnostic, so one table runs the rename scenario
//! over the 14 mainstream language providers. The single-language tests pin the
//! neighbouring states (no edit, edit adds a def, filters, direction).

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::fs;
use std::process::Command;

const SESSION: &str = "impact-overlay-caveat-matrix-session";
const NAME: &str = "dupfn";

/// (language, extension, definition source with `{N}` for the name, caller source).
/// Each definition file defines `{N}` once; the caller file calls it bare.
const LANGS: [(&str, &str, &str, &str); 14] = [
    (
        "typescript",
        "ts",
        "export function {N}() {}\n",
        "function run() { dupfn(); }\n",
    ),
    (
        "javascript",
        "js",
        "function {N}() {}\n",
        "function run() { dupfn(); }\n",
    ),
    (
        "python",
        "py",
        "def {N}():\n    pass\n",
        "def run():\n    dupfn()\n",
    ),
    (
        "java",
        "java",
        "class K@ { static void {N}() {} }\n",
        "class Run { void go() { dupfn(); } }\n",
    ),
    ("kotlin", "kt", "fun {N}() {}\n", "fun run() { dupfn() }\n"),
    (
        "csharp",
        "cs",
        "class K@ { static void {N}() {} }\n",
        "class Run { void Go() { dupfn(); } }\n",
    ),
    (
        "go",
        "go",
        "package p\n\nfunc {N}() {}\n",
        "package p\n\nfunc run() { dupfn() }\n",
    ),
    (
        "rust",
        "rs",
        "pub fn {N}() {}\n",
        "pub fn run() { dupfn(); }\n",
    ),
    (
        "php",
        "php",
        "<?php\nfunction {N}() {}\n",
        "<?php\nfunction run() { dupfn(); }\n",
    ),
    ("ruby", "rb", "def {N}\nend\n", "def run\n  dupfn()\nend\n"),
    (
        "swift",
        "swift",
        "func {N}() {}\n",
        "func run() { dupfn() }\n",
    ),
    (
        "c",
        "c",
        "void {N}(void) {}\n",
        "void run(void) { dupfn(); }\n",
    ),
    ("cpp", "cpp", "void {N}() {}\n", "void run() { dupfn(); }\n"),
    (
        "dart",
        "dart",
        "void {N}() {}\n",
        "void run() { dupfn(); }\n",
    ),
];

/// `@` makes the class name unique per file for the class-wrapped languages.
fn def_source(template: &str, name: &str, tag: &str) -> String {
    template.replace("{N}", name).replace('@', tag)
}

struct Fixture {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
    ext: String,
    template: String,
}

impl Fixture {
    fn path(&self, stem: &str) -> String {
        format!("{stem}.{}", self.ext)
    }

    fn write(&self, rel: &str, body: &str) {
        fs::write(self.repo.path().join(rel), body).unwrap();
    }

    /// Commit and index `files` (relative path, body), nothing else.
    fn index(files: &[(String, String)], ext: &str, template: &str) -> Self {
        let fx = Fixture {
            repo: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            ext: ext.to_string(),
            template: template.to_string(),
        };
        for (rel, body) in files {
            fx.write(rel, body);
        }
        run_git(fx.repo.path(), &["init", "-q", "-b", "main"]);
        commit_all(fx.repo.path(), "init");
        let out = fx.ecp(&["admin", "index", "--repo", "."], false);
        assert!(
            out.status.success(),
            "admin index failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        fx
    }

    /// Two files `c`/`d` each defining `dupfn`, plus caller `e`.
    fn two_defs(ext: &str, template: &str, caller: &str) -> Self {
        Self::index(
            &[
                (format!("c.{ext}"), def_source(template, NAME, "C")),
                (format!("d.{ext}"), def_source(template, NAME, "D")),
                (format!("e.{ext}"), caller.to_string()),
            ],
            ext,
            template,
        )
    }

    fn ecp(&self, args: &[&str], with_session: bool) -> std::process::Output {
        let mut cmd = Command::new(ecp_bin());
        cmd.args(args)
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env_remove("ECP_HOME")
            .env_remove("ECP_SESSION_ID")
            .env("ECP_SKIP_BG_REBUILD", "1");
        if with_session {
            cmd.env("ECP_SESSION_ID", SESSION);
        }
        cmd.output().expect("ecp failed to spawn")
    }

    fn impact_ok(&self, extra: &[&str], with_session: bool) -> serde_json::Value {
        let mut args = vec!["impact"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--repo", ".", "--format", "json"]);
        let out = self.ecp(&args, with_session);
        assert!(
            out.status.success(),
            "impact {extra:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "non-JSON impact output ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    /// Uncommitted rename of `dupfn` in `d`.
    fn rename_in_d(&self) {
        let renamed = def_source(&self.template, "renamedfn", "D");
        self.write(&self.path("d"), &renamed);
    }
}

fn has_ambiguity_caveat(json: &serde_json::Value) -> bool {
    json.get("result")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.contains("caller set may be incomplete") && s.contains("same-named"))
}

#[test]
fn test_impact_caveat_after_uncommitted_rename_fires_for_all_14_languages() {
    for (lang, ext, template, caller) in LANGS {
        let fx = Fixture::two_defs(ext, template, caller);
        fx.rename_in_d();
        let json = fx.impact_ok(&["--target", NAME, "--direction", "up"], true);
        assert!(
            has_ambiguity_caveat(&json),
            "{lang}: the survivor of an uncommitted rename must keep the caveat: {json}"
        );
    }
}

#[test]
fn test_impact_caveat_without_edit_two_defs_fires() {
    let (_, ext, template, caller) = LANGS[0];
    let fx = Fixture::two_defs(ext, template, caller);
    // No session: the overlay-absent path.
    let json = fx.impact_ok(
        &[
            "--target",
            NAME,
            "--file",
            &fx.path("c"),
            "--direction",
            "up",
        ],
        false,
    );
    assert!(has_ambiguity_caveat(&json), "{json}");
}

#[test]
fn test_impact_caveat_edit_adds_second_def_fires() {
    let (_, ext, template, caller) = LANGS[0];
    let fx = Fixture::index(
        &[
            (format!("c.{ext}"), def_source(template, NAME, "C")),
            (
                format!("d.{ext}"),
                "export function other() {}\n".to_string(),
            ),
            (format!("e.{ext}"), caller.to_string()),
        ],
        ext,
        template,
    );
    fx.write(&fx.path("d"), &def_source(template, NAME, "D"));
    let json = fx.impact_ok(
        &["--target", NAME, "--kind", "function", "--direction", "up"],
        true,
    );
    assert!(has_ambiguity_caveat(&json), "{json}");
}

#[test]
fn test_impact_caveat_single_def_without_edit_absent() {
    let (_, ext, template, caller) = LANGS[0];
    let fx = Fixture::index(
        &[
            (format!("c.{ext}"), def_source(template, NAME, "C")),
            (format!("e.{ext}"), caller.to_string()),
        ],
        ext,
        template,
    );
    let json = fx.impact_ok(&["--target", NAME, "--direction", "up"], false);
    assert!(!has_ambiguity_caveat(&json), "{json}");
}

#[test]
fn test_impact_caveat_single_def_with_unrelated_edit_absent() {
    let (_, ext, template, caller) = LANGS[0];
    let fx = Fixture::index(
        &[
            (format!("c.{ext}"), def_source(template, NAME, "C")),
            (
                format!("d.{ext}"),
                "export function other() {}\n".to_string(),
            ),
            (format!("e.{ext}"), caller.to_string()),
        ],
        ext,
        template,
    );
    fx.write(&fx.path("d"), "export function other_two() {}\n");
    let json = fx.impact_ok(&["--target", NAME, "--direction", "up"], true);
    assert!(!has_ambiguity_caveat(&json), "{json}");
}

#[test]
fn test_impact_caveat_downstream_after_rename_absent() {
    let (_, ext, template, caller) = LANGS[0];
    let fx = Fixture::two_defs(ext, template, caller);
    fx.rename_in_d();
    let json = fx.impact_ok(&["--target", NAME, "--direction", "down"], true);
    assert!(!has_ambiguity_caveat(&json), "{json}");
}

/// The caveat is about the resolver's view of the bare name, so `--file`
/// narrowing to one definition must not hide it.
#[test]
fn test_impact_caveat_file_filter_after_rename_still_fires() {
    let (_, ext, template, caller) = LANGS[0];
    let fx = Fixture::two_defs(ext, template, caller);
    fx.rename_in_d();
    let json = fx.impact_ok(
        &[
            "--target",
            NAME,
            "--file",
            &fx.path("c"),
            "--direction",
            "up",
        ],
        true,
    );
    assert!(has_ambiguity_caveat(&json), "{json}");
}

/// A class and a function share the name: the count is taken before `--kind`
/// narrowing, so narrowing to the class keeps the caveat after the function
/// is renamed away.
#[test]
fn test_impact_caveat_class_and_function_same_name_kind_filter_fires() {
    let fx = Fixture::index(
        &[
            ("c.ts".to_string(), format!("export class {NAME} {{}}\n")),
            (
                "d.ts".to_string(),
                format!("export function {NAME}() {{}}\n"),
            ),
            (
                "e.ts".to_string(),
                format!("function run() {{ {NAME}(); }}\n"),
            ),
        ],
        "ts",
        "",
    );
    let before = fx.impact_ok(
        &["--target", NAME, "--kind", "class", "--direction", "up"],
        false,
    );
    assert!(has_ambiguity_caveat(&before), "{before}");
    fx.write("d.ts", "export function renamedfn() {}\n");
    let after = fx.impact_ok(
        &["--target", NAME, "--kind", "class", "--direction", "up"],
        true,
    );
    assert!(has_ambiguity_caveat(&after), "{after}");
}

#[test]
fn test_impact_caveat_same_name_twice_in_one_file_absent_and_caller_listed() {
    // Two same-named defs in one file share a uid, so the index keeps one
    // node (reported as a uid-collision blind spot) and resolves the bare
    // call to it: no suppressed callers, so no ambiguity caveat.
    let fx = Fixture::index(
        &[
            (
                "c.ts".to_string(),
                format!("export function {NAME}() {{}}\nexport function {NAME}() {{}}\n"),
            ),
            (
                "e.ts".to_string(),
                format!("function run() {{ {NAME}(); }}\n"),
            ),
        ],
        "ts",
        "",
    );
    let json = fx.impact_ok(
        &["--target", NAME, "--kind", "function", "--direction", "up"],
        false,
    );
    assert!(!has_ambiguity_caveat(&json), "{json}");
    let callers: Vec<&str> = json["impact"]
        .as_array()
        .expect("impact array")
        .iter()
        .filter(|n| n["depth"] == 1)
        .filter_map(|n| n["filePath"].as_str())
        .collect();
    assert_eq!(callers, ["e.ts"], "{json}");
}

#[test]
fn test_impact_empty_target_errors_without_panic() {
    let (_, ext, template, caller) = LANGS[0];
    let fx = Fixture::two_defs(ext, template, caller);
    let out = fx.ecp(
        &["impact", "--target", "", "--repo", ".", "--format", "json"],
        false,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
}
