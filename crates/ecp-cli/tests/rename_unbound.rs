//! `ecp rename` must not write files and exit 0 while it leaves occurrences of
//! the name that the graph does not bind to the target.
//!
//! An occurrence inside a function that is not a referrer of the target is
//! unbound. Without `--dry-run` and without `--leave-unbound`, one unbound
//! occurrence refuses the whole rename before any write, code or markdown.
//! `--leave-unbound` waives only that refusal. The payload is toon on stdout,
//! so the tests assert on its field lines (`operation: refused`,
//! `unbound[N]{file,line}:` rows), not on prose.
//!
//! Fixture facts, measured on a built `ecp` before the tests were written:
//! the graph links `callback = run`, `run = make(); run()` and `const cb = run`
//! to the module-level `run` in Python and JavaScript, so those shapes are
//! bound. The unbound shapes below are the ones the graph leaves: an attribute
//! read (`obj.run`), a Ruby bare call, a Ruby local with `.call`, a plain
//! Python local, and a same-file JavaScript member call.

mod common;

use common::{commit_all, ecp_bin, init_and_analyze, run_git, write};
use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// An indexed git repo with its own cache root.
struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn indexed(files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        let repo = Self {
            _tmp: tmp,
            root,
            home,
        };
        for (rel, text) in files {
            let path = repo.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        run_git(&repo.root, &["init", "-q", "-b", "main"]);
        commit_all(&repo.root, "base");
        let out = repo.ecp(&["admin", "index", "--repo", "."]);
        assert!(out.status.success(), "admin index: {}", stderr(&out));
        repo
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn ecp(&self, args: &[&str]) -> Output {
        Command::new(ecp_bin())
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", "rename-unbound")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// True when the toon payload lists `file:line` as a row of its `unbound`
/// table. A row is the whole trimmed line `file,line`.
fn lists_unbound(out: &Output, file: &str, line: u32) -> bool {
    let row = format!("{file},{line}");
    let text = stdout(out);
    let mut in_table = false;
    for l in text.lines() {
        if l.starts_with("unbound[") {
            in_table = true;
        } else if in_table {
            if !l.starts_with(' ') {
                break;
            }
            if l.trim() == row {
                return true;
            }
        }
    }
    false
}

/// The refusal payload: operation, reason and the old and new names.
fn assert_unbound_refusal(out: &Output) {
    let text = stdout(out);
    assert!(!out.status.success(), "must exit non-zero: {text}");
    assert!(text.contains("operation: refused"), "{text}");
    assert!(text.contains("reason: unbound_occurrences"), "{text}");
    let err = stderr(out);
    assert!(
        err.contains("--dry-run") && err.contains("--leave-unbound"),
        "the error must name both flags: {err}"
    );
}

const JS_K: &str =
    "class K {\n  save() {}\n}\n\nfunction run(k) {\n  k.save();\n}\n\nrun(new K());\n";
const JS_K_LEFT: &str =
    "class K {\n  persist() {}\n}\n\nfunction run(k) {\n  k.save();\n}\n\nrun(new K());\n";

/// Stands for: `unbound_occurrences` refusal. The graph has no edge for the
/// same-file `k.save()` (overlay_qualified_call_parity.rs), so a rename that
/// writes only `K.save` breaks the call. It must refuse, change no byte, exit
/// non-zero, and list the `k.save` line. Failed before: it renamed the
/// declaration, left the call and exited 0.
#[test]
fn test_rename_js_missing_member_edge_refuses() {
    let repo = Repo::indexed(&[("lib.js", JS_K)]);

    let out = repo.ecp(&["rename", "K.save", "persist"]);

    assert_unbound_refusal(&out);
    assert!(lists_unbound(&out, "lib.js", 6), "{}", stdout(&out));
    assert_eq!(repo.read("lib.js"), JS_K, "no file may change");
}

const PY_ATTR: &str = "def run():\n    return 1\n\n\ndef main():\n    return run()\n\n\ndef other(obj):\n    callback = obj.run\n    return callback\n";

/// Stands for: a reference with no parentheses is no proof of another
/// binding. `obj.run` in `other` is not linked to the module-level `run`, so
/// the rename refuses and lists line 10. The graph does link a plain
/// `callback = run`, so the fixture reads the name off an attribute.
#[test]
fn test_rename_unbound_bare_reference_refuses() {
    let repo = Repo::indexed(&[("lib.py", PY_ATTR)]);

    let out = repo.ecp(&["rename", "run", "launch"]);

    assert_unbound_refusal(&out);
    assert!(lists_unbound(&out, "lib.py", 10), "{}", stdout(&out));
    assert_eq!(repo.read("lib.py"), PY_ATTR);
}

const RB_BARE: &str = "def run\n  1\nend\n\ndef main\n  run()\nend\n\ndef other\n  run\nend\n";

/// Stands for: a Ruby bare call `run` (no parentheses) inside a non-referrer
/// method is not linked by the graph (measured: line 10 is unbound), so the
/// rename refuses instead of leaving it behind.
#[test]
fn test_rename_ruby_unbound_bare_call_refuses() {
    let repo = Repo::indexed(&[("lib.rb", RB_BARE)]);

    let out = repo.ecp(&["rename", "run", "launch"]);

    assert_unbound_refusal(&out);
    assert!(lists_unbound(&out, "lib.rb", 10), "{}", stdout(&out));
    assert_eq!(repo.read("lib.rb"), RB_BARE);
}

const RB_LOCAL: &str = "def run\n  1\nend\n\ndef main\n  run()\nend\n\ndef other(make)\n  run = make.call\n  run.call\nend\n";

/// Stands for: a local named like the target, assigned and then used inside a
/// non-referrer function, is conservatively unbound. Python and JavaScript
/// bind `run = make(); run()` to the module function (measured), so the
/// fixture uses Ruby, where the graph leaves lines 10 and 11.
#[test]
fn test_rename_unbound_local_call_refuses() {
    let repo = Repo::indexed(&[("lib.rb", RB_LOCAL)]);

    let out = repo.ecp(&["rename", "run", "launch"]);

    assert_unbound_refusal(&out);
    assert!(lists_unbound(&out, "lib.rb", 10), "{}", stdout(&out));
    assert!(lists_unbound(&out, "lib.rb", 11), "{}", stdout(&out));
    assert_eq!(repo.read("lib.rb"), RB_LOCAL);
}

const RUN_PY: &str = "def run():\n    return 1\n\n\ndef main():\n    return run()\n\n\ndef other():\n    run = 5\n    return run\n";

/// Stands for: the default is to refuse. The local `run = 5` in `other` is
/// another binding, and without the flag the rename writes neither the
/// declaration nor the call.
#[test]
fn test_rename_unbound_default_refuses_without_changes() {
    let repo = Repo::indexed(&[("lib.py", RUN_PY)]);

    let out = repo.ecp(&["rename", "run", "launch"]);

    assert_unbound_refusal(&out);
    assert!(lists_unbound(&out, "lib.py", 10), "{}", stdout(&out));
    assert!(lists_unbound(&out, "lib.py", 11), "{}", stdout(&out));
    assert_eq!(repo.read("lib.py"), RUN_PY, "declaration and local stay");
}

/// Stands for: `--leave-unbound` keeps the unbound site byte-identical, still
/// lists it, writes the bound declaration, and exits 0.
#[test]
fn test_rename_leave_unbound_member_call_preserves_site() {
    let repo = Repo::indexed(&[("lib.js", JS_K)]);

    let out = repo.ecp(&["rename", "K.save", "persist", "--leave-unbound"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(repo.read("lib.js"), JS_K_LEFT);
    assert!(lists_unbound(&out, "lib.js", 6), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("operation: applied"),
        "{}",
        stdout(&out)
    );
}

/// Stands for: the check classifies every candidate file before any write.
/// `a.py` and `b.py` hold bound occurrences only, `c.py` (last in the sorted
/// file set) holds the unbound ones. With `--markdown` and a doc that names
/// the symbol, no code file and no markdown file changes. Failed before: the
/// earlier files were written first.
#[test]
fn test_rename_unbound_late_file_refuses_all_writes() {
    let a = "def run():\n    return 1\n";
    let b = "from a import run\n\n\ndef use():\n    return run()\n";
    let c = "from a import run\n\n\ndef use2():\n    return run()\n\n\ndef other():\n    run = 5\n    return run\n";
    let doc = "Call `run` here.\n";
    let repo = Repo::indexed(&[("a.py", a), ("b.py", b), ("c.py", c), ("docs.md", doc)]);

    let out = repo.ecp(&["rename", "run", "launch", "--markdown"]);

    assert_unbound_refusal(&out);
    assert!(lists_unbound(&out, "c.py", 9), "{}", stdout(&out));
    assert!(lists_unbound(&out, "c.py", 10), "{}", stdout(&out));
    assert_eq!(repo.read("a.py"), a);
    assert_eq!(repo.read("b.py"), b);
    assert_eq!(repo.read("c.py"), c);
    assert_eq!(repo.read("docs.md"), doc);
}

/// Stands for: dry-run lists the unbound sites, writes nothing, exits 0, and
/// its headline is `risk partial` with the unbound count, with and without
/// `--leave-unbound`. The flag does not change the headline.
#[test]
fn test_rename_unbound_dry_run_reports_partial() {
    let repo = Repo::indexed(&[("lib.py", RUN_PY)]);

    for extra in [&[][..], &["--leave-unbound"][..]] {
        let mut args = vec!["rename", "run", "launch", "--dry-run"];
        args.extend_from_slice(extra);
        let out = repo.ecp(&args);

        assert!(out.status.success(), "{extra:?}: {}", stderr(&out));
        let text = stdout(&out);
        assert!(
            text.lines()
                .any(|l| l.starts_with("risk partial;") && l.ends_with("; unbound 2")),
            "{extra:?}: {text}"
        );
        assert!(!text.contains("risk safe"), "{extra:?}: {text}");
        assert!(text.contains("operation: dry-run"), "{extra:?}: {text}");
        assert!(lists_unbound(&out, "lib.py", 10), "{extra:?}: {text}");
        assert_eq!(repo.read("lib.py"), RUN_PY, "{extra:?}: no write");
    }
}

/// Stands for: a rename with no unbound site applies, exits 0, and carries
/// neither a refusal nor an `unbound` list. The new check must not touch the
/// normal path.
#[test]
fn test_rename_fully_bound_applies() {
    let repo = Repo::indexed(&[(
        "lib.py",
        "def run():\n    return 1\n\n\ndef main():\n    return run()\n",
    )]);

    let out = repo.ecp(&["rename", "run", "launch"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        repo.read("lib.py"),
        "def launch():\n    return 1\n\n\ndef main():\n    return launch()\n"
    );
    let text = stdout(&out);
    assert!(
        !text.contains("refused") && !text.contains("unbound"),
        "{text}"
    );
}

/// Stands for: `--leave-unbound` waives only the unbound refusal. Two
/// same-name declarations are still an ambiguity: the command exits non-zero,
/// names the ambiguity, and writes nothing. Warm-attach is not covered here
/// (`target_resolver.rs` covers it without the flag; a fixture for both is
/// not built).
#[test]
fn test_rename_leave_unbound_existing_refusal_still_refuses() {
    let a = "def parse():\n    return 1\n";
    let b = "def parse():\n    return 2\n";
    let repo = Repo::indexed(&[("a.py", a), ("b.py", b)]);

    let out = repo.ecp(&["rename", "parse", "parse_config", "--leave-unbound"]);

    assert!(!out.status.success(), "an ambiguity must exit non-zero");
    assert!(stderr(&out).contains("ambiguous"), "{}", stderr(&out));
    assert_eq!(repo.read("a.py"), a);
    assert_eq!(repo.read("b.py"), b);
}

const NESTED_SHADOW: &str = "def helper():\n    return 1\n\n\ndef outer():\n    def inner(helper):\n        return helper\n    return helper() + inner(2)\n";

/// Stands for: the innermost enclosing callable decides a binding (commit
/// 02887213). `inner` takes a parameter named `helper`, so lines 6-7 are
/// another binding even though the parent `outer` calls the target. With
/// `--leave-unbound` the call in `outer` is renamed, `inner` stays, and
/// lines 6 and 7 are listed.
#[test]
fn test_binds_nested_shadow_uses_innermost_callable() {
    let repo = Repo::indexed(&[("lib.py", NESTED_SHADOW)]);

    let out = repo.ecp(&["rename", "helper", "aid", "--leave-unbound"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        repo.read("lib.py"),
        "def aid():\n    return 1\n\n\ndef outer():\n    def inner(helper):\n        return helper\n    return aid() + inner(2)\n"
    );
    assert!(lists_unbound(&out, "lib.py", 6), "{}", stdout(&out));
    assert!(lists_unbound(&out, "lib.py", 7), "{}", stdout(&out));
}

/// Stands for: a nested referrer is bound. `inner` calls the target and
/// `outer` does not, so the call inside `inner` is renamed with no flag, no
/// unbound site, and exit 0.
#[test]
fn test_binds_nested_referrer_remains_bound() {
    let repo = Repo::indexed(&[(
        "lib.py",
        "def helper():\n    return 1\n\n\ndef outer():\n    def inner():\n        return helper()\n    return inner()\n",
    )]);

    let out = repo.ecp(&["rename", "helper", "aid"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        repo.read("lib.py"),
        "def aid():\n    return 1\n\n\ndef outer():\n    def inner():\n        return aid()\n    return inner()\n"
    );
    assert!(!stdout(&out).contains("unbound"), "{}", stdout(&out));
}

// ---------------------------------------------------------------------------
// Injected-graph fixtures: a `.vue` file has no identifier finder and the Vue
// parser emits no calls, so only a fixture graph puts it in the file set.
// ---------------------------------------------------------------------------

const NO_FINDER: &str = "no identifier finder for this file type";
const APP_VUE: &str = "<template><div /></template>\n<script lang=\"ts\">\nimport { helper } from './util';\nexport function setup() {\n  return helper();\n}\n</script>\n";

fn find_graph_bin(repo: &Path) -> PathBuf {
    let mut queue = vec![repo.join(".ecp")];
    while let Some(dir) = queue.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
            if path.file_name().is_some_and(|n| n == "graph.bin") {
                return path;
            }
            if path.is_dir() {
                queue.push(path);
            }
        }
    }
    panic!("graph.bin not found under {}", repo.join(".ecp").display())
}

fn rename_in(repo: &Path, extra: &[&str]) -> Output {
    Command::new(ecp_bin())
        .args([
            "rename",
            "helper",
            "assist",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .args(extra)
        .env("HOME", repo)
        .current_dir(repo)
        .output()
        .expect("ecp rename spawn failed")
}

/// `src/util.ts` holds `helper` (lines 1-3, bound) and `other` (lines 4-7)
/// whose body spells `helper` as a local; `src/App.vue::setup` calls `helper`.
fn both_repo() -> tempfile::TempDir {
    const UTIL: &str = "export function helper() {\n  return 1;\n}\nexport function other() {\n  const helper = 2;\n  return helper;\n}\n";
    let repo = tempfile::tempdir().expect("tempdir");
    write(repo.path(), "src/util.ts", UTIL);
    write(repo.path(), "src/App.vue", APP_VUE);
    init_and_analyze(repo.path());
    let mut fx = GraphFixture::new();
    let helper = fx.func("src/util.ts", "helper");
    fx.span(helper, (0, 0, 2, 1));
    let setup = fx.func("src/App.vue", "setup");
    fx.span(setup, (3, 0, 5, 1));
    fx.edge(setup, helper, RelType::Calls);
    let other = fx.func("src/util.ts", "other");
    fx.span(other, (3, 0, 6, 1));
    fs::write(find_graph_bin(repo.path()), fx.into_bytes()).unwrap();
    repo
}

/// Stands for: the dry-run headline counts both kinds of gap, in the order
/// `not scanned K` then `unbound U`, and the payload carries both the
/// `not_scanned` and the `unbound` field.
#[test]
fn test_rename_unbound_and_not_scanned_reports_both() {
    let repo = both_repo();

    let out = rename_in(repo.path(), &["--dry-run"]);

    let text = stdout(&out);
    assert!(out.status.success(), "{text}\n{}", stderr(&out));
    assert!(
        text.contains("risk partial; files 1; usages 1; not scanned 1; unbound 2"),
        "{text}"
    );
    assert!(text.contains("not_scanned"), "{text}");
    assert!(
        text.lines()
            .any(|l| l.contains("src/App.vue") && l.contains(NO_FINDER)),
        "{text}"
    );
    assert!(lists_unbound(&out, "src/util.ts", 5), "{text}");
    assert!(lists_unbound(&out, "src/util.ts", 6), "{text}");
}

/// Stands for: the refusal runs before the zero-hit early return. The target
/// sits in a file without an identifier finder and its only referrer span
/// holds no spelling of the name, so the bound hits are zero while `other`
/// holds unbound ones (reachable when a parser span and the text disagree).
/// The command must refuse, not print "No occurrences" and exit 0. With
/// `--leave-unbound` it exits 0 and still lists the sites.
#[test]
fn test_rename_unbound_zero_hits_refuses() {
    const UTIL: &str = "export function caller() {\n  return 1;\n}\nexport function other() {\n  const helper = 2;\n  return helper;\n}\n";
    let repo = tempfile::tempdir().expect("tempdir");
    write(repo.path(), "src/util.ts", UTIL);
    write(repo.path(), "src/App.vue", APP_VUE);
    init_and_analyze(repo.path());
    let mut fx = GraphFixture::new();
    let helper = fx.func("src/App.vue", "helper");
    fx.span(helper, (3, 0, 5, 1));
    let caller = fx.func("src/util.ts", "caller");
    fx.span(caller, (0, 0, 2, 1));
    fx.edge(caller, helper, RelType::Calls);
    let other = fx.func("src/util.ts", "other");
    fx.span(other, (3, 0, 6, 1));
    fs::write(find_graph_bin(repo.path()), fx.into_bytes()).unwrap();

    let out = rename_in(repo.path(), &[]);

    assert_unbound_refusal(&out);
    assert!(lists_unbound(&out, "src/util.ts", 5), "{}", stdout(&out));
    assert!(stdout(&out).contains("not_scanned"), "{}", stdout(&out));
    let util = fs::read_to_string(repo.path().join("src/util.ts")).unwrap();
    assert_eq!(util, UTIL);

    let kept = rename_in(repo.path(), &["--leave-unbound"]);

    assert!(kept.status.success(), "{}", stderr(&kept));
    assert!(lists_unbound(&kept, "src/util.ts", 5), "{}", stdout(&kept));
}
