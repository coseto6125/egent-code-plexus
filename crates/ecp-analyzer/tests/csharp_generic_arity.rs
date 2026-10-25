//! C# declarations that differ only in generic arity keep their own nodes
//! through the full build (codex ruling P-a2). Two shapes from Newtonsoft
//! still lost a node after P-a:
//!
//! - `delegate TResult Func<T, TResult>(T a)` beside `Func<T1, T2, TResult>`
//!   in `Utilities/LinqBridge.cs`: the parser emits a delegate as a
//!   `Function`, and only types split by arity.
//! - `Foo1.foo` beside `Foo1<T>.foo` in `TestObjects/Foo1.cs`: the classes
//!   split, but their same-name properties shared one uid, so the second
//!   was a nameless tombstone and `HasProperty` bound `Foo1<T>` to the
//!   first.
//!
//! Each fixture is one file, so a node's 0-based start row names it.

mod overrides_support;

use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use overrides_support::edge_ends;
use std::path::Path;

fn csharp_graph(source: &str) -> ZeroCopyGraph {
    overrides_support::graph_of(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        &[("App/Shapes.cs", source)],
    )
}

/// `(0-based row, name)` of every node of `kind`, sorted. A tombstone has
/// the empty name.
fn rows(graph: &ZeroCopyGraph, kind: NodeKind) -> Vec<(u32, String)> {
    let pool = graph.string_pool.as_slice();
    let mut rows: Vec<(u32, String)> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| (n.span.0, n.name.resolve(pool).to_string()))
        .collect();
    rows.sort_unstable();
    rows
}

/// `(source row, target row)` of every `rel` edge, sorted.
fn edge_rows(graph: &ZeroCopyGraph, rel: RelType) -> Vec<(u32, u32)> {
    let mut rows: Vec<(u32, u32)> = edge_ends(graph, rel)
        .into_iter()
        .map(|(source, target)| (source.line, target.line))
        .collect();
    rows.sort_unstable();
    rows
}

/// [`edge_rows`] of the `ReadsField` edges that end at a `Property`.
fn property_reads(graph: &ZeroCopyGraph) -> Vec<(u32, u32)> {
    let mut rows: Vec<(u32, u32)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::ReadsField)
        .map(|e| {
            (
                &graph.nodes[e.source as usize],
                &graph.nodes[e.target as usize],
            )
        })
        .filter(|(_, target)| target.kind == NodeKind::Property)
        .map(|(source, target)| (source.span.0, target.span.0))
        .collect();
    rows.sort_unstable();
    rows
}

fn named(rows: &[(u32, &str)]) -> Vec<(u32, String)> {
    rows.iter()
        .map(|&(row, name)| (row, name.to_string()))
        .collect()
}

const DELEGATES: &str = "namespace N
{
    public delegate void Action();
    public delegate void Action<T>(T a);
    public delegate void Action<T1, T2>(T1 a, T2 b);
    public delegate TResult Func<TResult>();
    public delegate TResult Func<T, TResult>(T a);
    public delegate TResult Func<T1, T2, T3, TResult>(T1 a, T2 b, T3 c);
}
";

const DELEGATES_REVERSED: &str = "namespace N
{
    public delegate TResult Func<T1, T2, T3, TResult>(T1 a, T2 b, T3 c);
    public delegate TResult Func<T, TResult>(T a);
    public delegate TResult Func<TResult>();
    public delegate void Action<T1, T2>(T1 a, T2 b);
    public delegate void Action<T>(T a);
    public delegate void Action();
}
";

/// Stands for: delegates of zero, one and several generic parameters in
/// one file are one node each, named, in either declaration order, and the
/// build records no uid collision for them. Before the fix every delegate
/// after the first of its name was an empty-named tombstone with a
/// `uid-collision` BlindSpot.
#[test]
fn test_build_csharp_delegates_of_several_arities_both_orders_each_named() {
    for source in [DELEGATES, DELEGATES_REVERSED] {
        let graph = csharp_graph(source);
        let expected = match source == DELEGATES {
            true => named(&[
                (2, "Action"),
                (3, "Action"),
                (4, "Action"),
                (5, "Func"),
                (6, "Func"),
                (7, "Func"),
            ]),
            false => named(&[
                (2, "Func"),
                (3, "Func"),
                (4, "Func"),
                (5, "Action"),
                (6, "Action"),
                (7, "Action"),
            ]),
        };
        assert_eq!(rows(&graph, NodeKind::Function), expected, "{source}");
        let collisions = overrides_support::blind_spots(&graph, "uid-collision");
        assert!(collisions.is_empty(), "{source}: {collisions:?}");
    }
}

