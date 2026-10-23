//! A member call on a call (`Foo.create().bar()`, `make().bar()`) binds the
//! member of the type the inner callee declares it returns
//! (FU-2026-10-22-67874c602c84). The parser records the outer call as a
//! `CallSite::ChainedMember`; the resolver binds the inner call, reads the
//! one target's return type in that target's own file, and runs the
//! receiver-typing ladder on it.
//!
//! Every fixture declares `bar` on a second class too: with a unique `bar`
//! the call binds by name alone, which hides the gap.
//!
//! Where the return type is absent or is no bare type name (a generic such
//! as `Promise<Foo>`, `Optional[Foo]`, `List<Foo>`), the site resolves as
//! the entry the parser wrote before chains: a bare, untyped, or
//! receiver-rooted member. The fixtures leave that fallback unbound.
//! Unwrapping generics is the follow-up.
//!
//! C declares no methods: a nested call `foo_bar(foo_create())` is two
//! direct calls, which keep binding.
mod instantiation_calls_support;

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::path_aliases::PathAliases;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{CallSite, CHAIN_DEPTH_LIMIT};
use ecp_core::graph::ZeroCopyGraph;
use instantiation_calls_support::calls_from;
use std::path::Path;

/// `(file, owner, name)` of one `Calls` target.
type Target = (String, String, String);

fn graph(files: &[(&dyn LanguageProvider, &str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for (provider, path, src) in files {
        let local = provider
            .parse_file(Path::new(path), src.as_bytes())
            .unwrap_or_else(|e| panic!("parse {path}: {e}"));
        builder.add_graph(local);
    }
    builder.build()
}

fn single(provider: &dyn LanguageProvider, files: &[(&str, &str)]) -> ZeroCopyGraph {
    let with_provider: Vec<_> = files
        .iter()
        .map(|&(path, src)| (provider, path, src))
        .collect();
    graph(&with_provider)
}

/// Build a Go module fixture with the same module alias that the CLI derives
/// from its preserved `go.mod`.
fn go_module(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().expect("tempdir");
    let provider = GoProvider::new().expect("GoProvider init");
    let mut aliases = PathAliases::new();
    aliases.add("example.com/app/*", vec!["./*".to_string()]);
    let mut builder = GraphBuilder::new()
        .with_repo_root(tmp.path().to_path_buf())
        .with_path_aliases(aliases);
    for &(path, source) in files {
        let disk = tmp.path().join(path);
        std::fs::create_dir_all(disk.parent().expect("parent")).expect("create parent");
        std::fs::write(&disk, source).expect("write fixture");
        if path.ends_with(".go") {
            builder.add_graph(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .expect("parse Go fixture"),
            );
        }
    }
    builder.build()
}

/// The `Calls` targets of every node named `caller`, sorted.
fn targets(graph: &ZeroCopyGraph, caller: &str) -> Vec<Target> {
    let mut out: Vec<Target> = calls_from(graph, caller)
        .into_iter()
        .map(|hit| (hit.file, hit.owner, hit.name))
        .collect();
    out.sort();
    out
}

/// Owners of the `Calls` targets named `member` from `caller`, sorted.
fn owners(graph: &ZeroCopyGraph, caller: &str, member: &str) -> Vec<String> {
    let mut out: Vec<String> = calls_from(graph, caller)
        .into_iter()
        .filter(|hit| hit.name == member)
        .map(|hit| hit.owner)
        .collect();
    out.sort();
    out
}

/// `caller`'s chained `member` call binds `Foo`'s and never `Other`'s.
fn assert_binds_foo(graph: &ZeroCopyGraph, caller: &str, member: &str) {
    assert_eq!(
        owners(graph, caller, member),
        ["Foo"],
        "{caller}: the receiver's return type picks `Foo.{member}`; all: {:?}",
        targets(graph, caller)
    );
}

/// Today's behaviour for an untyped chain: the fallback `member` has two
/// owners, so it binds neither.
fn assert_falls_back(graph: &ZeroCopyGraph, caller: &str, member: &str) {
    assert_eq!(
        owners(graph, caller, member),
        Vec::<String>::new(),
        "{caller}: no reducible return type, the ambiguous fallback binds nothing; all: {:?}",
        targets(graph, caller)
    );
}

/// A known return type cannot resolve `member`, so no chain level binds it.
fn assert_no_member_edge(graph: &ZeroCopyGraph, caller: &str, member: &str) {
    assert_eq!(
        owners(graph, caller, member),
        Vec::<String>::new(),
        "{caller}: known receiver typing must not bind `{member}` by fallback; all: {:?}",
        targets(graph, caller)
    );
}

/// `caller` still binds the inner call `inner` (owned by `owner`).
fn assert_inner_binds(graph: &ZeroCopyGraph, caller: &str, owner: &str, inner: &str) {
    assert_eq!(
        owners(graph, caller, inner),
        [owner],
        "{caller}: the inner call keeps its own edge; all: {:?}",
        targets(graph, caller)
    );
}

/// The raw `calls` entries of `caller` in one parsed file.
fn raw_calls(provider: &dyn LanguageProvider, path: &str, src: &str, caller: &str) -> Vec<String> {
    let local = provider
        .parse_file(Path::new(path), src.as_bytes())
        .unwrap();
    local
        .nodes
        .iter()
        .filter(|n| n.name == caller)
        .flat_map(|n| n.calls.clone())
        .collect()
}

// ---- TypeScript ----

const TS_FOO: (&str, &str) = (
    "src/foo.ts",
    r#"export class Foo {
  static create(): Foo {
    return new Foo();
  }
  again(): Foo {
    return this;
  }
  static plain() {
    return new Foo();
  }
  static wrapped(): Promise<Foo> {
    return Promise.resolve(new Foo());
  }
  bar(): void {}
}

export class Other {
  bar(): void {}
}

export function make(): Foo {
  return new Foo();
}

export function maybe(): Foo | null {
  return null;
}
"#,
);

const TS_USE: (&str, &str) = (
    "src/use.ts",
    r#"import { Foo, make, maybe } from "./foo";

export function useIt() {
  Foo.create().bar();
}
export function useMake() {
  make().bar();
}
export function useMaybe() {
  maybe()?.bar();
}
export function useNested() {
  Foo.create().again().bar();
}
export function usePlain() {
  Foo.plain().bar();
}
export function useWrapped() {
  Foo.wrapped().bar();
}
export function useTyped(f: Foo) {
  f.bar();
}
export function useNew() {
  new Foo().bar();
}
"#,
);

fn ts_graph() -> ZeroCopyGraph {
    single(&TypeScriptProvider::new().unwrap(), &[TS_FOO, TS_USE])
}

#[test]
fn test_ts_callee_name_call_receiver_emits_chained_member() {
    let calls = raw_calls(
        &TypeScriptProvider::new().unwrap(),
        TS_USE.0,
        TS_USE.1,
        "useIt",
    );
    let chained = calls.iter().find_map(|c| match CallSite::parse(c) {
        site @ CallSite::ChainedMember { inner, .. } => Some((site, inner)),
        _ => None,
    });
    let (site, inner) = chained.unwrap_or_else(|| panic!("no chained entry: {calls:?}"));
    assert!(
        CallSite::parse(inner).name().ends_with("create"),
        "{calls:?}"
    );
    assert_eq!(site.fallback(), CallSite::UntypedMember("bar"), "{calls:?}");
    assert_eq!(site.name(), "bar", "{calls:?}");
}

#[test]
fn test_resolve_call_typescript_static_factory_chain_binds_return_type_member() {
    let graph = ts_graph();
    assert_binds_foo(&graph, "useIt", "bar");
    assert_inner_binds(&graph, "useIt", "Foo", "create");
}

#[test]
fn test_resolve_call_typescript_free_function_chain_binds_return_type_member() {
    assert_binds_foo(&ts_graph(), "useMake", "bar");
}

/// `Foo | null` reduces to `Foo`; `?.` keeps the call a member call.
#[test]
fn test_resolve_call_typescript_nullable_return_chain_binds_return_type_member() {
    assert_binds_foo(&ts_graph(), "useMaybe", "bar");
}

/// `a().b().c()` types each level by the level below.
#[test]
fn test_resolve_call_typescript_nested_chain_binds_each_level() {
    let graph = ts_graph();
    assert_binds_foo(&graph, "useNested", "again");
    assert_binds_foo(&graph, "useNested", "bar");
}

/// `new Foo().bar()`: the inner call constructs `Foo`.
#[test]
fn test_resolve_call_typescript_new_receiver_chain_binds_class_member() {
    assert_binds_foo(&ts_graph(), "useNew", "bar");
}

/// No declared return type: today's bare `bar`.
#[test]
fn test_resolve_call_typescript_untyped_factory_chain_keeps_fallback() {
    assert_falls_back(&ts_graph(), "usePlain", "bar");
}

/// Follow-up: `Promise<Foo>` is not unwrapped yet.
#[test]
fn test_resolve_call_typescript_generic_return_chain_keeps_fallback() {
    assert_falls_back(&ts_graph(), "useWrapped", "bar");
}

#[test]
fn test_resolve_call_typescript_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&ts_graph(), "useTyped", "bar");
}

/// The chained edge takes the ladder's typed-receiver confidence, never
/// more than a typed receiver's own edge.
#[test]
fn test_resolve_call_typescript_chained_edge_confidence_not_above_typed_receiver() {
    let graph = ts_graph();
    let confidence = |caller: &str| {
        calls_from(&graph, caller)
            .into_iter()
            .find(|hit| hit.name == "bar")
            .unwrap_or_else(|| panic!("{caller} binds bar"))
            .confidence
    };
    assert!(
        confidence("useIt") <= confidence("useTyped"),
        "chained {} > typed {}",
        confidence("useIt"),
        confidence("useTyped")
    );
}

// ---- JavaScript ----

const JS_FOO: (&str, &str) = (
    "src/foo.js",
    r#"export class Foo {
  static create() {
    return new Foo();
  }
  bar() {}
  baz() {
    this.bar();
  }
}

export class Other {
  bar() {}
}

export function make() {
  return new Foo();
}
"#,
);

const JS_USE: (&str, &str) = (
    "src/use.js",
    r#"import { Foo, make } from "./foo";

export function useIt() {
  Foo.create().bar();
}
export function useMake() {
  make().bar();
}
export function useNew() {
  new Foo().bar();
}
"#,
);

