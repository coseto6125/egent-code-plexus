//! Rust parser fixes: nested-use import sources and turbofish calls.

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{CallSite, LocalGraph};
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    RustProvider::new()
        .expect("RustProvider::new")
        .parse_file(Path::new("a.rs"), src.as_bytes())
        .expect("parse_file")
}

/// `(source, imported_name, alias)` of every import, in emitted order.
fn imports(g: &LocalGraph) -> Vec<(&str, &str, Option<&str>)> {
    g.imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect()
}

fn callee_names<'g>(g: &'g LocalGraph, caller: &str) -> Vec<&'g str> {
    g.nodes
        .iter()
        .find(|n| n.name == caller)
        .unwrap_or_else(|| panic!("no node {caller}"))
        .calls
        .iter()
        .map(|raw| CallSite::parse(raw).fallback().name())
        .collect()
}

// FU-2026-10-30-ca05646baa7b
#[test]
fn test_rust_nested_use_alias_in_nested_list_source_has_no_trailing_separator() {
    let g = parse("use a::{b::{c as d}};\n");
    let got = imports(&g);
    assert!(
        got.contains(&("a::b", "c", Some("d"))),
        "expected source a::b for `c as d`, got {got:?}"
    );
    assert!(
        got.iter().all(|(source, ..)| !source.ends_with("::")),
        "{got:?}"
    );
}

// FU-2026-10-30-ca05646baa7b
#[test]
fn test_rust_nested_use_alias_two_levels_deep_keeps_full_module_path() {
    let g = parse("use a::{b::{c::{e as f}}};\n");
    let got = imports(&g);
    assert!(got.contains(&("a::b::c", "e", Some("f"))), "{got:?}");
    assert!(
        got.iter().all(|(source, ..)| !source.ends_with("::")),
        "{got:?}"
    );
}

// FU-2026-10-30-ca05646baa7b
#[test]
fn test_rust_nested_use_alias_beside_plain_item_emits_both() {
    let g = parse("use a::{b::{c as d, e}};\n");
    let got = imports(&g);
    assert!(got.contains(&("a::b", "c", Some("d"))), "{got:?}");
    assert!(got.contains(&("a::b", "e", None)), "{got:?}");
    assert!(
        got.iter().all(|(source, ..)| !source.ends_with("::")),
        "{got:?}"
    );
}

// FU-2026-10-30-ca05646baa7b
#[test]
fn test_rust_use_alias_in_flat_list_stays_one_import() {
    let g = parse("use a::{b as d};\n");
    let got = imports(&g);
    assert_eq!(
        got.iter().filter(|(_, name, _)| *name == "b").count(),
        1,
        "{got:?}"
    );
    assert!(got.contains(&("a", "b", Some("d"))), "{got:?}");
}

// FU-2026-10-30-c017b835ee81
#[test]
fn test_rust_turbofish_call_followed_by_try_gets_calls_entry() {
    let g = parse("fn b<T>() -> Option<T> { None }\nfn a() -> Option<u8> { b::<u8>()?; None }\n");
    assert_eq!(callee_names(&g, "a"), ["b"]);
}

// FU-2026-10-30-c017b835ee81
#[test]
fn test_rust_scoped_turbofish_call_followed_by_try_keeps_path() {
    let g = parse("fn a() -> Option<u8> { m::b::<u8>(1)?; None }\n");
    let names = callee_names(&g, "a");
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with('b'), "{names:?}");
}

// FU-2026-10-30-c017b835ee81
#[test]
fn test_rust_turbofish_call_without_try_drops_type_arguments_from_callee() {
    let g = parse("fn b<T>() {}\nfn a() { b::<u8>(); }\n");
    assert_eq!(callee_names(&g, "a"), ["b"]);
}

// FU-2026-10-30-c017b835ee81
#[test]
fn test_rust_plain_call_followed_by_try_keeps_calls_entry() {
    let g = parse("fn b() -> Option<u8> { None }\nfn a() -> Option<u8> { b()?; None }\n");
    assert_eq!(callee_names(&g, "a"), ["b"]);
}

// FU-2026-10-30-c017b835ee81 sibling: method-call turbofish.
#[test]
fn test_rust_method_turbofish_call_drops_type_arguments_from_callee() {
    let g = parse("fn a(x: Foo) { x.m::<u8>(); }\n");
    let names = callee_names(&g, "a");
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(!names[0].contains('<'), "{names:?}");
}

