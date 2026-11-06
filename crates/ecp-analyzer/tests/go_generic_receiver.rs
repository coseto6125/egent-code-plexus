//! A Go method declared on a generic receiver (`func (w Wrap[T]) put`) belongs
//! to the type `Wrap`, and a local typed `Wrap[int]` binds that same type.
//!
//! Contract: `ecp impact --target put --direction upstream` lists the caller,
//! and `Wrap` owns its methods through `HasMethod`. The other 13 mainstream
//! languages are skipped: this change touches only the Go receiver and local
//! type binding, which no other language shares.

use crate::name_ref_support::{calls, function_refs, graph};
use ecp_analyzer::go::parser::GoProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};

const SRC: &str = r#"package p

type Wrap[T any] struct{ v T }

func (w Wrap[T]) put(x T) { w.v = x }
func (w *Wrap[T]) get() T { return w.v }

type Pair[K comparable, V any] struct {
	k K
	v V
}

func (p Pair[K, V]) key() K  { return p.k }
func (p *Pair[K, V]) val() V { return p.v }

type Plain struct{}

func (p Plain) take() {}

func useVar() {
	var w Wrap[int]
	w.put(1)
	_ = w.get()
}
func useShort() {
	w := Wrap[int]{}
	w.put(1)
}
func useAddr() {
	w := &Wrap[string]{}
	_ = w.get()
}
func useParam(w *Wrap[int]) { w.put(1) }
func usePair(p Pair[string, int]) {
	p.key()
	p.val()
}
func usePlain(p Plain) { p.take() }
"#;

fn built(src: &str) -> ZeroCopyGraph {
    graph(
        &GoProvider::new().expect("GoProvider init"),
        &[("w.go", src)],
    )
}

/// `(owner, method)` for every `HasMethod` edge, sorted.
fn owned(graph: &ZeroCopyGraph) -> Vec<(String, String)> {
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<(String, String)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::HasMethod)
        .map(|e| {
            (
                graph.nodes[e.source as usize]
                    .name
                    .resolve(pool)
                    .to_string(),
                graph.nodes[e.target as usize]
                    .name
                    .resolve(pool)
                    .to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

fn called(graph: &ZeroCopyGraph, caller: &str) -> Vec<String> {
    calls(graph, caller).into_iter().map(|(_, n)| n).collect()
}

fn pair(owner: &str, method: &str) -> (String, String) {
    (owner.to_string(), method.to_string())
}

// main: no HasMethod edge for any `Wrap` / `Pair` method; `Plain take` exists.
#[test]
fn test_has_method_go_generic_receiver_owned_by_base_type() {
    let owners = owned(&built(SRC));
    for expected in [
        pair("Wrap", "put"),
        pair("Wrap", "get"),
        pair("Pair", "key"),
        pair("Pair", "val"),
        pair("Plain", "take"),
    ] {
        assert!(
            owners.contains(&expected),
            "{expected:?} missing in {owners:?}"
        );
    }
    assert!(
        owners.iter().all(|(owner, _)| !owner.contains('[')),
        "no owner keeps its type arguments: {owners:?}"
    );
}

// main: `useVar` has no Calls edge to `put` or `get`.
#[test]
fn test_calls_go_generic_var_decl_binds_value_and_pointer_methods() {
    assert_eq!(called(&built(SRC), "useVar"), ["get", "put"]);
}

#[test]
fn test_calls_go_generic_short_var_composite_binds_method() {
    assert_eq!(called(&built(SRC), "useShort"), ["put"]);
}

#[test]
fn test_calls_go_generic_address_of_composite_binds_method() {
    assert_eq!(called(&built(SRC), "useAddr"), ["get"]);
}

#[test]
fn test_calls_go_generic_pointer_parameter_binds_method() {
    assert_eq!(called(&built(SRC), "useParam"), ["put"]);
}

#[test]
fn test_calls_go_generic_multi_param_type_binds_methods() {
    assert_eq!(called(&built(SRC), "usePair"), ["key", "val"]);
}

// Control: the non-generic path must keep working.
#[test]
fn test_calls_go_plain_receiver_still_binds_method() {
    assert_eq!(called(&built(SRC), "usePlain"), ["take"]);
}

// Edge: the receiver name is absent.
#[test]
fn test_has_method_go_generic_receiver_without_name_is_owned() {
    let src =
        "package p\ntype Wrap[T any] struct{}\nfunc (Wrap[T]) m() {}\nfunc (*Wrap[T]) n() {}\n";
    let owners = owned(&built(src));
    assert!(owners.contains(&pair("Wrap", "m")), "{owners:?}");
    assert!(owners.contains(&pair("Wrap", "n")), "{owners:?}");
}

// Negative: a type parameter is a placeholder. Even when a struct named `T`
// exists, a variable typed `T` binds nothing, so `x.run()` records no call to
// `T.run`. A second `run` on `Other` keeps a bare-name fallback ambiguous.
#[test]
fn test_calls_go_type_parameter_local_binds_nothing() {
    let src = r#"package p

type T struct{}

func (t T) run() {}

type Other struct{}

func (o Other) run() {}

type Wrap[T any] struct{ v T }

func (w Wrap[T]) put(x T) { x.run() }

func free[T any](x T) {
	var y T
	y.run()
	x.run()
}
"#;
    let g = built(src);
    assert_eq!(called(&g, "put"), Vec::<String>::new());
    assert_eq!(called(&g, "free"), Vec::<String>::new());
}

// Edge: an empty type-argument list is not generic syntax Go accepts; the
// parser recovers, and the method must still not panic or own a bracketed name.
#[test]
fn test_has_method_go_empty_type_argument_list_owns_no_bracketed_name() {
    let src = "package p\ntype Wrap[T any] struct{}\nfunc (w Wrap[]) m() {}\n";
    let owners = owned(&built(src));
    assert!(owners.iter().all(|(o, _)| !o.contains('[')), "{owners:?}");
}

// Edge: the same file parsed twice yields identical edges.
#[test]
fn test_has_method_go_generic_receiver_is_deterministic() {
    assert_eq!(owned(&built(SRC)), owned(&built(SRC)));
}

// Stands for: a variable typed by a type parameter (`var x T`) holds a
// placeholder, so `x.m` in value position never binds the project type `T`.
// The parameter is seen from a func literal, which Go cannot give a list of
// its own, and from a generic receiver. Both read the one `go_type_params`.
const TYPE_PARAM_VALUES: &str = r#"package p

type T struct{}

func (t T) m() {}

type Box[T any] struct{}

func inLiteral[T any]() {
	f := func() {
		var x T
		g := x.m
		_ = g
	}
	f()
}

func (b Box[T]) viaReceiver() {
	var x T
	g := x.m
	_ = g
}

func plain() {
	var x T
	g := x.m
	_ = g
}
"#;

#[test]
fn test_function_refs_go_func_literal_in_generic_function_binds_nothing() {
    let g = built(TYPE_PARAM_VALUES);
    assert_eq!(function_refs(&g, "inLiteral"), Vec::new());
}

#[test]
fn test_function_refs_go_generic_receiver_type_parameter_binds_nothing() {
    let g = built(TYPE_PARAM_VALUES);
    assert_eq!(function_refs(&g, "viaReceiver"), Vec::new());
}

/// Control: the same body in a function with no type parameter binds `T.m`.
#[test]
fn test_function_refs_go_plain_function_binds_method_of_project_type() {
    let g = built(TYPE_PARAM_VALUES);
    let found = function_refs(&g, "plain");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].1, "m");
}