/// Stands for: only another arity splits. Two delegates of one name and
/// one arity stay a uid collision: the second is the tombstone it was.
/// Passes before the fix as well; it guards the restriction of the ruling.
#[test]
fn test_build_csharp_delegates_of_one_arity_second_tombstoned() {
    let graph = csharp_graph(
        "namespace N
{
    public delegate void Handler<T>(T a);
    public delegate void Handler<U>(U b);
}
",
    );
    assert_eq!(
        rows(&graph, NodeKind::Function),
        named(&[(2, "Handler"), (3, "")])
    );
}

const FOO1: &str = "namespace N
{
    public class Foo1
    {
        public object foo { get; set; }
        public object Read() { return this.foo; }
    }
    public class Foo1<T> : Foo1
    {
        public new T foo { get; set; }
        public T foo2 { get; set; }
        public T Read2() { return this.foo; }
    }
}
";

/// Stands for: `Foo1.foo` and `Foo1<T>.foo` (Newtonsoft
/// `TestObjects/Foo1.cs`) each keep a named node; `HasProperty` binds each
/// class to its own `foo`, and a read inside each class reads its own
/// `foo`. Before the fix the second `foo` was a tombstone, `Foo1<T>` (row
/// 7) owned `foo` of row 4, and `Read2` read row 4.
#[test]
fn test_build_csharp_property_of_arity_twins_own_node_and_owner() {
    let graph = csharp_graph(FOO1);
    assert_eq!(
        rows(&graph, NodeKind::Property),
        named(&[(4, "foo"), (9, "foo"), (10, "foo2")])
    );
    assert_eq!(
        edge_rows(&graph, RelType::HasProperty),
        [(2, 4), (7, 9), (7, 10)]
    );
    assert_eq!(
        property_reads(&graph),
        [(5, 4), (11, 9)],
        "each class reads its own foo"
    );
}

/// Stands for: the declaration order does not matter. With `Foo1<T>`
/// first, its `foo` is the first node and `Foo1.foo` the twin; each still
/// binds its own class and its own reads.
#[test]
fn test_build_csharp_property_of_arity_twins_generic_first_own_node_and_owner() {
    let graph = csharp_graph(
        "namespace N
{
    public class Foo1<T> : Foo1
    {
        public new T foo { get; set; }
        public T Read2() { return this.foo; }
    }
    public class Foo1
    {
        public object foo { get; set; }
        public object Read() { return this.foo; }
    }
}
",
    );
    assert_eq!(
        rows(&graph, NodeKind::Property),
        named(&[(4, "foo"), (9, "foo")])
    );
    assert_eq!(edge_rows(&graph, RelType::HasProperty), [(2, 4), (7, 9)]);
    assert_eq!(
        property_reads(&graph),
        [(5, 4), (10, 9)],
        "each class reads its own foo"
    );
}

/// Stands for: methods of arity twins, overloads included, hang under the
/// class that declares them: `M(int)` in `Foo1`, and `M(int)` plus
/// `M(string)` in `Foo1<T>`, are three named methods, and each class owns
/// its own. Methods already kept an overload uid before the fix; this
/// guards that the member rule leaves them so.
#[test]
fn test_build_csharp_methods_and_overloads_of_arity_twins_owned_by_own_class() {
    let graph = csharp_graph(
        "namespace N
{
    public class Foo1
    {
        public void M(int a) {}
    }
    public class Foo1<T>
    {
        public void M(int a) {}
        public void M(string b) {}
    }
}
",
    );
    assert_eq!(
        rows(&graph, NodeKind::Method),
        named(&[(4, "M"), (8, "M"), (9, "M")])
    );
    assert_eq!(
        edge_rows(&graph, RelType::HasMethod),
        [(2, 4), (6, 8), (6, 9)]
    );
}

const NESTED: &str = "namespace N
{
    public class Outer
    {
        public class Inner
        {
            public int x;
            public int Get() { return this.x; }
        }
    }
    public class Outer<T>
    {
        public class Inner
        {
            public int x;
            public int Get() { return this.x; }
        }
    }
}
";