/// Every call entry of `caller`, decoded.
fn sites<'g>(g: &'g LocalGraph, caller: &str) -> Vec<CallSite<'g>> {
    g.nodes
        .iter()
        .find(|n| n.name == caller)
        .unwrap_or_else(|| panic!("no node {caller}"))
        .calls
        .iter()
        .map(|raw| CallSite::parse(raw))
        .collect()
}

/// The call entries of `caller` are exactly `want`, in any order.
fn assert_sites(g: &LocalGraph, caller: &str, want: &[CallSite<'_>]) {
    let got = sites(g, caller);
    assert_eq!(got.len(), want.len(), "got {got:?}, want {want:?}");
    for site in want {
        assert!(got.contains(site), "missing {site:?} in {got:?}");
    }
}

// FU-2026-10-30 round 2, item C2. Shape from tokio `tests/task_blocking.rs:133`:
// the initializer of `let err: &'static str = err.downcast_ref::<..>()` reads
// the earlier, untyped `err`, so the call has an untyped receiver. Only the
// later `err.contains` sees the new `str` binding.
#[test]
fn test_rust_typed_let_initializer_reads_earlier_untyped_binding() {
    let g = parse(
        "fn f(result: Result<(), Box<dyn Any>>) {\n    let err = result.unwrap_err();\n    let err: &'static str = err.downcast_ref::<&'static str>().unwrap();\n    err.contains(\"x\");\n}\n",
    );
    let got = sites(&g, "f");
    assert!(
        got.contains(&CallSite::UntypedMember("downcast_ref")),
        "{got:?}"
    );
    assert!(
        !got.contains(&CallSite::Plain("str.downcast_ref")),
        "{got:?}"
    );
    assert!(got.contains(&CallSite::Plain("str.contains")), "{got:?}");
}

// C2: a typed `let` over a typed parameter. The initializer sees the
// parameter's type, the statements after it the new type.
#[test]
fn test_rust_typed_let_shadowing_parameter_initializer_keeps_parameter_type() {
    let g = parse("fn a(x: Dog) { let x: Cat = x.m(); x.n(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("Dog.m"), CallSite::Plain("Cat.n")],
    );
}

// C2 atypical: two shadows in a row; each initializer reads the binding
// before it.
#[test]
fn test_rust_repeated_typed_let_shadow_each_initializer_reads_previous_type() {
    let g = parse("fn a(x: Dog) { let x: Cat = x.m(); let x: Bird = x.n(); x.o(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("Dog.m"),
            CallSite::Plain("Cat.n"),
            CallSite::Plain("Bird.o"),
        ],
    );
}

// C2 atypical: the initializer does not mention the old name. A use before
// the `let` still sees the parameter's type.
#[test]
fn test_rust_use_before_typed_let_keeps_parameter_type() {
    let g = parse("fn a(x: Dog) { x.m(); let x: Cat = make(); x.n(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("Dog.m"),
            CallSite::Plain("make"),
            CallSite::Plain("Cat.n"),
        ],
    );
}

// C2 atypical: a shadow inside a nested block ends with the block.
#[test]
fn test_rust_shadow_in_nested_block_ends_with_block() {
    let g = parse("fn a(x: Dog) { { let x: Cat = make(); x.n(); } x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("make"),
            CallSite::Plain("Cat.n"),
            CallSite::Plain("Dog.m"),
        ],
    );
}

// C2 atypical: a name bound only inside a block is unbound after it.
#[test]
fn test_rust_let_in_nested_block_leaves_no_binding_after_block() {
    let g = parse("fn a() { { let x: Cat = make(); x.n(); } x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("make"),
            CallSite::Plain("Cat.n"),
            CallSite::UntypedMember("m"),
        ],
    );
}

// C2 atypical: a shadow in one match arm does not reach the next arm.
#[test]
fn test_rust_shadow_in_match_arm_block_does_not_reach_other_arm() {
    let g = parse(
        "fn a(x: Dog) { match g() { A => { let x: Cat = make(); x.n(); } B => { x.m(); } } }\n",
    );
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("g"),
            CallSite::Plain("make"),
            CallSite::Plain("Cat.n"),
            CallSite::Plain("Dog.m"),
        ],
    );
}

// C2 sibling (beyond the `let` text of the item): an untyped `let` shadows a
// typed parameter, so the receiver after it is untyped.
#[test]
fn test_rust_untyped_let_shadowing_typed_parameter_makes_receiver_untyped() {
    let g = parse("fn a(x: Dog) { let x = make(); x.n(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("make"), CallSite::UntypedMember("n")],
    );
}