fn js_graph() -> ZeroCopyGraph {
    single(&JavaScriptProvider::new().unwrap(), &[JS_FOO, JS_USE])
}

/// JavaScript declares no return types (JSDoc is not read): today's
/// behaviour, no `bar` edge.
#[test]
fn test_resolve_call_javascript_static_factory_chain_keeps_fallback() {
    let graph = js_graph();
    assert_falls_back(&graph, "useIt", "bar");
    assert_inner_binds(&graph, "useIt", "Foo", "create");
}

#[test]
fn test_resolve_call_javascript_free_function_chain_keeps_fallback() {
    assert_falls_back(&js_graph(), "useMake", "bar");
}

/// A construction needs no return type: `new Foo()` is a `Foo`.
#[test]
fn test_resolve_call_javascript_new_receiver_chain_binds_class_member() {
    assert_binds_foo(&js_graph(), "useNew", "bar");
}

#[test]
fn test_resolve_call_javascript_this_receiver_still_binds_own_member() {
    assert_binds_foo(&js_graph(), "baz", "bar");
}

// ---- Python ----

const PY_FOO: (&str, &str) = (
    "foo.py",
    r#"from typing import Optional


class Foo:
    @staticmethod
    def create() -> "Foo":
        return Foo()

    @staticmethod
    def plain():
        return Foo()

    @staticmethod
    def wrapped() -> Optional[Foo]:
        return Foo()

    def bar(self):
        pass


class Other:
    def bar(self):
        pass


def make() -> Foo:
    return Foo()


def maybe() -> Foo | None:
    return None
"#,
);

const PY_USE: (&str, &str) = (
    "use.py",
    r#"from foo import Foo, make, maybe


def use_it():
    Foo.create().bar()


def use_make():
    make().bar()


def use_maybe():
    maybe().bar()


def use_plain():
    Foo.plain().bar()


def use_wrapped():
    Foo.wrapped().bar()


def use_typed(f: Foo):
    f.bar()


def use_ctor():
    Foo().bar()
"#,
);

fn py_graph() -> ZeroCopyGraph {
    single(&PythonProvider::new().unwrap(), &[PY_FOO, PY_USE])
}

/// The quoted forward reference `-> "Foo"` reduces to `Foo`.
#[test]
fn test_resolve_call_python_static_factory_chain_binds_return_type_member() {
    let graph = py_graph();
    assert_binds_foo(&graph, "use_it", "bar");
    assert_inner_binds(&graph, "use_it", "Foo", "create");
}

#[test]
fn test_resolve_call_python_free_function_chain_binds_return_type_member() {
    assert_binds_foo(&py_graph(), "use_make", "bar");
}

/// `Foo | None` reduces to `Foo`.
#[test]
fn test_resolve_call_python_optional_union_chain_binds_return_type_member() {
    assert_binds_foo(&py_graph(), "use_maybe", "bar");
}

/// `Foo().bar()`: the inner call constructs `Foo`.
#[test]
fn test_resolve_call_python_constructor_receiver_chain_binds_class_member() {
    assert_binds_foo(&py_graph(), "use_ctor", "bar");
}

#[test]
fn test_resolve_call_python_untyped_factory_chain_keeps_fallback() {
    assert_falls_back(&py_graph(), "use_plain", "bar");
}

/// Follow-up: `Optional[Foo]` is not unwrapped yet.
#[test]
fn test_resolve_call_python_generic_return_chain_keeps_fallback() {
    assert_falls_back(&py_graph(), "use_wrapped", "bar");
}

#[test]
fn test_resolve_call_python_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&py_graph(), "use_typed", "bar");
}

// ---- Java ----