/// Stands for: a nested type tells outer twins apart. `Outer.Inner` and
/// `Outer<T>.Inner` share kind, owner name, name and arity; only the outer
/// arity differs. Each `Inner`, each `x` and each `Get` keeps a named node,
/// each `Inner` owns its own members, and each `Get` reads its own `x`.
/// Before the fix the second `Inner` and the second `x` were tombstones,
/// and the second `Get` hung under the first `Inner`.
#[test]
fn test_build_csharp_nested_type_in_outer_arity_twins_own_members() {
    let graph = csharp_graph(NESTED);
    assert_eq!(
        rows(&graph, NodeKind::Class),
        named(&[(2, "Outer"), (4, "Inner"), (10, "Outer"), (12, "Inner")])
    );
    assert_eq!(
        rows(&graph, NodeKind::Property),
        named(&[(6, "x"), (14, "x")])
    );
    assert_eq!(edge_rows(&graph, RelType::HasProperty), [(4, 6), (12, 14)]);
    assert_eq!(edge_rows(&graph, RelType::HasMethod), [(4, 7), (12, 15)]);
    assert_eq!(property_reads(&graph), [(7, 6), (15, 14)]);
}

/// Stands for: same owner, same arity keeps today's rule. Two classes `C`
/// of one arity in one file each declare `a`: the second class and its
/// `a` are the tombstones they were. Passes before the fix as well.
#[test]
fn test_build_csharp_same_arity_class_duplicates_members_tombstoned() {
    let graph = csharp_graph(
        "namespace N
{
    public class C
    {
        public int a;
    }
    public class C
    {
        public int a;
    }
}
",
    );
    assert_eq!(rows(&graph, NodeKind::Class), named(&[(2, "C"), (6, "")]));
    assert_eq!(
        rows(&graph, NodeKind::Property),
        named(&[(4, "a"), (8, "")])
    );
}

/// The `FunctionMeta.decorators` names (what `ecp inspect` and cypher
/// `f.decorators` print) of the function or method named `name`.
fn meta_decorators(graph: &ZeroCopyGraph, name: &str) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    let idx = graph
        .nodes
        .iter()
        .position(|n| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method) && n.name.resolve(pool) == name
        })
        .unwrap_or_else(|| panic!("no function {name}"));
    graph
        .function_meta(idx as u32)
        .unwrap_or_else(|| panic!("no FunctionMeta for {name}"))
        .decorators
        .iter()
        .map(|d| d.resolve(pool).to_string())
        .collect()
}

/// Stands for: a decorator list shows the attributes the source wrote, and
/// none of the parser's internal markers. A generic delegate carries the
/// arity marker and an `override` method the override sentinel; before the
/// fix `FunctionMeta.decorators` listed `__generic_arity__:2` and
/// `__override__` beside `Obsolete`.
#[test]
fn test_function_meta_csharp_internal_markers_not_decorators() {
    let graph = csharp_graph(
        "namespace N
{
    [Obsolete] public delegate TResult Conv<T, TResult>(T a);
    public class B { public virtual void M() {} }
    public class D : B { [Obsolete] public override void M() {} }
}
",
    );
    assert_eq!(meta_decorators(&graph, "Conv"), ["Obsolete"]);
    let pool = graph.string_pool.as_slice();
    let overriding: Vec<Vec<String>> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == NodeKind::Method && n.name.resolve(pool) == "M")
        .filter_map(|(i, _)| graph.function_meta(i as u32))
        .map(|m| {
            m.decorators
                .iter()
                .map(|d| d.resolve(pool).to_string())
                .collect()
        })
        .collect();
    assert!(
        overriding.contains(&vec!["Obsolete".to_string()]),
        "the override keeps its attribute alone: {overriding:?}"
    );
    assert!(
        overriding.iter().flatten().all(|d| !d.starts_with("__")),
        "no internal marker: {overriding:?}"
    );
}

const ATTRIBUTES: &str = "namespace N
{
    public class Attr : System.Attribute {}
    public class Attr<T> : System.Attribute {}
}
";

const ATTRIBUTES_REVERSED: &str = "namespace N
{
    public class Attr<T> : System.Attribute {}
    public class Attr : System.Attribute {}
}
";

