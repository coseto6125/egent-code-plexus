//! A call whose name several declarations of one file share binds by owner,
//! never by source position (`SymbolTable::prefer_owned`). Each fixture
//! declares `A`'s `save` first, so a pick by position lands on `A`:
//!
//! - heritage: `C` extends `B`, and `run` in `C` calls `save` → `B.save`.
//! - unrelated owners: `A` and `B` both declare `save`, and `run` belongs to
//!   neither → no edge. The global tier also sees two candidates, so no
//!   later tier binds one either.
//! - free function: a free `save` follows `A.save` → the free one.
//! - one hierarchy: an interface or trait and its implementations, the
//!   implementations first → the interface or trait declaration, which a
//!   call through any of them reaches.
//! - one owner: only `A` declares `save` → `A.save`, when the call syntax
//!   permits a member. TS / JS / Go use an explicit receiver.
//!
//! Skipped cases: C has no members, so it has only the free-function and
//! one-owner fixtures. Go and Rust declare methods outside the type body,
//! so a caller carries no enclosing-class heritage; they have no heritage
//! fixture. Java and C# have no free functions.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

fn build(files: &[(&dyn LanguageProvider, &str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for &(provider, path, source) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

/// `(owner_class, file)` of each `save` node that `run` calls.
fn saves_called_by_run(graph: &ZeroCopyGraph) -> Vec<(String, String)> {
    let pool = &graph.string_pool;
    let name = |i: u32| graph.nodes[i as usize].name.resolve(pool);
    let runs: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| name(i) == "run")
        .collect();
    assert_eq!(runs.len(), 1, "one `run` caller");
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == runs[0])
        .map(|e| e.target)
        .filter(|&t| name(t) == "save")
        .map(|t| {
            let node = &graph.nodes[t as usize];
            (
                node.owner_class.resolve(pool).to_string(),
                graph.files[node.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string(),
            )
        })
        .collect()
}

/// Every `save` that `run` calls is owned by `owner` (`""` for a free
/// function), and there is at least one.
fn assert_run_binds(provider: &dyn LanguageProvider, path: &str, source: &str, owner: &str) {
    let targets = saves_called_by_run(&build(&[(provider, path, source)]));
    assert!(!targets.is_empty(), "{path}: `run` calls `save`");
    for (target_owner, _) in targets {
        assert_eq!(
            target_owner, owner,
            "{path}: `run` binds {owner:?}'s `save`"
        );
    }
}

fn assert_run_binds_no_save(provider: &dyn LanguageProvider, path: &str, source: &str) {
    let targets = saves_called_by_run(&build(&[(provider, path, source)]));
    assert!(
        targets.is_empty(),
        "{path}: no matching receiver, so no `save` edge; got {targets:?}"
    );
}

fn typescript() -> ecp_analyzer::typescript::TypeScriptProvider {
    ecp_analyzer::typescript::TypeScriptProvider::new().unwrap()
}
fn javascript() -> ecp_analyzer::javascript::parser::JavaScriptProvider {
    ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap()
}
fn python() -> ecp_analyzer::python::PythonProvider {
    ecp_analyzer::python::PythonProvider::new().unwrap()
}
fn java() -> ecp_analyzer::java::parser::JavaProvider {
    ecp_analyzer::java::parser::JavaProvider::new().unwrap()
}
fn kotlin() -> ecp_analyzer::kotlin::parser::KotlinProvider {
    ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap()
}
fn csharp() -> ecp_analyzer::c_sharp::parser::CSharpProvider {
    ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap()
}
fn go() -> ecp_analyzer::go::parser::GoProvider {
    ecp_analyzer::go::parser::GoProvider::new().unwrap()
}
fn rust() -> ecp_analyzer::rust::parser::RustProvider {
    ecp_analyzer::rust::parser::RustProvider::new().unwrap()
}
fn php() -> ecp_analyzer::php::parser::PhpProvider {
    ecp_analyzer::php::parser::PhpProvider::new().unwrap()
}
fn ruby() -> ecp_analyzer::ruby::parser::RubyProvider {
    ecp_analyzer::ruby::parser::RubyProvider::new().unwrap()
}
fn swift() -> ecp_analyzer::swift::parser::SwiftProvider {
    ecp_analyzer::swift::parser::SwiftProvider::new().unwrap()
}
fn c() -> ecp_analyzer::c::parser::CProvider {
    ecp_analyzer::c::parser::CProvider::new().unwrap()
}
fn cpp() -> ecp_analyzer::cpp::parser::CppProvider {
    ecp_analyzer::cpp::parser::CppProvider::new().unwrap()
}
fn dart() -> ecp_analyzer::dart::parser::DartProvider {
    ecp_analyzer::dart::parser::DartProvider::new().unwrap()
}

// ---- heritage: `run` in `C extends B` binds `B.save` ----

/// `this.save()` reaches the qualifier tier typed as `C.save`.
#[test]
fn test_resolve_call_typescript_base_member_binds_base_save() {
    assert_run_binds(
        &typescript(),
        "h.ts",
        "class A { save() {} }\nclass B<T> { save() {} }\nclass C extends B<string> { run() { this.save(); } }\n",
        "B",
    );
}

#[test]
fn test_resolve_call_javascript_base_member_binds_base_save() {
    assert_run_binds(
        &javascript(),
        "h.js",
        "class A { save() {} }\nclass B { save() {} }\nclass C extends B { run() { this.save(); } }\n",
        "B",
    );
}

#[test]
fn test_resolve_call_python_base_member_binds_base_save() {
    assert_run_binds(
        &python(),
        "h.py",
        "class A:\n    def save(self):\n        pass\nclass B:\n    def save(self):\n        pass\nclass C(B):\n    def run(self):\n        self.save()\n",
        "B",
    );
}

/// The heritage entry is `B<String>`; the owner index keys `B`.
#[test]
fn test_resolve_call_java_generic_base_member_binds_base_save() {
    assert_run_binds(
        &java(),
        "H.java",
        "class A {\n  void save() {}\n}\nclass B<T> {\n  void save() {}\n}\nclass C extends B<String> {\n  void run() {\n    save();\n  }\n}\n",
        "B",
    );
}

/// `C`'s file declares `A.save` and `X.save` but not `B`: Tier 1 binds
/// neither, and the heritage tier finds `B.save` in `B.java`.
#[test]
fn test_resolve_call_java_base_in_other_file_binds_base_file_save() {
    let provider = java();
    let java: &dyn LanguageProvider = &provider;
    let graph = build(&[
        (java, "B.java", "class B {\n  void save() {}\n}\n"),
        (
            java,
            "C.java",
            "class A {\n  void save() {}\n}\nclass X {\n  void save() {}\n}\nclass C extends B {\n  void run() {\n    save();\n  }\n}\n",
        ),
    ]);
    assert_eq!(
        saves_called_by_run(&graph),
        vec![("B".to_string(), "B.java".to_string())]
    );
}

#[test]
fn test_resolve_call_kotlin_base_member_binds_base_save() {
    assert_run_binds(
        &kotlin(),
        "H.kt",
        "open class A { fun save() {} }\nopen class B { fun save() {} }\nclass C : B() {\n  fun run() { save() }\n}\n",
        "B",
    );
}

#[test]
fn test_resolve_call_csharp_base_member_binds_base_save() {
    assert_run_binds(
        &csharp(),
        "H.cs",
        "class A { public void save() {} }\nclass B { public void save() {} }\nclass C : B {\n  void run() { save(); }\n}\n",
        "B",
    );
}

#[test]
fn test_resolve_call_php_base_member_binds_base_save() {
    assert_run_binds(
        &php(),
        "h.php",
        "<?php\nclass A { function save() {} }\nclass B { function save() {} }\nclass C extends B {\n  function run() { $this->save(); }\n}\n",
        "B",
    );
}

/// Bare `save()` goes through Tier 1, `self.save` through the qualifier
/// tier; both bind `B.save`.
#[test]
fn test_resolve_call_ruby_base_member_binds_base_save() {
    assert_run_binds(
        &ruby(),
        "h.rb",
        "class A\n  def save\n  end\nend\nclass B\n  def save\n  end\nend\nclass C < B\n  def run\n    save()\n    self.save\n  end\nend\n",
        "B",
    );
}

#[test]
fn test_resolve_call_swift_base_member_binds_base_save() {
    assert_run_binds(
        &swift(),
        "H.swift",
        "class A { func save() {} }\nclass B { func save() {} }\nclass C: B {\n  func run() { save() }\n}\n",
        "B",
    );
}

#[test]
fn test_resolve_call_cpp_base_member_binds_base_save() {
    assert_run_binds(
        &cpp(),
        "h.cpp",
        "class A { public: void save() {} };\nclass B { public: void save() {} };\nclass C : public B {\n public:\n  void run() { save(); }\n};\n",
        "B",
    );
}

#[test]
fn test_resolve_call_dart_base_member_binds_base_save() {
    assert_run_binds(
        &dart(),
        "h.dart",
        "class A { void save() {} }\nclass B { void save() {} }\nclass C extends B {\n  void run() { save(); }\n}\n",
        "B",
    );
}

// ---- unrelated owners: `run` belongs to neither → no edge ----

#[test]
fn test_resolve_call_typescript_two_unrelated_owners_binds_nothing() {
    assert_run_binds_no_save(
        &typescript(),
        "x.ts",
        "class A { save() {} }\nclass B { save() {} }\nfunction run() { save(); }\n",
    );
}

#[test]
fn test_resolve_call_javascript_two_unrelated_owners_binds_nothing() {
    assert_run_binds_no_save(
        &javascript(),
        "x.js",
        "class A { save() {} }\nclass B { save() {} }\nfunction run() { save(); }\n",
    );
}

#[test]
fn test_resolve_call_python_untyped_receiver_two_owners_binds_nothing() {
    assert_run_binds_no_save(
        &python(),
        "x.py",
        "class A:\n    def save(self):\n        pass\nclass B:\n    def save(self):\n        pass\ndef run(o):\n    o.save()\n",
    );
}

/// `next().save()` is a computed receiver, which reaches Tier 1 untyped.
#[test]
fn test_resolve_call_java_computed_receiver_two_owners_binds_nothing() {
    assert_run_binds_no_save(
        &java(),
        "X.java",
        "class A {\n  void save() {}\n}\nclass B {\n  void save() {}\n}\nclass D {\n  Object next() {\n    return null;\n  }\n  void run() {\n    next().save();\n    save();\n  }\n}\n",
    );
}

#[test]
fn test_resolve_call_kotlin_two_unrelated_owners_binds_nothing() {
    assert_run_binds_no_save(
        &kotlin(),
        "X.kt",
        "class A { fun save() {} }\nclass B { fun save() {} }\nfun run() { save() }\n",
    );
}

#[test]
fn test_resolve_call_csharp_two_unrelated_owners_binds_nothing() {
    assert_run_binds_no_save(
        &csharp(),
        "X.cs",
        "class A { public void save() {} }\nclass B { public void save() {} }\nclass D {\n  void run() { save(); }\n}\n",
    );
}

#[test]
fn test_resolve_call_go_two_receiver_types_binds_nothing() {
    assert_run_binds_no_save(
        &go(),
        "x.go",
        "package demo\ntype A struct{}\nfunc (a *A) save() {}\ntype B struct{}\nfunc (b *B) save() {}\nfunc run() { save() }\n",
    );
}

#[test]
fn test_resolve_call_rust_untyped_receiver_two_owners_binds_nothing() {
    assert_run_binds_no_save(
        &rust(),
        "x.rs",
        "struct A;\nimpl A { fn save(&self) {} }\nstruct B;\nimpl B { fn save(&self) {} }\nfn run(o: &Holder) { o.inner.save(); }\n",
    );
}

#[test]
fn test_resolve_call_php_untyped_receiver_two_owners_binds_nothing() {
    assert_run_binds_no_save(
        &php(),
        "x.php",
        "<?php\nclass A { function save() {} }\nclass B { function save() {} }\nfunction run($o) { $o->save(); }\n",
    );
}

#[test]
fn test_resolve_call_ruby_two_unrelated_owners_binds_nothing() {
    assert_run_binds_no_save(
        &ruby(),
        "x.rb",
        "class A\n  def save\n  end\nend\nclass B\n  def save\n  end\nend\nclass D\n  def run\n    save()\n  end\nend\n",
    );
}

#[test]
fn test_resolve_call_swift_two_unrelated_owners_binds_nothing() {
    assert_run_binds_no_save(
        &swift(),
        "X.swift",
        "class A { func save() {} }\nclass B { func save() {} }\nfunc run() { save() }\n",
    );
}

#[test]
fn test_resolve_call_cpp_two_unrelated_owners_binds_nothing() {
    assert_run_binds_no_save(
        &cpp(),
        "x.cpp",
        "class A { public: void save() {} };\nclass B { public: void save() {} };\nvoid run() { save(); }\n",
    );
}

#[test]
fn test_resolve_call_dart_two_unrelated_owners_binds_nothing() {
    assert_run_binds_no_save(
        &dart(),
        "x.dart",
        "class A { void save() {} }\nclass B { void save() {} }\nvoid run() { save(); }\n",
    );
}

// ---- free function: a free `save` after `A.save` wins ----

#[test]
fn test_resolve_call_typescript_free_fn_after_member_binds_free_fn() {
    assert_run_binds(
        &typescript(),
        "f.ts",
        "class A { save() {} }\nfunction save() {}\nfunction run() { save(); }\n",
        "",
    );
}

#[test]
fn test_resolve_call_javascript_free_fn_after_member_binds_free_fn() {
    assert_run_binds(
        &javascript(),
        "f.js",
        "class A { save() {} }\nfunction save() {}\nfunction run() { save(); }\n",
        "",
    );
}

/// The caller's own type `D` has no `save`, so the module function wins.
#[test]
fn test_resolve_call_python_free_fn_after_member_binds_free_fn() {
    assert_run_binds(
        &python(),
        "f.py",
        "class A:\n    def save(self):\n        pass\ndef save():\n    pass\nclass D:\n    def run(self):\n        save()\n",
        "",
    );
}

#[test]
fn test_resolve_call_kotlin_free_fn_after_member_binds_free_fn() {
    assert_run_binds(
        &kotlin(),
        "F.kt",
        "class A { fun save() {} }\nfun save() {}\nfun run() { save() }\n",
        "",
    );
}

#[test]
fn test_resolve_call_go_free_fn_after_method_binds_free_fn() {
    assert_run_binds(
        &go(),
        "f.go",
        "package demo\ntype A struct{}\nfunc (a *A) save() {}\nfunc save() {}\nfunc run() { save() }\n",
        "",
    );
}

#[test]
fn test_resolve_call_rust_free_fn_after_method_binds_free_fn() {
    assert_run_binds(
        &rust(),
        "f.rs",
        "struct A;\nimpl A { fn save(&self) {} }\nfn save() {}\nfn run() { save(); }\n",
        "",
    );
}

#[test]
fn test_resolve_call_php_free_fn_after_method_binds_free_fn() {
    assert_run_binds(
        &php(),
        "f.php",
        "<?php\nclass A { function save() {} }\nfunction save() {}\nfunction run() { save(); }\n",
        "",
    );
}

#[test]
fn test_resolve_call_ruby_top_level_def_after_method_binds_top_level_def() {
    assert_run_binds(
        &ruby(),
        "f.rb",
        "class A\n  def save\n  end\nend\ndef save\nend\ndef run\n  save()\nend\n",
        "",
    );
}

#[test]
fn test_resolve_call_swift_free_fn_after_method_binds_free_fn() {
    assert_run_binds(
        &swift(),
        "F.swift",
        "class A { func save() {} }\nfunc save() {}\nfunc run() { save() }\n",
        "",
    );
}

/// C has no members: the free function is the only candidate.
#[test]
fn test_resolve_call_c_free_fn_binds_free_fn() {
    assert_run_binds(
        &c(),
        "f.c",
        "void save(void) {}\nvoid run(void) { save(); }\n",
        "",
    );
}

#[test]
fn test_resolve_call_cpp_free_fn_after_method_binds_free_fn() {
    assert_run_binds(
        &cpp(),
        "f.cpp",
        "class A { public: void save() {} };\nvoid save() {}\nvoid run() { save(); }\n",
        "",
    );
}

#[test]
fn test_resolve_call_dart_free_fn_after_method_binds_free_fn() {
    assert_run_binds(
        &dart(),
        "f.dart",
        "class A { void save() {} }\nvoid save() {}\nvoid run() { save(); }\n",
        "",
    );
}

// ---- one hierarchy: the interface or trait declaration ----

/// `h: &dyn Handler` leaves the receiver untyped; the trait-object call
/// keeps its edge (and with it the dynamic-dispatch `CallMeta`).
#[test]
fn test_resolve_call_rust_trait_object_impls_first_binds_trait_declaration() {
    assert_run_binds(
        &rust(),
        "src/lib.rs",
        "pub struct Foo;\nimpl Handler for Foo { fn save(&self) {} }\npub struct Bar;\nimpl Handler for Bar { fn save(&self) {} }\npub trait Handler { fn save(&self); }\npub fn run(h: &dyn Handler) { h.save(); }\n",
        "Handler",
    );
}

#[test]
fn test_resolve_call_java_interface_impls_first_binds_interface_declaration() {
    assert_run_binds(
        &java(),
        "J.java",
        "class A implements H {\n  public void save() {}\n}\nclass B implements H {\n  public void save() {}\n}\ninterface H {\n  void save();\n}\nclass D {\n  H next() {\n    return null;\n  }\n  void run() {\n    next().save();\n  }\n}\n",
        "H",
    );
}

// ---- one owner: the binding stays ----

#[test]
fn test_resolve_call_typescript_one_owner_binds_its_save() {
    assert_run_binds(
        &typescript(),
        "u.ts",
        "class A { save() {} }\nfunction run(o: any) { o.save(); }\n",
        "A",
    );
}

#[test]
fn test_resolve_call_javascript_one_owner_binds_its_save() {
    assert_run_binds(
        &javascript(),
        "u.js",
        "class A { save() {} }\nfunction run(o) { o.save(); }\n",
        "A",
    );
}

#[test]
fn test_resolve_call_python_one_owner_binds_its_save() {
    assert_run_binds(
        &python(),
        "u.py",
        "class A:\n    def save(self):\n        pass\ndef run(o):\n    o.save()\n",
        "A",
    );
}

#[test]
fn test_resolve_call_java_one_owner_binds_its_save() {
    assert_run_binds(
        &java(),
        "U.java",
        "class A {\n  void save() {}\n}\nclass D {\n  A next() {\n    return null;\n  }\n  void run() {\n    next().save();\n  }\n}\n",
        "A",
    );
}

#[test]
fn test_resolve_call_java_object_return_binds_no_unrelated_owner() {
    assert_run_binds_no_save(
        &java(),
        "U.java",
        "class A {\n  void save() {}\n}\nclass D {\n  Object next() {\n    return null;\n  }\n  void run() {\n    next().save();\n  }\n}\n",
    );
}

#[test]
fn test_resolve_call_java_object_return_binds_no_unrelated_interface() {
    assert_run_binds_no_save(
        &java(),
        "J.java",
        "class A implements H {\n  public void save() {}\n}\nclass B implements H {\n  public void save() {}\n}\ninterface H {\n  void save();\n}\nclass D {\n  Object next() {\n    return null;\n  }\n  void run() {\n    next().save();\n  }\n}\n",
    );
}

#[test]
fn test_resolve_call_kotlin_one_owner_binds_its_save() {
    assert_run_binds(
        &kotlin(),
        "U.kt",
        "class A { fun save() {} }\nfun run() { save() }\n",
        "A",
    );
}

#[test]
fn test_resolve_call_csharp_one_owner_binds_its_save() {
    assert_run_binds(
        &csharp(),
        "U.cs",
        "class A { public void save() {} }\nclass D {\n  void run() { save(); }\n}\n",
        "A",
    );
}

#[test]
fn test_resolve_call_go_one_receiver_type_binds_its_save() {
    assert_run_binds(
        &go(),
        "u.go",
        "package demo\ntype A struct{}\nfunc (a *A) save() {}\nfunc run(o interface{}) { o.save() }\n",
        "A",
    );
}

/// `A` spans an inherent and a trait `impl` block: one owner.
#[test]
fn test_resolve_call_rust_one_owner_in_two_impl_blocks_binds_its_save() {
    assert_run_binds(
        &rust(),
        "u.rs",
        "struct A;\nimpl A { fn save(&self) {} }\nimpl Store for A { fn save(&self) {} }\nfn run(o: &Holder) { o.inner.save(); }\n",
        "A",
    );
}

#[test]
fn test_resolve_call_php_one_owner_binds_its_save() {
    assert_run_binds(
        &php(),
        "u.php",
        "<?php\nclass A { function save() {} }\nfunction run($o) { $o->save(); }\n",
        "A",
    );
}

#[test]
fn test_resolve_call_ruby_one_owner_binds_its_save() {
    assert_run_binds(
        &ruby(),
        "u.rb",
        "class A\n  def save\n  end\nend\nclass D\n  def run\n    save()\n  end\nend\n",
        "A",
    );
}

#[test]
fn test_resolve_call_swift_one_owner_binds_its_save() {
    assert_run_binds(
        &swift(),
        "U.swift",
        "class A { func save() {} }\nfunc run() { save() }\n",
        "A",
    );
}

/// C has no members: the one free function stays bound.
#[test]
fn test_resolve_call_c_one_free_fn_binds_it() {
    assert_run_binds(
        &c(),
        "u.c",
        "void save(void) {}\nvoid run(void) { save(); }\n",
        "",
    );
}

#[test]
fn test_resolve_call_cpp_one_owner_binds_its_save() {
    assert_run_binds(
        &cpp(),
        "u.cpp",
        "class A { public: void save() {} };\nvoid run() { save(); }\n",
        "A",
    );
}

#[test]
fn test_resolve_call_dart_one_owner_binds_its_save() {
    assert_run_binds(
        &dart(),
        "u.dart",
        "class A { void save() {} }\nvoid run() { save(); }\n",
        "A",
    );
}