#[test]
fn test_resolve_call_java_factory_imported_return_type_cross_directory_binds_member() {
    let graph = single(
        &JavaProvider::new().unwrap(),
        &[
            ("src/models/Foo.java", "package models;\npublic class Foo { public void bar() {} }\n"),
            ("src/decoy/Other.java", "package decoy;\npublic class Other { public void bar() {} }\n"),
            ("src/services/Factory.java", "package services;\nimport models.Foo;\npublic class Factory { public static Foo make() { return new Foo(); } }\n"),
            ("src/app/Use.java", "package app;\nimport services.Factory;\npublic class Use { void caller() { Factory.make().bar(); } }\n"),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_binds_foo(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_java_factory_wildcard_imported_return_type_binds_member() {
    let graph = single(
        &JavaProvider::new().unwrap(),
        &[
            ("src/models/Foo.java", "package models;\npublic class Foo { public void bar() {} }\n"),
            ("src/decoy/Other.java", "package decoy;\npublic class Other { public void bar() {} }\n"),
            ("src/services/Factory.java", "package services;\nimport models.*;\npublic class Factory { public static Foo make() { return new Foo(); } }\n"),
            ("src/app/Use.java", "package app;\nimport services.Factory;\npublic class Use { void caller() { Factory.make().bar(); } }\n"),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_binds_foo(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_java_factory_missing_imported_return_type_returns_no_edge() {
    let graph = single(
        &JavaProvider::new().unwrap(),
        &[
            ("src/models/Present.java", "package models;\npublic class Present {}\n"),
            ("src/decoy/Missing.java", "package decoy;\npublic class Missing { public void bar() {} }\n"),
            ("src/services/Factory.java", "package services;\nimport models.Missing;\npublic class Factory { public static Missing make() { return null; } }\n"),
            ("src/app/Use.java", "package app;\nimport services.Factory;\npublic class Use { void caller() { Factory.make().bar(); } }\n"),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_no_member_edge(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_java_factory_external_imported_return_type_returns_no_edge() {
    let graph = single(
        &JavaProvider::new().unwrap(),
        &[
            ("src/decoy/Foo.java", "package decoy;\npublic class Foo { public void bar() {} }\n"),
            ("src/services/Factory.java", "package services;\nimport external.Foo;\npublic class Factory { public static Foo make() { return null; } }\n"),
            ("src/app/Use.java", "package app;\nimport services.Factory;\npublic class Use { void caller() { Factory.make().bar(); } }\n"),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_no_member_edge(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_java_factory_ambiguous_wildcard_imports_return_no_edge() {
    let graph = single(
        &JavaProvider::new().unwrap(),
        &[
            ("src/left/Foo.java", "package left;\npublic class Foo { public void bar() {} }\n"),
            ("src/right/Foo.java", "package right;\npublic class Foo { public void bar() {} }\n"),
            ("src/services/Factory.java", "package services;\nimport left.*;\nimport right.*;\npublic class Factory { public static Foo make() { return null; } }\n"),
            ("src/app/Use.java", "package app;\nimport services.Factory;\npublic class Use { void caller() { Factory.make().bar(); } }\n"),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_no_member_edge(&graph, "caller", "bar");
}

fn java_graph() -> ZeroCopyGraph {
    single(
        &JavaProvider::new().unwrap(),
        &[
            (
                "src/app/Foo.java",
                "package app;\n\npublic class Foo {\n    public static Foo create() { return new Foo(); }\n    public static Object plain() { return new Foo(); }\n    public static java.util.Optional<Foo> wrapped() { return null; }\n    public void bar() {}\n}\n",
            ),
            (
                "src/app/Other.java",
                "package app;\n\npublic class Other {\n    public void bar() {}\n}\n",
            ),
            (
                "src/app/Factory.java",
                "package app;\n\npublic class Factory {\n    public static Foo make() { return new Foo(); }\n}\n",
            ),
            (
                "src/app/Use.java",
                "package app;\n\npublic class Use {\n    void useIt() { Foo.create().bar(); }\n    void useMake() { Factory.make().bar(); }\n    void usePlain() { Foo.plain().bar(); }\n    void useWrapped() { Foo.wrapped().bar(); }\n    void useCast() { ((Foo) Foo.plain()).bar(); }\n    void useNew() { new Foo().bar(); }\n    void useTyped(Foo f) { f.bar(); }\n}\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_call_java_static_factory_chain_binds_return_type_member() {
    let graph = java_graph();
    assert_binds_foo(&graph, "useIt", "bar");
    assert_inner_binds(&graph, "useIt", "Foo", "create");
}

/// Java has no free function: a factory on another class.
#[test]
fn test_resolve_call_java_other_class_factory_chain_binds_return_type_member() {
    assert_binds_foo(&java_graph(), "useMake", "bar");
}

/// `Object` names no project type, so the chain emits no member edge.
#[test]
fn test_resolve_call_java_non_project_return_chain_emits_no_member_edge() {
    assert_falls_back(&java_graph(), "usePlain", "bar");
}

/// Follow-up: `Optional<Foo>` is not unwrapped yet.
#[test]
fn test_resolve_call_java_generic_return_chain_keeps_fallback() {
    assert_falls_back(&java_graph(), "useWrapped", "bar");
}

/// `expression_type` still types a cast and a `new` receiver by syntax.
#[test]
fn test_resolve_call_java_cast_and_new_receivers_still_bind_type_member() {
    let graph = java_graph();
    assert_binds_foo(&graph, "useCast", "bar");
    assert_binds_foo(&graph, "useNew", "bar");
}

#[test]
fn test_resolve_call_java_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&java_graph(), "useTyped", "bar");
}

// ---- Kotlin ----

#[test]
fn test_resolve_call_kotlin_factory_imported_return_type_cross_directory_binds_member() {
    let graph = single(
        &KotlinProvider::new().unwrap(),
        &[
            (
                "src/models/Foo.kt",
                "package models\nclass Foo { fun bar() {} }\n",
            ),
            (
                "src/decoy/Other.kt",
                "package decoy\nclass Other { fun bar() {} }\n",
            ),
            (
                "src/services/Factory.kt",
                "package services\nimport models.Foo\nobject Factory { fun make(): Foo = Foo() }\n",
            ),
            (
                "src/app/Use.kt",
                "package app\nimport services.Factory\nfun caller() { Factory.make().bar() }\n",
            ),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_binds_foo(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_kotlin_factory_wildcard_imported_return_type_binds_member() {
    let graph = single(
        &KotlinProvider::new().unwrap(),
        &[
            (
                "src/models/Foo.kt",
                "package models\nclass Foo { fun bar() {} }\n",
            ),
            (
                "src/decoy/Other.kt",
                "package decoy\nclass Other { fun bar() {} }\n",
            ),
            (
                "src/services/Factory.kt",
                "package services\nimport models.*\nobject Factory { fun make(): Foo = Foo() }\n",
            ),
            (
                "src/app/Use.kt",
                "package app\nimport services.Factory\nfun caller() { Factory.make().bar() }\n",
            ),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_binds_foo(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_kotlin_factory_alias_imported_return_type_binds_member() {
    let graph = single(
        &KotlinProvider::new().unwrap(),
        &[
            (
                "src/models/Foo.kt",
                "package models\nclass Foo { fun bar() {} }\n",
            ),
            (
                "src/decoy/Other.kt",
                "package decoy\nclass Other { fun bar() {} }\n",
            ),
            (
                "src/services/Factory.kt",
                "package services\nimport models.Foo as BoundFoo\nobject Factory { fun make(): BoundFoo = BoundFoo() }\n",
            ),
            (
                "src/app/Use.kt",
                "package app\nimport services.Factory\nfun caller() { Factory.make().bar() }\n",
            ),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_binds_foo(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_kotlin_factory_missing_imported_return_type_returns_no_edge() {
    let graph = single(
        &KotlinProvider::new().unwrap(),
        &[
            ("src/models/Present.kt", "package models\nclass Present\n"),
            ("src/decoy/Missing.kt", "package decoy\nclass Missing { fun bar() {} }\n"),
            (
                "src/services/Factory.kt",
                "package services\nimport models.Missing\nobject Factory { fun make(): Missing = TODO() }\n",
            ),
            (
                "src/app/Use.kt",
                "package app\nimport services.Factory\nfun caller() { Factory.make().bar() }\n",
            ),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_no_member_edge(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_kotlin_factory_external_imported_return_type_returns_no_edge() {
    let graph = single(
        &KotlinProvider::new().unwrap(),
        &[
            (
                "src/decoy/Foo.kt",
                "package decoy\nclass Foo { fun bar() {} }\n",
            ),
            (
                "src/services/Factory.kt",
                "package services\nimport external.Foo\nobject Factory { fun make(): Foo = TODO() }\n",
            ),
            (
                "src/app/Use.kt",
                "package app\nimport services.Factory\nfun caller() { Factory.make().bar() }\n",
            ),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_no_member_edge(&graph, "caller", "bar");
}

#[test]
fn test_resolve_call_kotlin_factory_ambiguous_wildcard_imports_return_no_edge() {
    let graph = single(
        &KotlinProvider::new().unwrap(),
        &[
            (
                "src/left/Foo.kt",
                "package left\nclass Foo { fun bar() {} }\n",
            ),
            (
                "src/right/Foo.kt",
                "package right\nclass Foo { fun bar() {} }\n",
            ),
            (
                "src/services/Factory.kt",
                "package services\nimport left.*\nimport right.*\nobject Factory { fun make(): Foo = TODO() }\n",
            ),
            (
                "src/app/Use.kt",
                "package app\nimport services.Factory\nfun caller() { Factory.make().bar() }\n",
            ),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_no_member_edge(&graph, "caller", "bar");
}

fn kotlin_graph() -> ZeroCopyGraph {
    single(
        &KotlinProvider::new().unwrap(),
        &[
            (
                "src/app/Foo.kt",
                "package app\n\nclass Foo {\n    fun bar() {}\n}\n\nclass Other {\n    fun bar() {}\n}\n\nobject Factory {\n    fun create(): Foo = Foo()\n}\n\nfun make(): Foo = Foo()\nfun plain() = Foo()\nfun wrapped(): List<Foo> = listOf(Foo())\nfun maybe(): Foo? = null\n",
            ),
            (
                "src/app/Use.kt",
                "package app\n\nfun useIt() {\n    Factory.create().bar()\n}\n\nfun useMake() {\n    make().bar()\n}\n\nfun useMaybe() {\n    maybe()?.bar()\n}\n\nfun usePlain() {\n    plain().bar()\n}\n\nfun useWrapped() {\n    wrapped().bar()\n}\n\nfun useTyped(f: Foo) {\n    f.bar()\n}\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_call_kotlin_object_factory_chain_binds_return_type_member() {
    let graph = kotlin_graph();
    assert_binds_foo(&graph, "useIt", "bar");
    assert_inner_binds(&graph, "useIt", "Factory", "create");
}

#[test]
fn test_resolve_call_kotlin_free_function_chain_binds_return_type_member() {
    assert_binds_foo(&kotlin_graph(), "useMake", "bar");
}

/// `Foo?` reduces to `Foo`.
#[test]
fn test_resolve_call_kotlin_nullable_return_chain_binds_return_type_member() {
    assert_binds_foo(&kotlin_graph(), "useMaybe", "bar");
}

/// An expression body with no declared type: today's bare `bar`.
#[test]
fn test_resolve_call_kotlin_untyped_factory_chain_keeps_fallback() {
    assert_falls_back(&kotlin_graph(), "usePlain", "bar");
}

/// Follow-up: `List<Foo>` is not unwrapped yet.
#[test]
fn test_resolve_call_kotlin_generic_return_chain_keeps_fallback() {
    assert_falls_back(&kotlin_graph(), "useWrapped", "bar");
}

#[test]
fn test_resolve_call_kotlin_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&kotlin_graph(), "useTyped", "bar");
}

// ---- C# ----

#[test]
fn test_resolve_call_csharp_factory_imported_return_type_cross_directory_binds_member() {
    let graph = single(
        &CSharpProvider::new().unwrap(),
        &[
            ("src/Models/Foo.cs", "namespace Models { public class Foo { public void Bar() {} } }\n"),
            ("src/Decoy/Other.cs", "namespace Decoy { public class Other { public void Bar() {} } }\n"),
            ("src/Services/Factory.cs", "using Models;\nnamespace Services { public class Factory { public static Foo Make() { return new Foo(); } } }\n"),
            ("src/App/Use.cs", "using Services;\nnamespace App { public class Use { public void Caller() { Factory.Make().Bar(); } } }\n"),
        ],
    );
    assert_inner_binds(&graph, "Caller", "Factory", "Make");
    assert_binds_foo(&graph, "Caller", "Bar");
}

#[test]
fn test_resolve_call_csharp_factory_alias_return_type_binds_imported_member() {
    let graph = single(
        &CSharpProvider::new().unwrap(),
        &[
            ("src/Models/ModelValue.cs", "namespace Models { public class ModelValue { public void Bar() {} } }\n"),
            ("src/Decoy/DecoyValue.cs", "namespace Decoy { public class DecoyValue { public void Bar() {} } }\n"),
            ("src/Services/Factory.cs", "using Alias = Models.ModelValue;\nnamespace Services { public class Factory { public static Alias Make() { return new Alias(); } } }\n"),
            ("src/App/Use.cs", "using Services;\nnamespace App { public class Use { public void Caller() { Factory.Make().Bar(); } } }\n"),
        ],
    );
    assert_inner_binds(&graph, "Caller", "Factory", "Make");
    assert_eq!(owners(&graph, "Caller", "Bar"), ["ModelValue"]);
}

#[test]
fn test_resolve_call_csharp_factory_file_scoped_namespace_return_type_binds_member() {
    let graph = single(
        &CSharpProvider::new().unwrap(),
        &[
            ("src/Models/ModelValue.cs", "namespace Models;\npublic class ModelValue { public void Bar() {} }\n"),
            ("src/Decoy/DecoyValue.cs", "namespace Decoy;\npublic class DecoyValue { public void Bar() {} }\n"),
            ("src/Services/Factory.cs", "using Models;\nnamespace Services;\npublic class Factory { public static ModelValue Make() { return new ModelValue(); } }\n"),
            ("src/App/Use.cs", "using Services;\nnamespace App;\npublic class Use { public void Caller() { Factory.Make().Bar(); } }\n"),
        ],
    );
    assert_inner_binds(&graph, "Caller", "Factory", "Make");
    assert_eq!(owners(&graph, "Caller", "Bar"), ["ModelValue"]);
}

#[test]
fn test_resolve_call_csharp_nested_known_missing_member_returns_no_outer_edge() {
    let graph = single(
        &CSharpProvider::new().unwrap(),
        &[
            ("src/Models/Foo.cs", "namespace Models { public class Foo {} }\n"),
            ("src/Decoy/Decoy.cs", "namespace Decoy { public class Decoy { public void Bar() {} } }\n"),
            ("src/Services/Factory.cs", "using Models;\nnamespace Services { public class Factory { public static Foo Make() { return new Foo(); } } }\n"),
            ("src/App/Use.cs", "using Services;\nnamespace App { public class Use { public void Caller() { Factory.Make().Missing().Bar(); } } }\n"),
        ],
    );
    assert_inner_binds(&graph, "Caller", "Factory", "Make");
    assert_no_member_edge(&graph, "Caller", "Missing");
    assert_no_member_edge(&graph, "Caller", "Bar");
}

fn csharp_graph() -> ZeroCopyGraph {
    single(
        &CSharpProvider::new().unwrap(),
        &[
            (
                "src/Foo.cs",
                "namespace App\n{\n    public class Foo\n    {\n        public static Foo Create() { return new Foo(); }\n        public static object Plain() { return new Foo(); }\n        public static Task<Foo> Wrapped() { return null; }\n        public static Foo? Maybe() { return null; }\n        public void Bar() {}\n    }\n\n    public class Other\n    {\n        public void Bar() {}\n    }\n\n    public static class Factory\n    {\n        public static Foo Make() { return new Foo(); }\n    }\n}\n",
            ),
            (
                "src/Use.cs",
                "namespace App\n{\n    public class Use\n    {\n        public void UseIt() { Foo.Create().Bar(); }\n        public void UseMake() { Factory.Make().Bar(); }\n        public void UseMaybe() { Foo.Maybe().Bar(); }\n        public void UsePlain() { Foo.Plain().Bar(); }\n        public void UseWrapped() { Foo.Wrapped().Bar(); }\n        public void UseTyped(Foo f) { f.Bar(); }\n    }\n}\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_call_csharp_static_factory_chain_binds_return_type_member() {
    let graph = csharp_graph();
    assert_binds_foo(&graph, "UseIt", "Bar");
    assert_inner_binds(&graph, "UseIt", "Foo", "Create");
}

#[test]
fn test_resolve_call_csharp_other_class_factory_chain_binds_return_type_member() {
    assert_binds_foo(&csharp_graph(), "UseMake", "Bar");
}

/// `Foo?` reduces to `Foo`.
#[test]
fn test_resolve_call_csharp_nullable_return_chain_binds_return_type_member() {
    assert_binds_foo(&csharp_graph(), "UseMaybe", "Bar");
}

/// `object` names no project type, so the chain emits no member edge.
#[test]
fn test_resolve_call_csharp_non_project_return_chain_emits_no_member_edge() {
    assert_falls_back(&csharp_graph(), "UsePlain", "Bar");
}

/// Follow-up: `Task<Foo>` is not unwrapped yet.
#[test]
fn test_resolve_call_csharp_generic_return_chain_keeps_fallback() {
    assert_falls_back(&csharp_graph(), "UseWrapped", "Bar");
}

#[test]
fn test_resolve_call_csharp_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&csharp_graph(), "UseTyped", "Bar");
}

// ---- Go ----

fn go_graph() -> ZeroCopyGraph {
    single(
        &GoProvider::new().unwrap(),
        &[
            (
                "app/foo.go",
                "package app\n\ntype Foo struct{}\n\nfunc NewFoo() *Foo { return &Foo{} }\n\nfunc Plain() interface{} { return &Foo{} }\n\nfunc Wrapped() []*Foo { return nil }\n\nfunc (f *Foo) Bar() {}\n\ntype Other struct{}\n\nfunc (o *Other) Bar() {}\n",
            ),
            (
                "app/use.go",
                "package app\n\nfunc build() Foo { return Foo{} }\n\nfunc UseIt() {\n\tNewFoo().Bar()\n}\n\nfunc UseBuild() {\n\tbuild().Bar()\n}\n\nfunc UsePlain() {\n\tPlain().Bar()\n}\n\nfunc UseWrapped() {\n\tWrapped().Bar()\n}\n\nfunc UseTyped(f *Foo) {\n\tf.Bar()\n}\n",
            ),
        ],
    )
}

/// Go has no static members: `NewFoo` is the factory, `*Foo` reduces to
/// `Foo`.
#[test]
fn test_resolve_call_go_factory_pointer_return_chain_binds_return_type_member() {
    let graph = go_graph();
    assert_binds_foo(&graph, "UseIt", "Bar");
    assert_inner_binds(&graph, "UseIt", "", "NewFoo");
}

/// A value return declared in the caller's own file.
#[test]
fn test_resolve_call_go_same_file_function_chain_binds_return_type_member() {
    assert_binds_foo(&go_graph(), "UseBuild", "Bar");
}

#[test]
fn test_resolve_call_go_non_project_return_chain_keeps_fallback() {
    assert_falls_back(&go_graph(), "UsePlain", "Bar");
}

#[test]
fn test_resolve_call_go_dot_import_external_type_returns_no_edge() {
    let graph = go_module(&[
        ("go.mod", "module example.com/app\n\ngo 1.21\n"),
        (
            "svc/svc.go",
            "package svc\nimport . \"github.com/ext/client\"\nfunc New() *Client { return nil }\nfunc Run() { New().Do() }\n",
        ),
        ("mock/client.go", "package mock\ntype Client struct{}\nfunc (Client) Do() {}\n"),
        ("other.go", "package app\ntype Other struct{}\nfunc (Other) Do() {}\n"),
    ]);
    assert_inner_binds(&graph, "Run", "", "New");
    assert_no_member_edge(&graph, "Run", "Do");
}

/// Follow-up: `[]*Foo` is not unwrapped yet.
#[test]
fn test_resolve_call_go_slice_return_chain_keeps_fallback() {
    assert_falls_back(&go_graph(), "UseWrapped", "Bar");
}

#[test]
fn test_resolve_call_go_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&go_graph(), "UseTyped", "Bar");
}

// ---- Rust ----

#[test]
fn test_resolve_call_rust_imported_cross_directory_return_type_binds_member() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/config.rs",
                "use crate::types::Foo; fn make() -> Foo { todo!() } fn run() { make().bar(); }",
            ),
            (
                "src/types/mod.rs",
                "pub struct Foo; impl Foo { pub fn bar(&self) {} }",
            ),
        ],
    );
    assert_binds_foo(&graph, "run", "bar");
}

#[test]
fn test_resolve_call_rust_external_return_type_rejects_project_namesake() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/config.rs",
                "use external::Foo; fn make() -> Foo { todo!() } fn run() { make().bar(); }",
            ),
            (
                "src/types/mod.rs",
                "pub struct Foo; impl Foo { pub fn bar(&self) {} }",
            ),
        ],
    );
    assert!(owners(&graph, "run", "bar").is_empty());
}

#[test]
fn test_resolve_call_cpp_quoted_same_directory_include_return_type_binds_member() {
    let graph = single(
        &CppProvider::new().unwrap(),
        &[
            (
                "src/config.cpp",
                "#include \"foo.h\"\nFoo* make();\nvoid run() { make()->bar(); }",
            ),
            ("src/foo.h", "class Foo { public: void bar(); };"),
        ],
    );
    assert_binds_foo(&graph, "run", "bar");
}

#[test]
fn test_resolve_call_rust_external_wildcard_return_type_rejects_project_namesake() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod app; pub mod models;"),
            (
                "src/app.rs",
                "use external::*; fn load() -> Config { todo!() } fn run() { load().get(); }",
            ),
            ("src/models.rs", "pub mod config;"),
            (
                "src/models/config.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
        ],
    );
    assert!(owners(&graph, "run", "get").is_empty());
}

#[test]
fn test_resolve_call_rust_ambiguous_wildcard_return_type_rejects_members() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod app; pub mod left; pub mod right;"),
            (
                "src/app.rs",
                "use crate::left::*; use crate::right::*; fn load() -> Config { todo!() } fn run() { load().get(); }",
            ),
            (
                "src/left.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
            (
                "src/right.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
        ],
    );
    assert!(owners(&graph, "run", "get").is_empty());
}

#[test]
fn test_resolve_call_rust_named_external_import_precedes_local_glob() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod app; pub mod local;"),
            (
                "src/app.rs",
                "use external::Config; use crate::local::*; fn load() -> Config { todo!() } fn run() { load().get(); }",
            ),
            (
                "src/local.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
        ],
    );
    assert!(owners(&graph, "run", "get").is_empty());
}

#[test]
fn test_resolve_call_rust_local_glob_does_not_mask_external_reexport_glob() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod app; pub mod local; pub mod relay;"),
            (
                "src/app.rs",
                "use crate::local::*; use crate::relay::*; fn load() -> Config { todo!() } fn run() { load().get(); }",
            ),
            (
                "src/local.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
            ("src/relay.rs", "pub use external::Config;"),
        ],
    );
    assert!(owners(&graph, "run", "get").is_empty());
}

/// rustc rejects a name two globs both supply, so a local glob hit
/// outranks a direct glob the graph cannot open.
#[test]
fn test_resolve_call_rust_local_glob_beside_external_glob_binds_member() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod app; pub mod settings;"),
            (
                "src/app.rs",
                "use crate::settings::*; use std::io::prelude::*; fn load() -> Config { todo!() } fn run() { load().get(); }",
            ),
            (
                "src/settings.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
        ],
    );
    assert_eq!(
        owners(&graph, "run", "get"),
        ["Config"],
        "{:?}",
        targets(&graph, "run")
    );
}

/// Rust 2018 uniform paths: `use config::T` names the `mod config` beside it.
#[test]
fn test_resolve_call_rust_bare_module_import_return_type_binds_member() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/main.rs",
                "mod config; mod other; use config::Settings; fn load() -> Settings { Settings } fn run() { load().validate_paths(); }",
            ),
            (
                "src/config.rs",
                "pub struct Settings; impl Settings { pub fn validate_paths(&self) {} }",
            ),
            (
                "src/other.rs",
                "pub struct Other; impl Other { pub fn validate_paths(&self) {} }",
            ),
        ],
    );
    assert_eq!(
        owners(&graph, "run", "validate_paths"),
        ["Settings"],
        "{:?}",
        targets(&graph, "run")
    );
}

/// `self::config` from `src/models.rs` is `src/models/config.rs`; the
/// crate-root `src/config.rs` namesake is a different module.
#[test]
fn test_resolve_call_rust_child_module_reexport_ignores_parent_namesake() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod app; pub mod config; pub mod models;"),
            (
                "src/app.rs",
                "use crate::models::Config; fn load() -> Config { todo!() } fn run() { load().get(); }",
            ),
            ("src/models.rs", "pub mod config; pub use self::config::Config;"),
            (
                "src/models/config.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
            (
                "src/config.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
        ],
    );
    let gets: Vec<_> = calls_from(&graph, "run")
        .into_iter()
        .filter(|hit| hit.name == "get")
        .map(|hit| hit.file)
        .collect();
    assert_eq!(
        gets,
        ["src/models/config.rs"],
        "{:?}",
        targets(&graph, "run")
    );
}

#[test]
fn test_resolve_call_rust_named_alias_return_type_binds_imported_member() {
    for import in [
        "use crate::models::Config as Alias;",
        "use crate::models::{Config as Alias};",
    ] {
        let graph = single(
            &RustProvider::new().unwrap(),
            &[
                ("src/lib.rs", "pub mod app; pub mod models;"),
                (
                    "src/app.rs",
                    &format!(
                        "{import} fn load() -> Alias {{ todo!() }} fn run() {{ load().get(); }}"
                    ),
                ),
                (
                    "src/models.rs",
                    "pub struct Config; impl Config { pub fn get(&self) {} }",
                ),
            ],
        );
        assert_eq!(owners(&graph, "run", "get"), ["Config"], "{import}");
    }
}

#[test]
fn test_resolve_call_rust_external_named_alias_return_type_has_no_project_edge() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod app; pub mod models;"),
            (
                "src/app.rs",
                "use external::Config as Alias; fn load() -> Alias { todo!() } fn run() { load().get(); }",
            ),
            (
                "src/models.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
        ],
    );
    assert!(owners(&graph, "run", "get").is_empty());
}

#[test]
fn test_resolve_call_rust_mod_reexport_return_type_binds_imported_member() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod app; pub mod models;"),
            (
                "src/models/mod.rs",
                "mod config; pub use config::Config;",
            ),
            (
                "src/models/config.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
            (
                "src/app.rs",
                "use crate::models::Config; pub fn load() -> Config { Config } pub fn run() { load().get(); }",
            ),
        ],
    );
    assert_eq!(owners(&graph, "run", "get"), ["Config"]);
}

#[test]
fn test_resolve_call_rust_root_reexport_return_type_binds_imported_member() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/lib.rs",
                "mod config; pub use config::Config; pub mod app;",
            ),
            (
                "src/config.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
            (
                "src/app.rs",
                "use crate::Config; pub fn load() -> Config { Config } pub fn run() { load().get(); }",
            ),
        ],
    );
    assert_eq!(owners(&graph, "run", "get"), ["Config"]);
}

#[test]
fn test_resolve_call_cpp_quoted_include_preserves_root_candidate() {
    let graph = single(
        &CppProvider::new().unwrap(),
        &[
            (
                "src/config.cpp",
                "#include \"foo.h\"\nFoo* make();\nvoid run() { make()->bar(); }",
            ),
            ("foo.h", "class Foo { public: void bar(); };"),
        ],
    );
    assert_binds_foo(&graph, "run", "bar");
}

#[test]
fn test_resolve_call_rust_external_reexport_rejects_adjacent_undeclared_module() {
    for declarations in ["", "mod inner { mod config {} }"] {
        let module = format!("{declarations} pub use config::Config;");
        let graph = single(
            &RustProvider::new().unwrap(),
            &[
                ("src/lib.rs", "pub mod app; pub mod models;"),
                ("src/models/mod.rs", &module),
                (
                    "src/models/config.rs",
                    "pub struct Config; impl Config { pub fn get(&self) {} }",
                ),
                (
                    "src/app.rs",
                    "use crate::models::Config; pub fn load() -> Config { Config } pub fn run() { load().get(); }",
                ),
            ],
        );
        assert!(owners(&graph, "run", "get").is_empty(), "{module}");
    }
}

#[test]
fn test_resolve_call_rust_external_pub_use_return_type_rejects_project_namesake() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/lib.rs",
                "pub use external::Config; pub mod app; pub mod legacy;",
            ),
            (
                "src/legacy.rs",
                "pub struct Config; impl Config { pub fn get(&self) {} }",
            ),
            (
                "src/app.rs",
                "use crate::Config; fn load() -> Config { todo!() } fn run() { load().get(); }",
            ),
        ],
    );
    assert!(owners(&graph, "run", "get").is_empty());
}

#[test]
fn test_resolve_call_rust_wildcard_import_unique_return_type_binds_member() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/lib.rs",
                "mod models; use crate::models::*; fn make() -> Foo { Foo } fn run() { make().bar(); }",
            ),
            (
                "src/models.rs",
                "pub struct Foo; impl Foo { pub fn bar(&self) {} }",
            ),
        ],
    );
    assert_binds_foo(&graph, "run", "bar");
}

#[test]
fn test_resolve_call_php_imported_cross_directory_return_type_binds_member() {
    let graph = single(&PhpProvider::new().unwrap(), &[
        ("src/config.php", "<?php use App\\Entity\\Foo; function make(): Foo { return new Foo(); } function run() { make()->bar(); }"),
        ("src/Entity/Foo.php", "<?php namespace App\\Entity; class Foo { public function bar() {} }"),
    ]);
    assert_binds_foo(&graph, "run", "bar");
}

#[test]
fn test_resolve_call_php_external_return_type_rejects_project_namesake() {
    let graph = single(&PhpProvider::new().unwrap(), &[
        ("src/config.php", "<?php use External\\Foo; function make(): Foo { return new Foo(); } function run() { make()->bar(); }"),
        ("src/Entity/Foo.php", "<?php namespace App\\Entity; class Foo { public function bar() {} }"),
    ]);
    assert!(owners(&graph, "run", "bar").is_empty());
}

#[test]
fn test_resolve_call_php_same_namespace_cross_directory_unique_return_type_binds_member() {
    let graph = single(&PhpProvider::new().unwrap(), &[
        ("app/config.php", "<?php namespace App; function make(): Foo { return new Foo(); } function run() { make()->bar(); }"),
        ("src/Models/Foo.php", "<?php namespace App; class Foo { public function bar() {} }"),
    ]);
    assert_binds_foo(&graph, "run", "bar");
}

#[test]
fn test_resolve_call_php_ambiguous_return_type_keeps_unique_member_fallback() {
    let graph = single(&PhpProvider::new().unwrap(), &[
        ("app/config.php", "<?php namespace App; function make(): Foo { return new Foo(); } function run() { make()->bar(); }"),
        ("src/one/Foo.php", "<?php namespace App; class Foo {}"),
        ("src/two/Foo.php", "<?php namespace App; class Foo { public function bar() {} }"),
    ]);
    let bars: Vec<_> = calls_from(&graph, "run")
        .into_iter()
        .filter(|hit| hit.name == "bar")
        .collect();
    assert_eq!(bars.len(), 1, "all targets: {:?}", targets(&graph, "run"));
    assert_eq!(bars[0].file, "src/two/Foo.php");
    assert_eq!(bars[0].confidence, 0.7);
}

#[test]
fn test_resolve_call_dart_imported_cross_directory_return_type_binds_member() {
    let graph = single(
        &DartProvider::new().unwrap(),
        &[
            (
                "lib/config.dart",
                "import 'entity/foo.dart'; Foo make() => Foo(); void run() { make().bar(); }",
            ),
            ("lib/entity/foo.dart", "class Foo { void bar() {} }"),
        ],
    );
    assert_binds_foo(&graph, "run", "bar");
}

#[test]
fn test_resolve_call_cpp_included_cross_directory_return_type_binds_member() {
    let graph = single(&CppProvider::new().unwrap(), &[
        ("src/config.cpp", "#include \"../include/foo.h\"\nFoo* make() { return nullptr; } void run() { make()->bar(); }"),
        ("include/foo.h", "class Foo { public: void bar() {} };"),
    ]);
    assert_binds_foo(&graph, "run", "bar");
}

#[test]
fn test_resolve_call_dart_imported_return_type_rejects_unavailable_type() {
    for import in [
        "import 'entity/foo.dart' as entity;",
        "import 'missing/foo.dart';",
        "import 'package:external/foo.dart';",
        "import 'entity/foo.dart'; import 'other/foo.dart';",
    ] {
        let source = format!("{import} Foo make() => throw 0; void run() {{ make().bar(); }}");
        let graph = single(
            &DartProvider::new().unwrap(),
            &[
                ("lib/config.dart", &source),
                ("lib/entity/foo.dart", "class Foo { void bar() {} }"),
                ("lib/other/foo.dart", "class Foo { void bar() {} }"),
            ],
        );
        assert!(owners(&graph, "run", "bar").is_empty(), "{import}");
    }
}

/// The nearest ancestor holding `lib/<path>` is the importer's own package,
/// from the package root, `lib/`, `test/` or a nested test directory.
#[test]
fn test_resolve_call_dart_project_package_import_return_type_binds_member() {
    for (importer, root) in [
        ("main.dart", ""),
        ("lib/config.dart", ""),
        ("test/config_test.dart", ""),
        ("packages/app/test/a/x_test.dart", "packages/app/"),
    ] {
        let source = "import 'package:app/entity/foo.dart'; Foo make() => throw 0; void run() { make().bar(); }";
        let own = format!("{root}lib/entity/foo.dart");
        let decoy = format!("{root}lib/other/foo.dart");
        let mut files = vec![
            (importer, source),
            (own.as_str(), "class Foo { void bar() {} }"),
            (decoy.as_str(), "class Foo { void bar() {} }"),
        ];
        if !root.is_empty() {
            // A farther package root must not win over the nearest one.
            files.push(("lib/entity/foo.dart", "class Foo { void bar() {} }"));
        }
        let graph = single(&DartProvider::new().unwrap(), &files);
        assert_eq!(
            targets(&graph, "run")
                .into_iter()
                .filter(|(_, _, name)| name == "bar")
                .collect::<Vec<_>>(),
            vec![(own.clone(), "Foo".into(), "bar".into())],
            "{importer}"
        );
    }
}

/// An untyped call receiver keeps the member name only: no free function,
/// import or inherited method of that name is evidence for it.
#[test]
fn test_resolve_call_js_ts_untyped_call_receiver_member_binds_no_import_or_heritage() {
    for (provider, path) in [
        (
            &JavaScriptProvider::new().unwrap() as &dyn LanguageProvider,
            "main.js",
        ),
        (
            &TypeScriptProvider::new().unwrap() as &dyn LanguageProvider,
            "main.ts",
        ),
    ] {
        let util = path.replace("main", "util");
        let base = path.replace("main", "base");
        let graph = single(
            provider,
            &[
                (
                    path,
                    "import { json } from './util';\nimport { Base } from './base';\n\
                     class K extends Base { run(u) { fetch(u).json(); getRepo().save(); } }\n\
                     function top(u) { fetch(u).json(); new Response(u).json(); }\n",
                ),
                (util.as_str(), "export function json() {}\n"),
                (base.as_str(), "export class Base { save() {} }\n"),
            ],
        );
        for caller in ["run", "top"] {
            assert!(owners(&graph, caller, "json").is_empty(), "{path} {caller}");
        }
        // Only the project-wide member-name fallback (0.7) remains, never
        // the inherited method at the heritage tier (0.8).
        let saves: Vec<f32> = calls_from(&graph, "run")
            .into_iter()
            .filter(|hit| hit.name == "save")
            .map(|hit| hit.confidence)
            .collect();
        assert!(
            !saves.is_empty() && saves.iter().all(|&c| c <= 0.7),
            "{path} {saves:?}"
        );
    }
}

#[test]
fn test_resolve_call_dart_part_file_external_library_type_returns_no_edge() {
    let graph = single(
        &DartProvider::new().unwrap(),
        &[
            (
                "lib.dart",
                "import 'package:external/client.dart';\npart 'src/part.dart';\n",
            ),
            (
                "src/part.dart",
                "part of '../lib.dart';\nClient make() => Client();\nvoid run() { make().doWork(); }\n",
            ),
            ("mock/client.dart", "class Client { void doWork() {} }"),
            ("other.dart", "class Other { void doWork() {} }"),
        ],
    );
    assert_inner_binds(&graph, "run", "", "make");
    assert_no_member_edge(&graph, "run", "doWork");
}

#[test]
fn test_resolve_call_dart_named_part_file_external_library_type_returns_no_edge() {
    let graph = single(
        &DartProvider::new().unwrap(),
        &[
            (
                "lib.dart",
                "library helpers;\nimport 'package:external/client.dart';\npart 'src/part.dart';\n",
            ),
            (
                "src/part.dart",
                "part of helpers;\nClient make() => Client();\nvoid run() { make().doWork(); }\n",
            ),
            ("src/helpers.dart", "class Client { void doWork() {} }"),
            ("other.dart", "class Other { void doWork() {} }"),
        ],
    );
    assert_inner_binds(&graph, "run", "", "make");
    assert_no_member_edge(&graph, "run", "doWork");
}

/// A part file's library type keeps the parser fallback main had.
#[test]
fn test_resolve_call_dart_part_file_library_type_keeps_fallback_edge() {
    let graph = single(
        &DartProvider::new().unwrap(),
        &[
            (
                "lib.dart",
                "part 'src/part.dart';\nclass Foo { void bar() {} }\n",
            ),
            (
                "src/part.dart",
                "part of '../lib.dart';\nFoo make() => Foo();\nvoid run() { make().bar(); }\n",
            ),
        ],
    );
    let bars: Vec<_> = calls_from(&graph, "run")
        .into_iter()
        .filter(|hit| hit.name == "bar")
        .collect();
    assert_eq!(bars.len(), 1, "all targets: {:?}", targets(&graph, "run"));
    assert_eq!(
        (bars[0].file.as_str(), bars[0].owner.as_str()),
        ("lib.dart", "Foo")
    );
    assert_eq!(bars[0].confidence, 0.7);
}

#[test]
fn test_resolve_call_dart_filtered_import_rejects_hidden_project_namesake() {
    for combinator in ["hide Foo", "show Other"] {
        let source = format!("import 'entity/foo.dart' {combinator}; import 'package:external/foo.dart'; Foo make() => throw 0; void run() {{ make().bar(); }}");
        let graph = single(
            &DartProvider::new().unwrap(),
            &[
                ("lib/config.dart", &source),
                (
                    "lib/entity/foo.dart",
                    "class Foo { void bar() {} } class Other {}",
                ),
            ],
        );
        assert!(owners(&graph, "run", "bar").is_empty(), "{combinator}");
    }
}

/// A precompiled header reaches `foo.h` through a second include; the
/// adjacent declaration stays evidence. Ambiguous includes stay sticky.
#[test]
fn test_resolve_call_cpp_indirect_include_same_directory_type_binds_member() {
    let source = "#include \"pch.h\"\nFoo* make();\nvoid run() { make()->bar(); }";
    let graph = single(
        &CppProvider::new().unwrap(),
        &[
            ("src/main.cpp", source),
            ("src/pch.h", "#include \"foo.h\""),
            ("src/foo.h", "class Foo { public: void bar(); };"),
        ],
    );
    assert_eq!(
        owners(&graph, "run", "bar"),
        ["Foo"],
        "{:?}",
        targets(&graph, "run")
    );

    let ambiguous = "#include \"../include/foo.h\"\n#include \"../other/foo.h\"\nFoo* make();\nvoid run() { make()->bar(); }";
    let graph = single(
        &CppProvider::new().unwrap(),
        &[
            ("src/main.cpp", ambiguous),
            ("src/foo.h", "class Foo { public: void bar(); };"),
            ("include/foo.h", "class Foo { public: void bar() {} };"),
            ("other/foo.h", "class Foo { public: void bar() {} };"),
        ],
    );
    assert!(
        owners(&graph, "run", "bar").is_empty(),
        "{:?}",
        targets(&graph, "run")
    );
}

#[test]
fn test_resolve_call_cpp_imported_return_type_rejects_unavailable_type() {
    for include in [
        "#include \"../missing/foo.h\"",
        "#include \"../include/foo.h\"\n#include \"../other/foo.h\"",
    ] {
        let source =
            format!("{include}\nFoo* make() {{ return nullptr; }} void run() {{ make()->bar(); }}");
        let graph = single(
            &CppProvider::new().unwrap(),
            &[
                ("src/config.cpp", &source),
                ("include/foo.h", "class Foo { public: void bar() {} };"),
                ("other/foo.h", "class Foo { public: void bar() {} };"),
            ],
        );
        assert!(owners(&graph, "run", "bar").is_empty(), "{include}");
    }
}

/// One file: the GraphBuilder here runs with no module tree.
fn rust_graph() -> ZeroCopyGraph {
    single(
        &RustProvider::new().unwrap(),
        &[(
            "src/lib.rs",
            "pub struct Foo;\n\nimpl Foo {\n    pub fn create() -> Self { Foo }\n    pub fn plain() {}\n    pub fn wrapped() -> Option<Foo> { None }\n    pub fn bar(&self) {}\n}\n\npub struct Other;\n\nimpl Other {\n    pub fn bar(&self) {}\n}\n\npub fn make() -> Foo { Foo }\n\npub fn use_it() {\n    Foo::create().bar();\n}\n\npub fn use_make() {\n    make().bar();\n}\n\npub fn use_plain() {\n    Foo::plain().bar();\n}\n\npub fn use_wrapped() {\n    Foo::wrapped().bar();\n}\n\npub fn use_typed(f: Foo) {\n    f.bar();\n}\n",
        )],
    )
}

/// `-> Self` stands for the `impl` block's type.
#[test]
fn test_resolve_call_rust_self_returning_constructor_chain_binds_return_type_member() {
    let graph = rust_graph();
    assert_binds_foo(&graph, "use_it", "bar");
    assert_inner_binds(&graph, "use_it", "Foo", "create");
}

#[test]
fn test_resolve_call_rust_free_function_chain_binds_return_type_member() {
    assert_binds_foo(&rust_graph(), "use_make", "bar");
}

/// A unit return: today's untyped member.
#[test]
fn test_resolve_call_rust_unit_return_chain_keeps_fallback() {
    assert_falls_back(&rust_graph(), "use_plain", "bar");
}

/// Follow-up: `Option<Foo>` is not unwrapped yet.
#[test]
fn test_resolve_call_rust_generic_return_chain_keeps_fallback() {
    assert_falls_back(&rust_graph(), "use_wrapped", "bar");
}

#[test]
fn test_resolve_call_rust_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&rust_graph(), "use_typed", "bar");
}

