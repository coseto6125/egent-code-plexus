//! Receiver typing, e2e, C: no edge, by design. C has no inheritance (an
//! embedded struct member is not heritage) and no method-call syntax. Its
//! binder rewrites `f(recv)` to `T.f` only when `f` is defined in the same
//! file with a receiver-convention first parameter (`T *self` / `this` /
//! `me`), so a call into another file stays the bare name. With `greet` on
//! two structs in two files, the bare name is ambiguous and the ladder never
//! runs (it needs a qualified callee). The assertion pins that no edge
//! appears.

mod receiver_typing_support;

use ecp_analyzer::c::parser::CProvider;
use receiver_typing_support::{assert_binder_emits, build, callee_files, parse_all};

#[test]
fn test_embedded_struct_call_into_other_file_stays_bare_and_unresolved() {
    let provider = CProvider::new().expect("CProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/base.c",
                "struct Base { int x; };\n\nint greet(struct Base *self) {\n    return self->x;\n}\n",
            ),
            (
                "src/decoy.c",
                "struct Decoy { int y; };\n\nint greet(struct Decoy *self) {\n    return self->y;\n}\n",
            ),
            (
                "src/app.c",
                "struct Derived { struct Base *base; };\n\nint run(struct Derived *d) {\n    return greet(d->base);\n}\n",
            ),
        ],
    );
    assert_binder_emits(&graphs, "run", "greet");
    let graph = build(graphs);
    assert!(
        callee_files(&graph, "run", "greet").is_empty(),
        "an ambiguous bare C call must stay without an edge"
    );
}
