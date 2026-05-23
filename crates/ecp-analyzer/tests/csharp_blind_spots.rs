use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_cs(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    provider
        .parse_file(Path::new("Test.cs"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── Activator.CreateInstance: runtime type instantiation ──

#[test]
fn csharp_activator_create_instance_emits_blind_spot() {
    let src = r#"
using System;
class Loader {
    void Load(Type t) {
        object o = Activator.CreateInstance(t);
    }
}
"#;
    let g = parse_cs(src);
    assert!(
        kinds(&g).contains(&"cs-activator-create-instance"),
        "expected cs-activator-create-instance; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn csharp_activator_create_instance_with_type_name_emits_blind_spot() {
    let src = r#"
using System;
class Loader {
    void Load(string name) {
        Type t = Type.GetType(name);
        object o = Activator.CreateInstance(t);
    }
}
"#;
    let g = parse_cs(src);
    let ks = kinds(&g);
    assert!(
        ks.contains(&"cs-activator-create-instance"),
        "expected cs-activator-create-instance; got: {:?}",
        ks
    );
}

// ── MethodInfo.Invoke: reflective dispatch ──

#[test]
fn csharp_method_info_invoke_emits_blind_spot() {
    let src = r#"
using System.Reflection;
class Dispatcher {
    void Run(MethodInfo m, object target, object[] args) {
        m.Invoke(target, args);
    }
}
"#;
    let g = parse_cs(src);
    assert!(
        kinds(&g).contains(&"cs-method-invoke"),
        "expected cs-method-invoke; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn csharp_chained_reflective_call_emits_invoke_and_activator_if_present() {
    let src = r#"
using System;
using System.Reflection;
class Chain {
    void Run(Type t, object target) {
        t.GetMethod("Foo").Invoke(target, null);
        object o = Activator.CreateInstance(t);
    }
}
"#;
    let g = parse_cs(src);
    let ks = kinds(&g);
    assert!(
        ks.contains(&"cs-method-invoke"),
        "expected cs-method-invoke; got: {:?}",
        ks
    );
    assert!(
        ks.contains(&"cs-activator-create-instance"),
        "expected cs-activator-create-instance; got: {:?}",
        ks
    );
}

// ── unrelated: NOT blind ──

#[test]
fn csharp_interface_method_call_emits_no_blind_spot() {
    let src = r#"
interface IHandler { void Handle(); }
class App {
    void Run(IHandler h) { h.Handle(); }
}
"#;
    let g = parse_cs(src);
    assert!(
        g.blind_spots.is_empty(),
        "interface dispatch must not emit BlindSpot; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn csharp_ordinary_call_emits_no_blind_spot() {
    let src = r#"
class A {
    int Add(int a, int b) { return a + b; }
    void Main() { int x = Add(1, 2); }
}
"#;
    let g = parse_cs(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary call must not emit; got: {:?}",
        g.blind_spots
    );
}
