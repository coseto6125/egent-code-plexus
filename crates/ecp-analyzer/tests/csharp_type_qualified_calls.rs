//! C# member calls on an identifier receiver that is no local, parameter,
//! field or property in scope (`StringAssert.AreEqual()`). The parser used
//! to drop such a receiver and record the bare member, so three static
//! classes of one file that each declare `AreEqual` (Newtonsoft's
//! `TestFixtureBase.cs`) left every call ambiguous. The receiver now reaches
//! the resolver, which binds the member of the type it names, through a
//! `using` alias too, and keeps the bare-member resolution for a receiver
//! that names no type.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

/// Three static classes in one file, each with `AreEqual`; `XUnitAssert`
/// declares two overloads.
const ASSERTS: (&str, &str) = (
    "Tests/TestFixtureBase.cs",
    r#"namespace Ns.Tests
{
    public class XUnitAssert
    {
        public static void AreEqual(object expected, object actual) { }
        public static void AreEqual(double expected, double actual, double r) { }
    }

    public static class CollectionAssert
    {
        public static void AreEqual(object expected, object actual) { }
    }

    public static class StringAssert
    {
        public static void AreEqual(string expected, string actual) { }
    }
}
"#,
);

const CALLERS: (&str, &str) = (
    "Tests/UseTests.cs",
    r#"using System;
using Assert = Ns.Tests.XUnitAssert;

namespace Ns.Tests
{
    public class Helper
    {
        public void Run() { }
    }

    public class Service
    {
        public void Go() { }
    }

    public class Box<T>
    {
        public static void Make() { }
    }

    public class Outer
    {
        public static class Inner
        {
            public static void Ping() { }
        }
    }

    public class UseTests
    {
        public Helper Tool { get; }

        public void ViaString() { StringAssert.AreEqual("a", "b"); }
        public void ViaCollection() { CollectionAssert.AreEqual(1, 2); }
        public void ViaType() { XUnitAssert.AreEqual(1, 2); }
        public void ViaAlias() { Assert.AreEqual(1, 2); }
        public void ViaProperty() { Tool.Run(); }
        public void ViaLocal() { Service svc = new Service(); svc.Go(); }
        public void ViaTypedLocalShadow() { Helper StringAssert = new Helper(); StringAssert.Run(); }
        public void ViaVarLocalShadow() { var CollectionAssert = new Helper(); CollectionAssert.Run(); }
        public void ViaNested() { Outer.Inner.Ping(); }
        public void ViaGlobal() { global::Ns.Tests.StringAssert.AreEqual("a", "b"); }
        public void ViaGeneric() { Box<int>.Make(); }
        public void ViaNameof() { var n = nameof(StringAssert); }
    }
}
"#,
);

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    for &(path, source) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

fn name(graph: &ZeroCopyGraph, id: u32) -> &str {
    graph.nodes[id as usize].name.resolve(&graph.string_pool)
}

fn owner(graph: &ZeroCopyGraph, id: u32) -> &str {
    graph.nodes[id as usize]
        .owner_class
        .resolve(&graph.string_pool)
}

/// The one node named `caller`.
fn node(graph: &ZeroCopyGraph, caller: &str) -> u32 {
    let ids: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| name(graph, i) == caller)
        .collect();
    assert_eq!(ids.len(), 1, "one `{caller}` node");
    ids[0]
}

/// `(callee name, callee owner)` of every Calls edge out of `caller`.
fn calls_from(graph: &ZeroCopyGraph, caller: &str) -> Vec<(String, String)> {
    let source = node(graph, caller);
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == source)
        .map(|e| {
            (
                name(graph, e.target).to_string(),
                owner(graph, e.target).to_string(),
            )
        })
        .collect()
}

/// `caller` calls `callee` exactly once, and that callee belongs to `owner`.
fn assert_binds(graph: &ZeroCopyGraph, caller: &str, callee: &str, owner: &str) {
    let hits: Vec<(String, String)> = calls_from(graph, caller)
        .into_iter()
        .filter(|(n, _)| n == callee)
        .collect();
    assert_eq!(
        hits,
        [(callee.to_string(), owner.to_string())],
        "`{caller}` binds {owner}.{callee}"
    );
}

/// `caller` binds no `callee` of any owner other than `allowed`.
fn assert_never_binds_other(graph: &ZeroCopyGraph, caller: &str, callee: &str, allowed: &str) {
    for (n, o) in calls_from(graph, caller) {
        assert!(
            n != callee || o == allowed,
            "`{caller}` must not bind {o}.{n}"
        );
    }
}

#[test]
fn test_resolve_call_csharp_string_assert_receiver_binds_string_assert() {
    assert_binds(
        &build(&[ASSERTS, CALLERS]),
        "ViaString",
        "AreEqual",
        "StringAssert",
    );
}

#[test]
fn test_resolve_call_csharp_collection_assert_receiver_binds_collection_assert() {
    assert_binds(
        &build(&[ASSERTS, CALLERS]),
        "ViaCollection",
        "AreEqual",
        "CollectionAssert",
    );
}

/// The call binds the first overload, which references the later one
/// (`overload_set`), so upstream impact from either reaches the caller.
#[test]
fn test_resolve_call_csharp_static_type_receiver_binds_first_overload() {
    let graph = build(&[ASSERTS, CALLERS]);
    assert_binds(&graph, "ViaType", "AreEqual", "XUnitAssert");
    let mut overloads: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| name(&graph, i) == "AreEqual" && owner(&graph, i) == "XUnitAssert")
        .collect();
    overloads.sort_by_key(|&i| graph.nodes[i as usize].span);
    assert_eq!(overloads.len(), 2, "both overloads are nodes");
    let caller = node(&graph, "ViaType");
    assert!(
        graph.edges.iter().any(|e| e.rel_type == RelType::Calls
            && e.source == caller
            && e.target == overloads[0]),
        "`ViaType` binds the first overload"
    );
    assert!(
        graph.edges.iter().any(|e| e.rel_type == RelType::References
            && e.source == overloads[0]
            && e.target == overloads[1]
            && e.reason.resolve(&graph.string_pool) == "overload_set"),
        "the first overload references the later one"
    );
}