// ---- PHP ----

fn php_graph() -> ZeroCopyGraph {
    single(
        &PhpProvider::new().unwrap(),
        &[
            (
                "src/Foo.php",
                "<?php\nclass Foo {\n    public static function create(): static { return new static(); }\n    public static function plain() { return new Foo(); }\n    public static function wrapped(): array { return []; }\n    public function bar() {}\n    public function baz() { $this->bar(); }\n}\n\nclass Other {\n    public function bar() {}\n}\n\nfunction make(): Foo { return new Foo(); }\n\nfunction maybe(): ?Foo { return null; }\n",
            ),
            (
                "src/use.php",
                "<?php\nfunction useIt() { Foo::create()->bar(); }\nfunction useMake() { make()->bar(); }\nfunction useMaybe() { maybe()?->bar(); }\nfunction usePlain() { Foo::plain()->bar(); }\nfunction useWrapped() { Foo::wrapped()->bar(); }\nfunction useNew() { (new Foo())->bar(); }\n",
            ),
        ],
    )
}

/// `: static` stands for the declaring class.
#[test]
fn test_resolve_call_php_static_factory_chain_binds_return_type_member() {
    let graph = php_graph();
    assert_binds_foo(&graph, "useIt", "bar");
    assert_inner_binds(&graph, "useIt", "Foo", "create");
}

