//! Receiver typing, e2e: the binder types a method parameter (`Derived d`). The
//! callee's method is inherited from a base type in another file, and an
//! unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::types::CallSite;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use receiver_typing_support::{assert_single_call_into_base, build, callee_files, parse_all};

/// [`receiver_typing_support::call_sites`] of `source` read as `A.cs`.
fn call_sites(source: &str, caller: &str) -> Vec<String> {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    receiver_typing_support::call_sites(&provider, "A.cs", source, caller)
}

fn sorted(sites: Vec<CallSite<'_>>) -> Vec<String> {
    let mut text: Vec<String> = sites.into_iter().map(|site| format!("{site:?}")).collect();
    text.sort();
    text
}

/// `(start line, confidence, reason)` of every `Calls` edge from the node
/// named `caller` to a node named `callee`.
fn call_edges(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<(u32, f32, String)> {
    let pool = &graph.string_pool;
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            (
                graph.nodes[e.target as usize].span.0,
                e.confidence,
                e.reason.resolve(pool).to_string(),
            )
        })
        .collect()
}

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/Base.cs",
                r#"public class Base
{
    public int Greet()
    {
        return 1;
    }
}
"#,
            ),
            (
                "src/Derived.cs",
                r#"public class Derived : Base
{
}
"#,
            ),
            (
                "src/Decoy.cs",
                r#"public class Decoy
{
    public int Greet()
    {
        return 2;
    }
}
"#,
            ),
            (
                "src/App.cs",
                r#"public class App
{
    public int Run(Derived d)
    {
        return d.Greet();
    }
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "Run", "Derived.Greet", "Greet", "src/Base.cs");
}

// FU-2026-10-25-933678d6bd56. A string literal is a `String` value, in each
// literal form and when empty. It is spelled by its `System` name: the
// keyword `string.M()` names the type, a static access that no extension
// method reaches. Before the fix each site was an untyped member call.
#[test]
fn test_literal_receiver_string_forms_are_typed_string() {
    let sites = call_sites(
        "class Lit
{
    void Strings()
    {
        \"a\".A(1);
        @\"b\".B(1);
        $\"c{n}\".C(1);
        \"\".D(1);
    }
}
",
        "Strings",
    );
    assert_eq!(
        sites,
        sorted(vec![
            CallSite::Plain("String.A"),
            CallSite::Plain("String.B"),
            CallSite::Plain("String.C"),
            CallSite::Plain("String.D"),
        ])
    );
}

// The other literals take the `System` type the language gives them: an
// integer the first of int, uint, long, ulong that holds it (or its suffix),
// a real `double` unless `f` / `m` says otherwise, a char, a bool. `null`
// has no type and stays an untyped member call.
#[test]
fn test_literal_receiver_numeric_char_bool_and_null_types() {
    let sites = call_sites(
        "class Lit
{
    void Values()
    {
        (7).IntM();
        0xFF.HexM();
        1U.UintM();
        4000000000.BigM();
        5L.LongM();
        2UL.UlongM();
        99999999999999999999.HugeM();
        1.5f.FloatM();
        1.5d.DoubleM();
        1.5.PlainM();
        2m.DecimalM();
        'c'.CharM();
        true.BoolM();
        null.NullM();
    }
}
",
        "Values",
    );
    assert_eq!(
        sites,
        sorted(vec![
            CallSite::Plain("Int32.IntM"),
            CallSite::Plain("Int32.HexM"),
            CallSite::Plain("UInt32.UintM"),
            CallSite::Plain("UInt32.BigM"),
            CallSite::Plain("Int64.LongM"),
            CallSite::Plain("UInt64.UlongM"),
            CallSite::Plain("Single.FloatM"),
            CallSite::Plain("Double.DoubleM"),
            CallSite::Plain("Double.PlainM"),
            CallSite::Plain("Decimal.DecimalM"),
            CallSite::Plain("Char.CharM"),
            CallSite::Plain("Boolean.BoolM"),
            CallSite::UntypedMember("NullM"),
            CallSite::UntypedMember("HugeM"),
        ])
    );
}

const LIT_EXT: (&str, &str) = (
    "Lib/StringExt.cs",
    "namespace Lib
{
    public static class StringExt
    {
        public static string FormatWith(this string s, object a) { return s; }
    }
}
",
);