#[test]
fn test_resolve_call_csharp_using_alias_receiver_binds_aliased_type() {
    assert_binds(
        &build(&[ASSERTS, CALLERS]),
        "ViaAlias",
        "AreEqual",
        "XUnitAssert",
    );
}

/// A property receiver keeps the bare member: `Run` is the only one.
#[test]
fn test_resolve_call_csharp_property_receiver_binds_bare_member() {
    assert_binds(&build(&[ASSERTS, CALLERS]), "ViaProperty", "Run", "Helper");
}

/// C# looks a simple name up as a member before a type: a property named
/// like a type with its own `Run` still calls through the property.
#[test]
fn test_resolve_call_csharp_property_named_like_type_binds_property_member() {
    let tool = (
        "Lib/Tool.cs",
        "namespace Lib { public class Tool { public void Run() { } } }\n",
    );
    let graph = build(&[ASSERTS, CALLERS, tool]);
    assert_never_binds_other(&graph, "ViaProperty", "Run", "Helper");
}

#[test]
fn test_resolve_call_csharp_typed_local_receiver_binds_declared_type() {
    assert_binds(&build(&[ASSERTS, CALLERS]), "ViaLocal", "Go", "Service");
}

/// A local typed with a library type stays unresolved, as before: it never
/// falls back to a project member of the same name.
#[test]
fn test_resolve_call_csharp_library_typed_local_binds_nothing() {
    let caller = (
        "App/Net.cs",
        r#"namespace App
{
    public class Sender { public void Send() { } }
    public class Net
    {
        public void ViaClient() { HttpClient client = null; client.Send(); }
    }
}
"#,
    );
    let graph = build(&[caller]);
    assert!(
        calls_from(&graph, "ViaClient").is_empty(),
        "`HttpClient.Send` names a library type"
    );
}

/// A local wins over a type of the same name, typed or not.
#[test]
fn test_resolve_call_csharp_local_named_like_type_binds_local_member() {
    let graph = build(&[ASSERTS, CALLERS]);
    assert_binds(&graph, "ViaTypedLocalShadow", "Run", "Helper");
    assert_binds(&graph, "ViaVarLocalShadow", "Run", "Helper");
}

/// The contract this pins: a receiver that names no project type keeps the
/// bare-member resolution it had before. `WriteLine` is the project's only
/// one, so that resolution binds it (a guess the global tier allows).
#[test]
fn test_resolve_call_csharp_unknown_type_receiver_keeps_bare_member() {
    let caller = (
        "App/Printer.cs",
        r#"namespace App
{
    public class Printer
    {
        public void WriteLine(string s) { }
    }
    public class Log
    {
        public void ViaConsole() { Console.WriteLine("x"); }
    }
}
"#,
    );
    assert_binds(&build(&[caller]), "ViaConsole", "WriteLine", "Printer");
}

/// An alias to a type the project does not hold names a library type: no
/// edge, though a project type declares the member.
#[test]
fn test_resolve_call_csharp_alias_to_library_type_binds_nothing() {
    let caller = (
        "App/Checks.cs",
        r#"using XAssert = Xunit.Assert;
using Assert = Other.XUnitAssert;

namespace App
{
    public class Comparer { public static void Equal(object a, object b) { } }
    public class Checks
    {
        public void ViaLibraryAlias() { XAssert.Equal(1, 2); }
        public void ViaOtherNamespaceAlias() { Assert.AreEqual(1, 2); }
    }
}
"#,
    );
    let graph = build(&[ASSERTS, caller]);
    assert!(
        calls_from(&graph, "ViaLibraryAlias").is_empty(),
        "`Xunit.Assert` is not `Comparer`"
    );
    assert!(
        calls_from(&graph, "ViaOtherNamespaceAlias").is_empty(),
        "`Other.XUnitAssert` is not `Ns.Tests.XUnitAssert`"
    );
}

/// Receivers other than a simple identifier keep the bare member: `Ping`
/// and `Make` are unique, and the three `AreEqual` owners leave a
/// `global::` path without an edge to a wrong owner.
#[test]
fn test_resolve_call_csharp_non_identifier_receivers_keep_bare_member() {
    let graph = build(&[ASSERTS, CALLERS]);
    assert_binds(&graph, "ViaNested", "Ping", "Inner");
    assert_binds(&graph, "ViaGeneric", "Make", "Box");
    assert_never_binds_other(&graph, "ViaGlobal", "AreEqual", "StringAssert");
}

#[test]
fn test_resolve_call_csharp_nameof_binds_nothing() {
    assert!(calls_from(&build(&[ASSERTS, CALLERS]), "ViaNameof").is_empty());
}

/// `using static` brings every `AreEqual` overload of `XUnitAssert` into
/// scope; the bare call must never land on another class's `AreEqual`.
#[test]
fn test_resolve_call_csharp_using_static_bare_call_never_binds_other_type() {
    let caller = (
        "Tests/Static.cs",
        r#"using static Ns.Tests.XUnitAssert;

namespace Ns.Tests
{
    public class Static
    {
        public void ViaUsingStatic() { AreEqual(1, 2); }
    }
}
"#,
    );
    let graph = build(&[ASSERTS, caller]);
    assert_never_binds_other(&graph, "ViaUsingStatic", "AreEqual", "XUnitAssert");
}