#[test]
fn test_resolve_call_php_free_function_chain_binds_return_type_member() {
    assert_binds_foo(&php_graph(), "useMake", "bar");
}

/// `?Foo` reduces to `Foo`; `?->` keeps the call a member call.
#[test]
fn test_resolve_call_php_nullable_return_chain_binds_return_type_member() {
    assert_binds_foo(&php_graph(), "useMaybe", "bar");
}

/// `(new Foo())->bar()`: the inner call constructs `Foo`.
#[test]
fn test_resolve_call_php_new_receiver_chain_binds_class_member() {
    assert_binds_foo(&php_graph(), "useNew", "bar");
}

#[test]
fn test_resolve_call_php_untyped_factory_chain_keeps_fallback() {
    assert_falls_back(&php_graph(), "usePlain", "bar");
}

/// `array` (PHP has no generic signature) names no project type.
#[test]
fn test_resolve_call_php_array_return_chain_keeps_fallback() {
    assert_falls_back(&php_graph(), "useWrapped", "bar");
}

/// PHP types only `$this`: it still binds the enclosing class.
#[test]
fn test_resolve_call_php_this_receiver_still_binds_own_member() {
    assert_binds_foo(&php_graph(), "baz", "bar");
}

// ---- Ruby ----

fn ruby_graph() -> ZeroCopyGraph {
    single(
        &RubyProvider::new().unwrap(),
        &[
            (
                "lib/foo.rb",
                "class Foo\n  def self.create\n    new\n  end\n\n  def self.build\n    new\n  end\n\n  def bar\n  end\nend\n\nclass Other\n  def bar\n  end\nend\n\ndef make\n  Foo.new\nend\n",
            ),
            (
                "lib/use.rb",
                "def use_it\n  Foo.create.bar\nend\n\ndef use_build\n  Foo.build().bar\nend\n\ndef use_make\n  make().bar\nend\n\ndef use_new\n  Foo.new.bar\nend\n",
            ),
        ],
    )
}

