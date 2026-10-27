//! A bare type name selects the type over its own same-named constructors
//! (C1 "Typed target result", Bare name exception). The four languages whose
//! constructor carries the type name (C#, Java, C++, Dart) give a `Class` and
//! a `Constructor` both named `Foo`; before the exception every command
//! answered `ambiguous`.

mod common;

use common::{commit_all, ecp_bin, run_git};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

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
            repo.write(rel, text);
        }
        run_git(&repo.root, &["init", "-q", "-b", "main"]);
        commit_all(&repo.root, "base");
        let out = repo.ecp(&["admin", "index", "--repo", "."]);
        assert!(out.status.success(), "admin index: {}", text(&out.stderr));
        repo
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn ecp(&self, args: &[&str]) -> Output {
        Command::new(ecp_bin())
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", "constructor-bare-name")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
    }

    fn impact_foo(&self) -> Value {
        let out = self.ecp(&[
            "impact",
            "Foo",
            "--direction",
            "upstream",
            "--repo",
            ".",
            "--format",
            "json",
        ]);
        json_of(&out)
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn json_of(out: &Output) -> Value {
    let stdout = text(&out.stdout);
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON on stdout: {stdout}\nstderr: {}", text(&out.stderr)));
    serde_json::Deserializer::from_str(&stdout[start..])
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap()
}

/// The rows of `payload["impact"]` as `(depth, kind, name)`.
fn rows(payload: &Value) -> Vec<(u64, String, String)> {
    payload["impact"]
        .as_array()
        .unwrap_or_else(|| panic!("no impact rows: {payload}"))
        .iter()
        .map(|r| {
            (
                r["depth"].as_u64().unwrap(),
                r["kind"].as_str().unwrap().to_string(),
                r["name"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Stands for: the bare type name reaches the type and its construction
/// callers, not an `ambiguous` answer. Failed before: `impact Foo` listed the
/// `Class` and the `Constructor` as two candidates.
fn assert_bare_foo_selects_type_with_caller(files: &[(&str, &str)], caller: &str) {
    let repo = Repo::indexed(files);

    let payload = repo.impact_foo();

    assert_eq!(payload["status"], "success", "{payload}");
    let rows = rows(&payload);
    assert_eq!(rows[0], (0, "Class".into(), "Foo".into()), "{payload}");
    assert!(
        rows.iter().any(|r| r.0 == 1 && r.2 == caller),
        "the construction caller {caller} is at depth 1: {payload}"
    );
}

#[test]
fn test_impact_bare_type_csharp_lists_new_caller() {
    assert_bare_foo_selects_type_with_caller(
        &[
            (
                "Foo.cs",
                "class Foo\n{\n    public Foo()\n    {\n    }\n}\n",
            ),
            (
                "Maker.cs",
                "class Maker\n{\n    void Make()\n    {\n        var f = new Foo();\n    }\n}\n",
            ),
        ],
        "Make",
    );
}

#[test]
fn test_impact_bare_type_java_lists_new_caller() {
    assert_bare_foo_selects_type_with_caller(
        &[
            (
                "Foo.java",
                "public class Foo {\n    public Foo() {\n    }\n}\n",
            ),
            (
                "Maker.java",
                "public class Maker {\n    public void make() {\n        Foo f = new Foo();\n    }\n}\n",
            ),
        ],
        "make",
    );
}

#[test]
fn test_impact_bare_type_cpp_lists_construction_caller() {
    assert_bare_foo_selects_type_with_caller(
        &[
            ("foo.cpp", "class Foo {\npublic:\n    Foo() {}\n};\n"),
            (
                "maker.cpp",
                "void make() {\n    Foo f = Foo();\n    Foo* g = new Foo();\n}\n",
            ),
        ],
        "make",
    );
}

#[test]
fn test_impact_bare_type_dart_lists_construction_caller() {
    assert_bare_foo_selects_type_with_caller(
        &[
            ("foo.dart", "class Foo {\n  Foo();\n}\n"),
            ("maker.dart", "void make() {\n  var f = Foo();\n}\n"),
        ],
        "make",
    );
}

/// Stands for: a dirty type file answers like a clean one. The overlay's
/// virtual `Class` and `Constructor` go through the same `select`.
#[test]
fn test_impact_bare_type_csharp_dirty_type_file_selects_type() {
    let src = "class Foo\n{\n    public Foo()\n    {\n    }\n}\n";
    let repo = Repo::indexed(&[
        ("Foo.cs", src),
        (
            "Maker.cs",
            "class Maker\n{\n    void Make()\n    {\n        var f = new Foo();\n    }\n}\n",
        ),
    ]);
    repo.write("Foo.cs", &format!("{src}// dirty edit\n"));

    let payload = repo.impact_foo();

    assert_eq!(payload["status"], "success", "{payload}");
    assert_eq!(rows(&payload)[0], (0, "Class".into(), "Foo".into()));
}

/// Stands for: `Foo.Foo` and `--kind constructor` still reach the
/// constructor overloads and answer their ambiguity.
#[test]
fn test_impact_foo_dot_foo_two_overloads_stays_ambiguous() {
    let repo = Repo::indexed(&[(
        "Foo.cs",
        "class Foo\n{\n    public Foo()\n    {\n    }\n    public Foo(int x)\n    {\n    }\n}\n",
    )]);

    let out = repo.ecp(&["impact", "Foo.Foo", "--repo", ".", "--format", "json"]);

    let payload = json_of(&out);
    assert_eq!(payload["status"], "ambiguous", "{payload}");
}

/// Stands for: a C++ constructor defined outside its class is not owned by
/// another same-named class of its file. Failed at 9aeb1dda: `impl.cpp`
/// holds `A::Foo` and the definition `B::Foo::Foo()`, the constructor
/// dropped, and the bare name selected `A::Foo`; failed at 15f1bbe1 with
/// both on one line (codex counterexamples).
#[test]
fn test_impact_bare_type_cpp_out_of_class_constructor_stays_ambiguous() {
    for impl_cpp in [
        "#include \"b.hpp\"\nnamespace A {\nclass Foo {};\n}\nnamespace B {\nFoo::Foo() {}\n}\n",
        "#include \"b.hpp\"\nnamespace A { class Foo {}; } namespace B { Foo::Foo() {} }\n",
    ] {
        let repo = Repo::indexed(&[
            ("b.hpp", "namespace B {\nclass Foo { public: Foo(); };\n}\n"),
            ("impl.cpp", impl_cpp),
        ]);

        let out = repo.ecp(&[
            "impact", "Foo", "--file", "impl.cpp", "--repo", ".", "--format", "json",
        ]);

        let payload = json_of(&out);
        assert_eq!(payload["status"], "ambiguous", "{impl_cpp}: {payload}");
    }
}

/// Stands for: a C# attribute class and a Kotlin `annotation class` are
/// types, so a bare name selects them over their own constructors. Failed at
/// 73949060: the drop read `is_type`, which leaves out `Annotation`, and
/// both answered `ambiguous`.
#[test]
fn test_impact_bare_annotation_type_selects_type() {
    for (file, src, name) in [
        (
            "a.cs",
            "public class FooAttribute : System.Attribute\n{\n    public FooAttribute() {}\n}\n",
            "FooAttribute",
        ),
        ("Tag.kt", "annotation class Tag(val v: String)\n", "Tag"),
    ] {
        let repo = Repo::indexed(&[(file, src)]);

        let out = repo.ecp(&["impact", name, "--repo", ".", "--format", "json"]);

        let payload = json_of(&out);
        assert_eq!(payload["status"], "success", "{file}: {payload}");
        assert_eq!(
            rows(&payload)[0],
            (0, "Annotation".into(), name.into()),
            "{file}: {payload}"
        );
    }
}

/// Stands for: `impact` from the attribute-class part of a C# `partial`
/// type reaches the other part and the construction caller. Failed at
/// 39dba9fb: the part filter read `is_type`, so the `Annotation` part
/// expanded to nothing.
#[test]
fn test_impact_csharp_partial_attribute_part_lists_other_part_and_caller() {
    let repo = Repo::indexed(&[
        ("x.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />\n"),
        (
            "a.cs",
            "public partial class FooAttribute : System.Attribute\n{\n}\n",
        ),
        (
            "b.cs",
            "public partial class FooAttribute\n{\n    public void Extra() {}\n}\n",
        ),
        (
            "c.cs",
            "class User\n{\n    public object Use()\n    {\n        return new FooAttribute();\n    }\n}\n",
        ),
    ]);

    let out = repo.ecp(&[
        "impact",
        "FooAttribute",
        "--file",
        "a.cs",
        "--repo",
        ".",
        "--format",
        "json",
    ]);

    let payload = json_of(&out);
    let rows = rows(&payload);
    assert!(
        rows.iter().any(|r| r.0 == 0 && r.1 == "Class"),
        "the b.cs part is at depth 0: {payload}"
    );
    assert!(
        rows.iter().any(|r| r.0 == 1 && r.2 == "Use"),
        "the construction caller is at depth 1: {payload}"
    );
}