const LIT_USE: (&str, &str) = (
    "App/Lit.cs",
    "using Lib;
namespace App
{
    public class Lit
    {
        public string F() { return \"..\".FormatWith(1); }
    }
}
",
);

const LIT_INT_EXT: (&str, &str) = (
    "Lib/IntExt.cs",
    "namespace Lib
{
    public static class IntExt
    {

        public static string FormatWith(this int n, object a) { return null; }
    }
}
",
);

// The graph side of the literal. Contract: a `String` receiver picks the
// `this string` declaration (line 4): `this int` does not accept a
// `String`. Before the receiver typing the receiver was untyped, both
// declarations applied, and the site bound neither. `String` is a sealed
// predefined type that declares no instance `FormatWith`, so the match is
// proven (0.85, `call`); before FU 933678d6bd56 a receiver with no project
// type was never proven and the edge stayed at 0.5,
// `call:extension-heuristic`.
#[test]
fn test_resolve_call_string_literal_receiver_picks_the_string_extension() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graph = build(parse_all(&provider, &[LIT_EXT, LIT_INT_EXT, LIT_USE]));
    assert_eq!(
        call_edges(&graph, "F", "FormatWith"),
        [(4, 0.85, "call".to_string())]
    );
}

// A `u8` literal is a `ReadOnlySpan<byte>`, never a `String`: in the plain,
// verbatim and raw forms, either suffix case, parenthesized or not, the
// receiver stays untyped. Before the fix each was typed `String`.
#[test]
fn test_literal_receiver_u8_forms_stay_untyped() {
    let sites = call_sites(
        "class Lit
{
    void Spans()
    {
        \"a\"u8.A();
        (\"b\"U8).B();
        @\"c\"u8.C();
        \"\"\"d\"\"\"u8.D();
        \"\"u8.E();
    }
}
",
        "Spans",
    );
    assert_eq!(
        sites,
        sorted(vec![
            CallSite::UntypedMember("A"),
            CallSite::UntypedMember("B"),
            CallSite::UntypedMember("C"),
            CallSite::UntypedMember("D"),
            CallSite::UntypedMember("E"),
        ])
    );
}

const U8_EXT: (&str, &str) = (
    "Lib/Bytes.cs",
    "namespace Lib
{
    public static class SpanExt
    {
        public static int Bytes(this ReadOnlySpan<byte> s) { return 0; }
    }
    public static class StringExt
    {
        public static int Bytes(this string s) { return 0; }
    }
}
",
);

const U8_USE: (&str, &str) = (
    "App/Raw.cs",
    "using Lib;
namespace App
{
    public class Raw
    {
        public int F() { return (\"a\"u8).Bytes(); }
    }
}
",
);

// The graph side of the `u8` literal: `("a"u8).Bytes()` binds no edge to
// the `this string` declaration (line 8). Typed `String`, the receiver
// ruled out the `this ReadOnlySpan<byte>` rival and bound the string one.
#[test]
fn test_resolve_call_u8_literal_receiver_binds_no_string_extension() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graph = build(parse_all(&provider, &[U8_EXT, U8_USE]));
    let string_ext: Vec<u32> = call_edges(&graph, "F", "Bytes")
        .into_iter()
        .map(|(line, _, _)| line)
        .filter(|&line| line == 8)
        .collect();
    assert_eq!(string_ext, Vec::<u32>::new());
}

// FU-2026-10-25-cdfe6e8cd408. A cast names the type of its operand: a plain
// type, a keyword type (spelled by its `System` name), `as`, nested
// parentheses and a parenthesized typed local. A qualified or generic type
// name may be a project type (its last segment); a nullable type stays
// untyped. Before the fix every one was an untyped member call.
#[test]
fn test_cast_and_parenthesized_receivers_take_the_named_type() {
    let sites = call_sites(
        "class C
{
    void Casts(object o, Tok t)
    {
        ((Tok)o).CreateReader();
        (o as Tok).AsM();
        ((Ns.Qual)o).QualM();
        ((Box<int>)o).GenM();
        ((Tok?)o).NullableM();
        ((int)o).KwM();
        ((((Tok)o))).Nested();
        (t).Paren();
        ( /* c */ t ).Commented();
    }
}
",
        "Casts",
    );
    assert_eq!(
        sites,
        sorted(vec![
            CallSite::Plain("Tok.CreateReader"),
            CallSite::Plain("Tok.AsM"),
            CallSite::FromReceiver {
                receiver: "Qual",
                member: "QualM"
            },
            CallSite::FromReceiver {
                receiver: "Box",
                member: "GenM"
            },
            CallSite::UntypedMember("NullableM"),
            CallSite::Plain("Int32.KwM"),
            CallSite::Plain("Tok.Nested"),
            CallSite::Plain("Tok.Paren"),
            CallSite::Plain("Tok.Commented"),
        ])
    );
}

// Element access has no element type to read: `a[0].M()` stays an untyped
// member call (`a?[0]()` names no method to record).
#[test]
fn test_element_access_receiver_stays_untyped() {
    let sites = call_sites(
        "class C
{
    void Elems(Tok[] a)
    {
        a[0].ElemM();
    }
}
",
        "Elems",
    );
    assert_eq!(sites, sorted(vec![CallSite::UntypedMember("ElemM")]));
}

// Degenerate input: an empty file and a whitespace-only one record nothing.
#[test]
fn test_receiver_typing_empty_and_whitespace_only_source_records_no_calls() {
    for source in ["", "  \n\t\n"] {
        assert_eq!(call_sites(source, "Anything"), Vec::<String>::new());
    }
}

// The graph side of the cast: `((Tok)o).CreateReader()` binds the member of
// the cast type in its own file. Before the fix the untyped receiver bound
// no edge.
// `dynamic` parses as an identifier but names no type: a cast to it, a
// parameter, a local and a field of it leave the value untyped, so the
// resolver keeps its member fallback instead of looking up `dynamic.M`.
#[test]
fn test_dynamic_receivers_stay_untyped() {
    let sites = call_sites(
        "class C
{
    dynamic field;
    void Use(object o, dynamic p)
    {
        dynamic local = o;
        ((dynamic)o).CastM();
        (o as dynamic).AsM();
        p.ParamM();
        local.LocalM();
        field.FieldM();
    }
}
",
        "Use",
    );
    assert_eq!(
        sites,
        sorted(vec![
            CallSite::UntypedMember("CastM"),
            CallSite::UntypedMember("AsM"),
            CallSite::UntypedMember("ParamM"),
            CallSite::UntypedMember("LocalM"),
            CallSite::UntypedMember("FieldM"),
        ])
    );
}

#[test]
fn test_resolve_call_cast_receiver_binds_member_of_cast_type() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graph = build(parse_all(
        &provider,
        &[
            (
                "Lib/Tok.cs",
                "namespace Lib { public class Tok { public void CreateReader() { } } }\n",
            ),
            (
                "Lib/Decoy.cs",
                "namespace Lib { public class Decoy { public void CreateReader() { } } }\n",
            ),
            (
                "Lib/Use.cs",
                "namespace Lib { public class Use { public void Run(object o) { ((Tok)o).CreateReader(); } } }\n",
            ),
        ],
    ));
    assert_eq!(callee_files(&graph, "Run", "CreateReader"), ["Lib/Tok.cs"]);
}

// Guard for FU-2026-10-26-755942c857a3: the explicit implementation and
// the ordinary member both carry the bare name `Remove`, and
// `b.Remove(null)` on a `Bag` binds the ordinary one (line 12), never the
// explicit one (line 7). Verified against 0.17.2 (edge to line 13, 1-based,
// at 0.85). The FU's own shape, an explicit implementation on a nearer base
// class than the ordinary member, binds nothing today: the walk also meets
// the interface declaration, and the arity check that separates them is
// still open.
#[test]
fn test_resolve_call_explicit_interface_member_is_not_an_ordinary_target() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let source = "interface IBag
{
    bool Remove(object o);
}

class Bag : IBag
{
    bool IBag.Remove(object o)
    {
        return false;
    }

    public bool Remove(object o)
    {
        return true;
    }
}

class Caller
{
    void Go(Bag b)
    {
        b.Remove(null);
    }
}
";
    let graph = build(parse_all(&provider, &[("Bag.cs", source)]));
    let lines: Vec<u32> = call_edges(&graph, "Go", "Remove")
        .into_iter()
        .map(|(line, _, _)| line)
        .collect();
    assert_eq!(lines, [12]);
}

