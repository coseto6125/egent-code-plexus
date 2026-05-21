//! C uses a receiver-convention model: `void op(struct T *self, ...)` is
//! treated as a method on `T`. The spec's OQ-2 vtable case
//! (`static struct foo_ops = { .open = my_open }`) is an enhancement not yet
//! plumbed through the C parser; the test below covers the implemented
//! receiver-convention path.

use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = CProvider::new().expect("CProvider::new");
    provider
        .parse_file(Path::new("test.c"), source.as_bytes())
        .expect("parse_file")
}

fn owner_of(g: &LocalGraph, name: &str) -> Option<String> {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .and_then(|n| n.owner_class.clone())
}

#[test]
fn receiver_convention_method_gets_owner() {
    // `void calc_add(struct Calc *self, int a)` — `self` first-param identifies
    // this as a receiver-convention method on Calc.
    let src = "\
struct Calc { int val; };\n\
void calc_add(struct Calc *self, int a) { self->val += a; }\n";
    let g = parse(src);
    let oc = owner_of(&g, "calc_add");
    assert_eq!(
        oc.as_deref(),
        Some("Calc"),
        "calc_add must own Calc via receiver convention; got {oc:?}"
    );
}

#[test]
fn free_function_no_self_param_has_no_owner() {
    let src = "void free_fn(int x) { return; }\n";
    let g = parse(src);
    let oc = owner_of(&g, "free_fn");
    assert!(
        oc.is_none(),
        "free_fn has no receiver param, owner_class must be None; got {oc:?}"
    );
}