/// The `TYPE_PRESERVING_METHODS` name heuristic still types `create` and
/// `new` chains.
#[test]
fn test_resolve_call_ruby_type_preserving_chain_still_binds_type_member() {
    let graph = ruby_graph();
    assert_binds_foo(&graph, "use_it", "bar");
    assert_binds_foo(&graph, "use_new", "bar");
}

/// Ruby declares no return types: a call receiver the heuristic misses
/// keeps today's untyped member.
#[test]
fn test_resolve_call_ruby_untyped_call_receiver_chain_keeps_fallback() {
    let graph = ruby_graph();
    assert_falls_back(&graph, "use_make", "bar");
    assert_falls_back(&graph, "use_build", "bar");
}

// ---- Swift ----

#[test]
fn test_resolve_call_swift_factory_imported_return_type_cross_directory_binds_member() {
    let graph = single(
        &SwiftProvider::new().unwrap(),
        &[
            ("Sources/Models/Foo.swift", "public class Foo { public func bar() {} }\n"),
            ("Sources/Decoy/Other.swift", "public class Other { public func bar() {} }\n"),
            ("Sources/Services/Factory.swift", "import Models\npublic class Factory { public static func make() -> Foo { return Foo() } }\n"),
            ("Sources/App/Use.swift", "import Services\nfunc caller() { Factory.make().bar() }\n"),
        ],
    );
    assert_inner_binds(&graph, "caller", "Factory", "make");
    assert_binds_foo(&graph, "caller", "bar");
}