/// Stands for: attribute classes of two generic arities in one file are two
/// named `Annotation` nodes, in either declaration order, and the build
/// records no uid collision for them. Before the fix the arity of `Attr<T>`
/// read 0, so it was an empty-named tombstone with a `uid-collision`
/// BlindSpot.
#[test]
fn test_build_csharp_attribute_classes_of_both_arities_both_orders_each_named() {
    for source in [ATTRIBUTES, ATTRIBUTES_REVERSED] {
        let graph = csharp_graph(source);
        assert_eq!(
            rows(&graph, NodeKind::Annotation),
            named(&[(2, "Attr"), (3, "Attr")]),
            "{source}"
        );
        let collisions = overrides_support::blind_spots(&graph, "uid-collision");
        assert!(collisions.is_empty(), "{source}: {collisions:?}");
    }
}

/// `(name, generic arity)` of every node of `kind` in one parsed C# file,
/// as the parser states them before the build.
fn parsed_arities(kind: NodeKind, source: &str) -> Vec<(String, u32)> {
    let provider = ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap();
    let local = provider
        .parse_file(Path::new("App/Shapes.cs"), source.as_bytes())
        .unwrap();
    local
        .nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| (n.name.as_str().to_string(), n.generic_arity()))
        .collect()
}

/// Stands for: an attribute class with no type parameter states arity 0, and
/// `Attr<T, U>` states its two parameters, as a class does. Before the fix
/// `Attr<T, U>` stated 0.
#[test]
fn test_parse_csharp_attribute_class_arity_follows_type_parameters() {
    assert_eq!(
        parsed_arities(
            NodeKind::Annotation,
            "namespace N
{
    public class Attr : System.Attribute {}
    public class Attr<T, U> : System.Attribute {}
}
"
        ),
        [("Attr".to_string(), 0), ("Attr".to_string(), 2)]
    );
}

/// Stands for: an attribute class nested in a generic class states its own
/// type parameters, not the outer class's: here none, so arity 0.
#[test]
fn test_parse_csharp_attribute_nested_in_generic_class_arity_is_its_own() {
    assert_eq!(
        parsed_arities(
            NodeKind::Annotation,
            "namespace N
{
    public class Outer<T>
    {
        public class Attr : System.Attribute {}
    }
}
"
        ),
        [("Attr".to_string(), 0)]
    );
}

/// Stands for: properties of arity-twin attribute classes keep their own
/// nodes. `Attr.foo` and `Attr<T>.foo` are two named properties, because the
/// member's enclosing type is found among the arity-declaring kinds, which
/// now include `Annotation`. Before the fix the second `foo` was a tombstone
/// with a `uid-collision` BlindSpot.
#[test]
fn test_build_csharp_property_of_attribute_arity_twins_own_node() {
    let graph = csharp_graph(
        "namespace N
{
    public class Attr : System.Attribute
    {
        public object foo { get; set; }
    }
    public class Attr<T> : System.Attribute
    {
        public T foo { get; set; }
    }
}
",
    );
    assert_eq!(
        rows(&graph, NodeKind::Property),
        named(&[(4, "foo"), (8, "foo")])
    );
    let collisions = overrides_support::blind_spots(&graph, "uid-collision");
    assert!(collisions.is_empty(), "{collisions:?}");
}

/// Stands for: the parts of a `partial` attribute class of a generic arity
/// are one type. The part that lists `System.Attribute` (`A<T>`, the
/// representative as the base-list part) and the plain part `A<T>` join one
/// group, in either file order. Before the fix the attribute part stated
/// arity 0, so the two parts never grouped and no `PartOf` edge existed.
#[test]
fn test_build_csharp_partial_attribute_class_of_generic_arity_parts_group() {
    let attribute = "namespace N\n{\n    public partial class A<T> : System.Attribute {}\n}\n";
    let plain = "namespace N\n{\n    public partial class A<T> {}\n}\n";
    for files in [
        [("App/A.cs", plain), ("App/A.Attr.cs", attribute)],
        [("App/A.Attr.cs", attribute), ("App/A.cs", plain)],
    ] {
        let graph = overrides_support::graph_of_projects(
            &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
            &files,
            &["App"],
        );
        let mut ends: Vec<(String, String)> = edge_ends(&graph, RelType::PartOf)
            .into_iter()
            .map(|(source, target)| (source.file, target.file))
            .collect();
        ends.sort();
        assert_eq!(
            ends,
            [("App/A.cs".to_string(), "App/A.Attr.cs".to_string())],
            "one PartOf edge from the plain part to the attribute part"
        );
    }
}
