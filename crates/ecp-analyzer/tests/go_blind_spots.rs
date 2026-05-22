use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_go(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = GoProvider::new().expect("GoProvider::new");
    provider
        .parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── reflect.Value.MethodByName — anchor for runtime method dispatch ──

#[test]
fn go_method_by_name_emits_blind_spot() {
    let src = r#"
package main

import "reflect"

func invoke(obj interface{}, name string, args []reflect.Value) {
    v := reflect.ValueOf(obj)
    m := v.MethodByName(name)
    _ = m.Call(args)
}
"#;
    let g = parse_go(src);
    assert!(
        kinds(&g).contains(&"go-reflect-method-by-name"),
        "expected go-reflect-method-by-name; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn go_method_by_name_chained_emits_blind_spot() {
    // The chained form `reflect.ValueOf(x).MethodByName(n).Call(args)`
    // — same anchor (MethodByName) regardless of chain depth.
    let src = r#"
package main

import "reflect"

func chain(x interface{}, n string) {
    _ = reflect.ValueOf(x).MethodByName(n).Call(nil)
}
"#;
    let g = parse_go(src);
    assert!(
        kinds(&g).contains(&"go-reflect-method-by-name"),
        "expected go-reflect-method-by-name on chain; got: {:?}",
        kinds(&g)
    );
}

// ── plugin.Open — dynamic library load ──

#[test]
fn go_plugin_open_emits_blind_spot() {
    let src = r#"
package main

import "plugin"

func load(path string) {
    p, _ := plugin.Open(path)
    _, _ = p.Lookup("Hook")
}
"#;
    let g = parse_go(src);
    assert!(
        kinds(&g).contains(&"go-plugin-open"),
        "expected go-plugin-open; got: {:?}",
        kinds(&g)
    );
}

// ── unrelated patterns: NOT blind ──

#[test]
fn go_interface_method_call_emits_no_blind_spot() {
    // Interface method dispatch is graph-traversable (Implements edges) —
    // belongs to CallMeta / verdict path, NOT BlindSpot.
    let src = r#"
package main

type Handler interface{ Handle() }

func run(h Handler) {
    h.Handle()
}
"#;
    let g = parse_go(src);
    assert!(
        g.blind_spots.is_empty(),
        "interface dispatch must not emit BlindSpot; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn go_unrelated_method_named_call_does_not_match_method_by_name() {
    // `client.Call(...)` in an RPC library is NOT MethodByName — the
    // narrow MethodByName-only rule must skip it.
    let src = r#"
package main

type RpcClient struct{}
func (c *RpcClient) Call(method string) {}

func main() {
    c := &RpcClient{}
    c.Call("Service.Method")
}
"#;
    let g = parse_go(src);
    assert!(
        !kinds(&g).contains(&"go-reflect-method-by-name"),
        ".Call(...) must NOT match the MethodByName anchor; got: {:?}",
        kinds(&g)
    );
}

// ── span shape ──

#[test]
fn go_method_by_name_span_covers_full_call() {
    let src =
        "package main\nimport \"reflect\"\nfunc x() { _ = reflect.ValueOf(1).MethodByName(\"X\") }";
    let g = parse_go(src);
    let bs = g
        .blind_spots
        .iter()
        .find(|b| b.kind == "go-reflect-method-by-name")
        .expect("go-reflect-method-by-name BlindSpot");
    let (sr, _, er, _) = bs.span;
    assert_eq!(sr, er, "single-line MethodByName span must stay on one row");
}