// C2 sibling: a destructuring `let` binds `x` as a new value.
#[test]
fn test_rust_tuple_let_shadowing_typed_parameter_makes_receiver_untyped() {
    let g = parse("fn a(x: Dog) { let (x, y) = pair(); x.n(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("pair"), CallSite::UntypedMember("n")],
    );
}

// C2 sibling: an untyped closure parameter of the same name hides the
// typed parameter inside the closure only.
#[test]
fn test_rust_closure_parameter_shadows_typed_parameter_inside_closure_only() {
    let g = parse("fn a(x: Dog) { let f = |x| x.n(); x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::UntypedMember("n"), CallSite::Plain("Dog.m")],
    );
}

// C2 sibling: a match-arm pattern binds a new `x` for its arm only.
#[test]
fn test_rust_match_arm_pattern_shadows_typed_parameter_inside_arm_only() {
    let g = parse("fn a(x: Dog) { match g() { Some(x) => x.n(), None => {} } x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("g"),
            CallSite::UntypedMember("n"),
            CallSite::Plain("Dog.m"),
        ],
    );
}

// C2 guard: a typed `let` with no shadowing types its receiver.
#[test]
fn test_rust_typed_let_without_shadow_types_receiver() {
    let g = parse("fn a() { let x: Dog = make(); x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("make"), CallSite::Plain("Dog.m")],
    );
}

// C2 guard: `self` still resolves to the impl type.
#[test]
fn test_rust_self_receiver_resolves_to_impl_type() {
    let g = parse("struct Dog;\nimpl Dog { fn a(&self) { self.m(); } }\n");
    assert_sites(&g, "a", &[CallSite::Plain("Dog.m")]);
}

// C2 guard: the same name in two functions keeps one type per function.
#[test]
fn test_rust_same_name_in_two_functions_keeps_each_type() {
    let g = parse("fn a(x: Dog) { x.m(); }\nfn b() { let x: Cat = make(); x.n(); }\n");
    assert_sites(&g, "a", &[CallSite::Plain("Dog.m")]);
    assert_sites(
        &g,
        "b",
        &[CallSite::Plain("make"), CallSite::Plain("Cat.n")],
    );
}

// C2 guard: a `let _` binds no name, so the parameter keeps its type.
#[test]
fn test_rust_wildcard_let_keeps_parameter_type() {
    let g = parse("fn a(x: Dog) { let _: Cat = x.m(); x.n(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("Dog.m"), CallSite::Plain("Dog.n")],
    );
}

// C2 guard: two typed lets side by side keep their own types.
#[test]
fn test_rust_two_typed_lets_side_by_side_keep_their_types() {
    let g = parse("fn a() { let p: Cat = make(); let q: Dog = make(); p.m(); q.n(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("make"),
            CallSite::Plain("make"),
            CallSite::Plain("Cat.m"),
            CallSite::Plain("Dog.n"),
        ],
    );
}

// C2 guard: an empty file has no nodes.
#[test]
fn test_rust_empty_file_has_no_nodes() {
    assert!(parse("").nodes.is_empty());
}

// Round 3, C-R1. The guard of a match arm (`p if cond =>`) is no part of the
// pattern: `buf` there is the parameter, not an arm binding.
#[test]
fn test_rust_match_guard_identifier_keeps_parameter_type() {
    let g = parse("fn a(buf: Buf) { match g() { (false, None) if buf.len() > 3 => 1, _ => 2 } }\n");
    assert_sites(&g, "a", &[CallSite::Plain("g"), CallSite::Plain("Buf.len")]);
}

// C-R1 neighbour: a name the pattern does bind shadows in the guard and the
// arm body alike.
#[test]
fn test_rust_match_guard_sees_name_bound_by_the_same_pattern() {
    let g =
        parse("fn a(buf: Buf) { match g() { Some(buf) if buf.len() > 3 => buf.n(), _ => 2 } }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("g"),
            CallSite::UntypedMember("len"),
            CallSite::UntypedMember("n"),
        ],
    );
}

// Round 3, C-R2. A struct pattern as a closure parameter names its type in a
// `type` field; the names it binds have no known type.
#[test]
fn test_rust_closure_struct_pattern_parameter_binds_names_untyped() {
    let g = parse("fn a(ctx: Ctx) { let f = |LocalData { ctx, .. }| ctx.set(); ctx.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::UntypedMember("set"), CallSite::Plain("Ctx.m")],
    );
}