fn swift_graph() -> ZeroCopyGraph {
    single(
        &SwiftProvider::new().unwrap(),
        &[
            (
                "Sources/Foo.swift",
                "class Foo {\n    static func create() -> Foo { return Foo() }\n    static func plain() {}\n    static func wrapped() -> [Foo] { return [] }\n    static func maybe() -> Foo? { return nil }\n    func bar() {}\n}\n\nclass Other {\n    func bar() {}\n}\n\nfunc make() -> Foo { return Foo() }\n",
            ),
            (
                "Sources/Use.swift",
                "func useIt() {\n    Foo.create().bar()\n}\n\nfunc useMake() {\n    make().bar()\n}\n\nfunc useMaybe() {\n    Foo.maybe()?.bar()\n}\n\nfunc usePlain() {\n    Foo.plain().bar()\n}\n\nfunc useWrapped() {\n    Foo.wrapped().bar()\n}\n\nfunc useTyped(f: Foo) {\n    f.bar()\n}\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_call_swift_static_factory_chain_binds_return_type_member() {
    let graph = swift_graph();
    assert_binds_foo(&graph, "useIt", "bar");
    assert_inner_binds(&graph, "useIt", "Foo", "create");
}

#[test]
fn test_resolve_call_swift_free_function_chain_binds_return_type_member() {
    assert_binds_foo(&swift_graph(), "useMake", "bar");
}

/// `Foo?` reduces to `Foo`.
#[test]
fn test_resolve_call_swift_optional_return_chain_binds_return_type_member() {
    assert_binds_foo(&swift_graph(), "useMaybe", "bar");
}

#[test]
fn test_resolve_call_swift_untyped_factory_chain_keeps_fallback() {
    assert_falls_back(&swift_graph(), "usePlain", "bar");
}

/// Follow-up: `[Foo]` is not unwrapped yet.
#[test]
fn test_resolve_call_swift_array_return_chain_keeps_fallback() {
    assert_falls_back(&swift_graph(), "useWrapped", "bar");
}

#[test]
fn test_resolve_call_swift_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&swift_graph(), "useTyped", "bar");
}

// ---- C++ ----

fn cpp_graph() -> ZeroCopyGraph {
    single(
        &CppProvider::new().unwrap(),
        &[
            (
                "src/foo.cpp",
                "class Foo {\npublic:\n    static Foo create() { return Foo(); }\n    static Foo* createPtr() { return new Foo(); }\n    static auto plain() { return Foo(); }\n    static std::vector<Foo> wrapped() { return {}; }\n    void bar() {}\n};\n\nclass Other {\npublic:\n    void bar() {}\n};\n\nFoo make() { return Foo(); }\n",
            ),
            (
                "src/use.cpp",
                "void useIt() { Foo::create().bar(); }\nvoid usePtr() { Foo::createPtr()->bar(); }\nvoid useMake() { make().bar(); }\nvoid usePlain() { Foo::plain().bar(); }\nvoid useWrapped() { Foo::wrapped().bar(); }\nvoid useTyped(Foo f) { f.bar(); }\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_call_cpp_static_factory_chain_binds_return_type_member() {
    let graph = cpp_graph();
    assert_binds_foo(&graph, "useIt", "bar");
    assert_inner_binds(&graph, "useIt", "Foo", "create");
}

/// `Foo*` with `->`: the pointer sits in the declarator, the type is `Foo`.
#[test]
fn test_resolve_call_cpp_pointer_factory_chain_binds_return_type_member() {
    assert_binds_foo(&cpp_graph(), "usePtr", "bar");
}

#[test]
fn test_resolve_call_cpp_free_function_chain_binds_return_type_member() {
    assert_binds_foo(&cpp_graph(), "useMake", "bar");
}

/// A deduced `auto` return: today's untyped member.
#[test]
fn test_resolve_call_cpp_auto_return_chain_keeps_fallback() {
    assert_falls_back(&cpp_graph(), "usePlain", "bar");
}

/// Follow-up: `std::vector<Foo>` is not unwrapped yet.
#[test]
fn test_resolve_call_cpp_generic_return_chain_keeps_fallback() {
    assert_falls_back(&cpp_graph(), "useWrapped", "bar");
}

#[test]
fn test_resolve_call_cpp_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&cpp_graph(), "useTyped", "bar");
}

// ---- Dart ----

fn dart_graph() -> ZeroCopyGraph {
    single(
        &DartProvider::new().unwrap(),
        &[
            (
                "lib/foo.dart",
                "class Foo {\n  static Foo create() => Foo();\n  static plain() => Foo();\n  static Future<Foo> wrapped() async => Foo();\n  static Foo? maybe() => null;\n  void bar() {}\n}\n\nclass Other {\n  void bar() {}\n}\n\nFoo make() => Foo();\n",
            ),
            (
                "lib/use.dart",
                "import 'foo.dart';\n\nvoid useIt() {\n  Foo.create().bar();\n}\n\nvoid useMake() {\n  make().bar();\n}\n\nvoid useMaybe() {\n  Foo.maybe()?.bar();\n}\n\nvoid usePlain() {\n  Foo.plain().bar();\n}\n\nvoid useWrapped() {\n  Foo.wrapped().bar();\n}\n\nvoid useTyped(Foo f) {\n  f.bar();\n}\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_call_dart_static_factory_chain_binds_return_type_member() {
    let graph = dart_graph();
    assert_binds_foo(&graph, "useIt", "bar");
    assert_inner_binds(&graph, "useIt", "Foo", "create");
}

#[test]
fn test_resolve_call_dart_free_function_chain_binds_return_type_member() {
    assert_binds_foo(&dart_graph(), "useMake", "bar");
}

/// `Foo?` reduces to `Foo`.
#[test]
fn test_resolve_call_dart_nullable_return_chain_binds_return_type_member() {
    assert_binds_foo(&dart_graph(), "useMaybe", "bar");
}

#[test]
fn test_resolve_call_dart_untyped_factory_chain_keeps_fallback() {
    assert_falls_back(&dart_graph(), "usePlain", "bar");
}

/// Follow-up: `Future<Foo>` is not unwrapped yet.
#[test]
fn test_resolve_call_dart_generic_return_chain_keeps_fallback() {
    assert_falls_back(&dart_graph(), "useWrapped", "bar");
}

#[test]
fn test_resolve_call_dart_typed_param_receiver_still_binds_type_member() {
    assert_binds_foo(&dart_graph(), "useTyped", "bar");
}

// ---- C: no methods ----

/// A nested call is two direct calls: the chain rule never touches them.
#[test]
fn test_resolve_call_c_nested_free_calls_still_bind_both() {
    let graph = single(
        &CProvider::new().unwrap(),
        &[
            (
                "src/foo.c",
                "struct Foo { int x; };\nstruct Foo *foo_create(void) { return 0; }\nvoid foo_bar(struct Foo *f) {}\n",
            ),
            (
                "src/use.c",
                "struct Foo;\nstruct Foo *foo_create(void);\nvoid foo_bar(struct Foo *f);\nvoid use_it(void) { foo_bar(foo_create()); }\n",
            ),
        ],
    );
    let names: Vec<String> = targets(&graph, "use_it")
        .into_iter()
        .map(|(_, _, name)| name)
        .collect();
    assert!(names.contains(&"foo_create".to_string()), "{names:?}");
    assert!(names.contains(&"foo_bar".to_string()), "{names:?}");
}

// ---- chain fallback length (JavaScript) ----

/// The fallback stores a fixed receiver marker and member, not receiver text.
/// A long chain stays near-linear: receiver text made every site O(depth) and
/// the file O(depth^3) bytes. The bound stands for "not cubic", a cubic file
/// is megabytes here.
#[test]
fn test_js_callee_name_long_chain_stores_near_linear_callee_bytes() {
    let depth = 200;
    let mut src = String::from("let p = program;\nexport function run() {\n  p");
    for _ in 0..depth {
        src.push_str(".option('--flag', 'x')");
    }
    src.push_str(";\n}\n");
    let calls = raw_calls(
        &JavaScriptProvider::new().unwrap(),
        "src/chain.js",
        &src,
        "run",
    );
    let total: usize = calls.iter().map(String::len).sum();
    assert!(
        calls.len() >= depth && total < depth * depth * 16,
        "{} callees, {total} bytes",
        calls.len()
    );
    for call in &calls {
        if let site @ CallSite::ChainedMember { .. } = CallSite::parse(call) {
            assert_eq!(
                site.fallback(),
                CallSite::UntypedMember("option"),
                "{call:?}"
            );
        }
    }
}

#[test]
fn test_js_callee_name_new_receiver_chain_fallback_is_untyped_member() {
    let calls = raw_calls(
        &JavaScriptProvider::new().unwrap(),
        JS_USE.0,
        JS_USE.1,
        "useNew",
    );
    let site = calls
        .iter()
        .map(|c| CallSite::parse(c))
        .find(|s| matches!(s, CallSite::ChainedMember { .. }))
        .unwrap_or_else(|| panic!("no chained entry: {calls:?}"));
    assert_eq!(site.fallback(), CallSite::UntypedMember("bar"), "{calls:?}");
}

// ---- chain on a local closure (Go, Python, Swift) ----

/// `caller` never binds `Alpha.bar`, the module function's return type.
fn assert_no_alpha_bar(graph: &ZeroCopyGraph, caller: &str) {
    assert!(
        !owners(graph, caller, "bar").contains(&"Alpha".to_string()),
        "{caller}: a local closure is no module function; all: {:?}",
        targets(graph, caller)
    );
}

/// `caller`'s `make().bar()` binds `Alpha.bar` through the module function.
fn assert_binds_alpha(graph: &ZeroCopyGraph, caller: &str) {
    assert_eq!(
        owners(graph, caller, "bar"),
        ["Alpha"],
        "{caller}: all: {:?}",
        targets(graph, caller)
    );
}

fn go_closure_graph() -> ZeroCopyGraph {
    single(
        &GoProvider::new().unwrap(),
        &[(
            "app/closure.go",
            "package app\n\ntype Alpha struct{}\n\nfunc (a *Alpha) bar() {}\n\ntype Beta struct{}\n\nfunc (b *Beta) bar() {}\n\nfunc produce() Alpha { return Alpha{} }\n\nfunc useLocal() {\n\tproduce := func() Beta { return Beta{} }\n\tproduce().bar()\n}\n\nfunc useParam(produce func() Beta) {\n\tproduce().bar()\n}\n\nfunc useModule() {\n\tproduce().bar()\n}\n",
        )],
    )
}

#[test]
fn test_resolve_call_go_local_closure_chain_skips_module_function_type() {
    assert_no_alpha_bar(&go_closure_graph(), "useLocal");
}

#[test]
fn test_resolve_call_go_closure_param_chain_skips_module_function_type() {
    assert_no_alpha_bar(&go_closure_graph(), "useParam");
}

#[test]
fn test_resolve_call_go_module_function_chain_binds_return_type_member() {
    assert_binds_alpha(&go_closure_graph(), "useModule");
}

fn py_closure_graph() -> ZeroCopyGraph {
    single(
        &PythonProvider::new().unwrap(),
        &[(
            "closure.py",
            "class Alpha:\n    def bar(self):\n        pass\n\n\nclass Beta:\n    def bar(self):\n        pass\n\n\ndef make() -> Alpha:\n    return Alpha()\n\n\ndef use_local():\n    make = lambda: Beta()\n    make().bar()\n\n\ndef use_param(make):\n    make().bar()\n\n\ndef use_module():\n    make().bar()\n",
        )],
    )
}

#[test]
fn test_resolve_call_python_local_lambda_chain_skips_module_function_type() {
    assert_no_alpha_bar(&py_closure_graph(), "use_local");
}

#[test]
fn test_resolve_call_python_closure_param_chain_skips_module_function_type() {
    assert_no_alpha_bar(&py_closure_graph(), "use_param");
}

#[test]
fn test_resolve_call_python_module_function_chain_binds_return_type_member() {
    assert_binds_alpha(&py_closure_graph(), "use_module");
}

fn swift_closure_graph() -> ZeroCopyGraph {
    single(
        &SwiftProvider::new().unwrap(),
        &[(
            "Sources/Closure.swift",
            "class Alpha {\n    func bar() {}\n}\n\nclass Beta {\n    func bar() {}\n}\n\nfunc make() -> Alpha { return Alpha() }\n\nfunc useLocal() {\n    let make = { Beta() }\n    make().bar()\n}\n\nfunc useParam(make: () -> Beta) {\n    make().bar()\n}\n\nfunc useModule() {\n    make().bar()\n}\n",
        )],
    )
}

#[test]
fn test_resolve_call_swift_local_closure_chain_skips_module_function_type() {
    assert_no_alpha_bar(&swift_closure_graph(), "useLocal");
}

#[test]
fn test_resolve_call_swift_closure_param_chain_skips_module_function_type() {
    assert_no_alpha_bar(&swift_closure_graph(), "useParam");
}

#[test]
fn test_resolve_call_swift_module_function_chain_binds_return_type_member() {
    assert_binds_alpha(&swift_closure_graph(), "useModule");
}

fn typescript_closure_graph() -> ZeroCopyGraph {
    single(
        &TypeScriptProvider::new().unwrap(),
        &[(
            "src/closure.ts",
            "class Alpha { bar(): void {} }\n\nclass Beta { bar(): void {} }\n\nfunction make(): Alpha { return new Alpha(); }\n\nfunction useLocal(): void {\n    const make = (): Beta => new Beta();\n    make().bar();\n}\n\nfunction useParam(make: () => Beta): void {\n    make().bar();\n}\n\nfunction useModule(): void {\n    make().bar();\n}\n",
        )],
    )
}

#[test]
fn test_resolve_call_typescript_local_closure_chain_skips_module_function_type() {
    assert_no_alpha_bar(&typescript_closure_graph(), "useLocal");
}

#[test]
fn test_resolve_call_typescript_closure_param_chain_skips_module_function_type() {
    assert_no_alpha_bar(&typescript_closure_graph(), "useParam");
}

#[test]
fn test_resolve_call_typescript_module_function_chain_binds_return_type_member() {
    assert_binds_alpha(&typescript_closure_graph(), "useModule");
}

fn rust_closure_graph() -> ZeroCopyGraph {
    single(
        &RustProvider::new().unwrap(),
        &[(
            "src/closure.rs",
            r#"
struct Alpha;
impl Alpha { fn bar(&self) {} }
struct Beta;
impl Beta { fn bar(&self) {} }
fn make() -> Alpha { Alpha }
fn use_local() {
    let make = || Beta;
    make().bar();
}
fn use_param(make: fn() -> Beta) {
    make().bar();
}
fn use_module() {
    make().bar();
}
fn use_before_binding() {
    make().bar();
    let make = || Beta;
}
fn use_after_block() {
    { let make = || Beta; }
    make().bar();
}
fn use_closure_param() {
    let invoke = |make: fn() -> Beta| { make().bar(); };
}
"#,
        )],
    )
}

#[test]
fn test_resolve_call_rust_local_closure_chain_skips_module_function_type() {
    assert_no_alpha_bar(&rust_closure_graph(), "use_local");
}

#[test]
fn test_resolve_call_rust_closure_param_chain_skips_module_function_type() {
    assert_no_alpha_bar(&rust_closure_graph(), "use_param");
}

#[test]
fn test_resolve_call_rust_module_function_chain_binds_return_type_member() {
    assert_binds_alpha(&rust_closure_graph(), "use_module");
}

#[test]
fn test_resolve_call_rust_call_before_local_binding_binds_module_function_type() {
    assert_binds_alpha(&rust_closure_graph(), "use_before_binding");
}

#[test]
fn test_resolve_call_rust_call_after_local_block_binds_module_function_type() {
    assert_binds_alpha(&rust_closure_graph(), "use_after_block");
}

#[test]
fn test_resolve_call_rust_closure_parameter_skips_module_function_type() {
    assert_no_alpha_bar(&rust_closure_graph(), "use_closure_param");
}

#[test]
fn test_chained_call_cut_receiver_namesake_does_not_type_outer_member() {
    let provider = TypeScriptProvider::new().unwrap();
    for depth in [2, CHAIN_DEPTH_LIMIT as usize + 3] {
        let source = format!(
            "class Cmd {{ option(): Cmd {{ return this; }} run() {{}} }}\n\
             class Logger {{ option(): Logger {{ return this; }} run() {{}} }}\n\
             function option(): Logger {{ return new Logger(); }}\n\
             function caller(p: Cmd) {{ p{}.run(); }}\n",
            ".option()".repeat(depth)
        );
        let g = single(&provider, &[("a.ts", &source)]);
        let expected = if depth == 2 { vec!["Cmd"] } else { vec![] };
        assert_eq!(owners(&g, "caller", "run"), expected, "depth {depth}");
    }
}

/// In every language that encodes chains, an entry of a long chain nests
/// exactly `CHAIN_DEPTH_LIMIT` receiver calls and then the cut root, so the
/// entries grow linearly with the chain.
#[test]
fn test_chained_call_long_chain_nests_to_depth_limit_in_every_language() {
    type Provider = fn() -> Box<dyn LanguageProvider>;
    let depth = 300;
    let chain = |link: &str| link.repeat(depth);
    let cases: Vec<(Provider, &str, String, &str)> = vec![
        (
            || Box::new(JavaScriptProvider::new().unwrap()),
            "a.js",
            format!("function run() {{ p{}; }}\n", chain(".option('x')")),
            "run",
        ),
        (
            || Box::new(TypeScriptProvider::new().unwrap()),
            "a.ts",
            format!("function run() {{ p{}; }}\n", chain(".option('x')")),
            "run",
        ),
        (
            || Box::new(JavaProvider::new().unwrap()),
            "A.java",
            format!("class A {{ void run() {{ p{}; }} }}\n", chain(".option(1)")),
            "run",
        ),
        (
            || Box::new(CSharpProvider::new().unwrap()),
            "A.cs",
            format!("class A {{ void Run() {{ p{}; }} }}\n", chain(".Option(1)")),
            "Run",
        ),
        (
            || Box::new(KotlinProvider::new().unwrap()),
            "a.kt",
            format!("fun run() {{ p{} }}\n", chain(".option(1)")),
            "run",
        ),
        (
            || Box::new(SwiftProvider::new().unwrap()),
            "a.swift",
            format!("func run() {{ p{} }}\n", chain(".option(1)")),
            "run",
        ),
        (
            || Box::new(DartProvider::new().unwrap()),
            "a.dart",
            format!("void run() {{ p{}; }}\n", chain(".option(1)")),
            "run",
        ),
        (
            || Box::new(GoProvider::new().unwrap()),
            "a.go",
            format!("package a\nfunc run() {{ p{} }}\n", chain(".Option(1)")),
            "run",
        ),
        (
            || Box::new(RustProvider::new().unwrap()),
            "a.rs",
            format!("fn run() {{ p{}; }}\n", chain(".option(1)")),
            "run",
        ),
        (
            || Box::new(PythonProvider::new().unwrap()),
            "a.py",
            format!("def run():\n    p{}\n", chain(".option(1)")),
            "run",
        ),
        (
            || Box::new(PhpProvider::new().unwrap()),
            "a.php",
            format!("<?php\nfunction run() {{ $p{}; }}\n", chain("->option(1)")),
            "run",
        ),
        (
            || Box::new(RubyProvider::new().unwrap()),
            "a.rb",
            format!("def run\n  p{}\nend\n", chain(".option(1)")),
            "run",
        ),
        (
            || Box::new(CppProvider::new().unwrap()),
            "a.cpp",
            format!("void run() {{ p{}; }}\n", chain(".option(1)")),
            "run",
        ),
    ];
    for (provider, path, src, caller) in cases {
        let calls = raw_calls(provider().as_ref(), path, &src, caller);
        let deepest = calls
            .iter()
            .map(|raw| {
                let mut site = CallSite::parse(raw);
                let mut depth = 0;
                while let CallSite::ChainedMember { inner, .. } = site {
                    depth += 1;
                    site = CallSite::parse(inner);
                }
                (depth, site)
            })
            .max_by_key(|&(depth, _)| depth);
        assert!(calls.len() >= depth, "{path}: {} callees", calls.len());
        assert_eq!(
            deepest,
            Some((CHAIN_DEPTH_LIMIT + 1, CallSite::Plain(CallSite::CUT_CHAIN))),
            "{path}"
        );
    }
}