// An interface-typed call binds the interface declaration (line 2), from the
// implementing class and from another class in the same file. The receiver
// typer spells the call `IBag.Remove`; while 56dbd869 named the explicit
// implementation by that same string, the same-file tier bound `Clear` to it
// at 1.0 and `Go` to nothing. 0.17.2 binds both to the interface at 0.85.
#[test]
fn test_resolve_call_interface_typed_receiver_binds_the_interface_member() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let source = "interface IBag
{
    void Remove(object o);
}

class Bag : IBag
{
    void IBag.Remove(object o)
    {
    }

    public void Clear(IBag x)
    {
        x.Remove(1);
    }
}

class Other
{
    void Go(IBag b)
    {
        b.Remove(1);
    }
}
";
    let graph = build(parse_all(&provider, &[("Bag.cs", source)]));
    for caller in ["Clear", "Go"] {
        let lines: Vec<u32> = call_edges(&graph, caller, "Remove")
            .into_iter()
            .map(|(line, _, _)| line)
            .collect();
        assert_eq!(lines, [2], "{caller}");
    }
}

// A property that only an explicit implementation declares is no member in
// scope by its simple name (C# reaches it only through the interface), so
// `Item.Run()` stays a name that may be a type. The node keeps the bare
// name `Item`; the receiver map, not the node name, skips it.
#[test]
fn test_explicit_interface_property_does_not_type_a_simple_name_receiver() {
    let sites = call_sites(
        "class Holder : IHolder
{
    Widget IHolder.Item { get; }
    void Use() { Item.Run(); }
}
",
        "Use",
    );
    assert_eq!(
        sites,
        sorted(vec![CallSite::FromReceiver {
            receiver: "Item",
            member: "Run"
        }])
    );
}