// C-R2: the same for a tuple-struct pattern.
#[test]
fn test_rust_closure_tuple_struct_pattern_parameter_binds_names_untyped() {
    let g = parse("fn a(x: Dog) { let f = |Wrapper(x)| x.go(); x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::UntypedMember("go"), CallSite::Plain("Dog.m")],
    );
}

// C-R2 neighbour: a typed closure parameter keeps its annotation.
#[test]
fn test_rust_closure_typed_parameter_keeps_its_type() {
    let g = parse("fn a(s: Dog) { let f = |s: Cat| s.n(); s.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("Cat.n"), CallSite::Plain("Dog.m")],
    );
}

// Round 3, C-R3. `_` asks the compiler to infer a type; it is no type name.
#[test]
fn test_rust_inferred_underscore_closure_parameter_type_is_unknown() {
    let g = parse("fn a(s: Dog) { let f = |s: &mut _| s.size_hint(); s.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::UntypedMember("size_hint"),
            CallSite::Plain("Dog.m"),
        ],
    );
}

// C-R3: the same for a `let` annotation.
#[test]
fn test_rust_inferred_underscore_let_type_is_unknown() {
    let g = parse("fn a() { let x: _ = make(); x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("make"), CallSite::UntypedMember("m")],
    );
}

// Round 3, C-R4. `let x = x;` and `let mut x = x;` keep the type `x` had.
#[test]
fn test_rust_let_rebinding_same_name_keeps_type() {
    let g = parse("fn a(x: Dog) { let x = x; x.m(); }\nfn b(x: Dog) { let mut x = x; x.m(); }\n");
    assert_sites(&g, "a", &[CallSite::Plain("Dog.m")]);
    assert_sites(&g, "b", &[CallSite::Plain("Dog.m")]);
}

// C-R4: `let x = x.clone();` keeps it too.
#[test]
fn test_rust_let_rebinding_clone_keeps_type() {
    let g = parse("fn a(x: &Dog) { let x = x.clone(); x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("Dog.clone"), CallSite::Plain("Dog.m")],
    );
}

// C-R4 atypical: a chain of aliases under other names resolves in order.
#[test]
fn test_rust_let_alias_chain_keeps_type() {
    let g = parse("fn a(x: Dog) { let y = x; let z = y.clone(); z.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[CallSite::Plain("Dog.clone"), CallSite::Plain("Dog.m")],
    );
}

// C-R4 atypical: the alias sits in a closure body and reads a captured name.
#[test]
fn test_rust_let_alias_inside_closure_keeps_captured_type() {
    let g = parse("fn a(buf: Buf) { spawn(move || { let mut buf = buf; buf.read(); }); }\n");
    assert_sites(&g, "a", &[CallSite::Plain("spawn")]);
    // A closure passed as a call argument owns its calls on an `<anonymous>` node.
    assert_sites(&g, &anonymous_name(&g), &[CallSite::Plain("Buf.read")]);
}

fn anonymous_name(g: &LocalGraph) -> String {
    g.nodes
        .iter()
        .find(|n| n.name.starts_with("<anonymous"))
        .map(|n| n.name.clone())
        .expect("no <anonymous> node")
}

// C-R4 neighbour: an annotated `let` inside a closure binds its type.
#[test]
fn test_rust_annotated_let_inside_closure_binds_its_type() {
    let g = parse("fn a(rt: Dog) { spawn(move || { let rt: Runtime = make(); rt.go(); }); }\n");
    assert_sites(&g, "a", &[CallSite::Plain("spawn")]);
    assert_sites(
        &g,
        &anonymous_name(&g),
        &[CallSite::Plain("make"), CallSite::Plain("Runtime.go")],
    );
}

// C-R4 neighbour: only a bare name or `name.clone()` keeps the type; any
// other initializer gives an unknown one.
#[test]
fn test_rust_let_from_other_initializer_stays_untyped() {
    let g = parse("fn a(x: Dog) { let x = x.to_string(); x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::Plain("Dog.to_string"),
            CallSite::UntypedMember("m"),
        ],
    );
}

// C-R4 neighbour: the aliased name has no known type.
#[test]
fn test_rust_let_alias_of_untyped_name_stays_untyped() {
    let g = parse("fn a() { let x = x.clone(); x.m(); }\nfn b(x: Dog) { let x = make(); let x = x; x.m(); }\n");
    assert_sites(
        &g,
        "a",
        &[
            CallSite::UntypedMember("clone"),
            CallSite::UntypedMember("m"),
        ],
    );
    assert_sites(
        &g,
        "b",
        &[CallSite::Plain("make"), CallSite::UntypedMember("m")],
    );
}
