//! Receiver typing, e2e: the binder types a method parameter (`Derived d`). The
//! callee's method is inherited from a base type in another file, and an
//! unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::csharp_extension::{HEURISTIC_CONFIDENCE, HEURISTIC_REASON};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::CallSite;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use receiver_typing_support::{assert_single_call_into_base, build, parse_all};
use std::path::Path;

/// The decoded call sites the parser records for the node named `caller`,
/// as `{:?}` text, sorted.
fn call_sites(source: &str, caller: &str) -> Vec<String> {
    let local = CSharpProvider::new()
        .expect("CSharpProvider::new")
        .parse_file(Path::new("A.cs"), source.as_bytes())
        .expect("parse_file");
    let mut sites: Vec<String> = local
        .nodes
        .iter()
        .filter(|n| n.name == caller)
        .flat_map(|n| {
            n.calls
                .iter()
                .map(|raw| format!("{:?}", CallSite::parse(raw)))
        })
        .collect();
    sites.sort();
    sites
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

// The graph side of the literal: a proven `string` extension binds as a
// typed receiver's extension does, not as the untyped heuristic (0.5,
// `call:extension-heuristic`) it was before.
#[test]
fn test_resolve_call_string_literal_receiver_binds_extension_above_heuristic() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graph = build(parse_all(&provider, &[LIT_EXT, LIT_USE]));
    let edges = call_edges(&graph, "F", "FormatWith");
    assert_eq!(edges.len(), 1, "{edges:?}");
    let (_, confidence, reason) = &edges[0];
    assert!(*confidence > HEURISTIC_CONFIDENCE, "{edges:?}");
    assert_ne!(reason, HEURISTIC_REASON, "{edges:?}");
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
    let pool = &graph.string_pool;
    let files: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "Run")
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            format!(
                "{}::{}",
                graph.files[target.file_idx as usize].path.resolve(pool),
                target.name.resolve(pool)
            )
        })
        .collect();
    assert_eq!(files, ["Lib/Tok.cs::CreateReader"]);
}

// FU-2026-10-26-755942c857a3. An explicit interface implementation is no
// member of the type by name: `b.Remove(null)` binds the ordinary `Remove`
// (line 12), never `IBag.Remove` (line 7), which is declared first. Before
// the fix both carried the name `Remove` and the call bound the explicit one.
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

// A property that only an explicit implementation declares is no member in
// scope: `Item.Run()` stays a name that may be a type, where it was typed by
// the explicit property's declared type.
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
