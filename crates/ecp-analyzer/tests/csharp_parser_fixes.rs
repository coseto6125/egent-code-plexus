//! `CSharpProvider` defects found in the driver-move review
//! (FU-2026-10-30-2f3e400dd5f2). Each test asserts the correct records, so
//! it fails until the fix lands.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

type Span = (u32, u32, u32, u32);

/// `(name, kind, span, owner_class)` of each node, in emitted order.
type Row<'g> = (&'g str, NodeKind, Span, Option<&'g str>);

fn parse(src: &[u8]) -> LocalGraph {
    CSharpProvider::new()
        .expect("CSharpProvider::new")
        .parse_file(Path::new("A.cs"), src)
        .expect("parse_file")
}

fn rows(g: &LocalGraph) -> Vec<Row<'_>> {
    g.nodes
        .iter()
        .map(|n| (n.name.as_str(), n.kind, n.span, n.owner_class.as_deref()))
        .collect()
}

/// `(source, imported_name, alias)`
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

fn exported<'g>(g: &'g LocalGraph, names: &[&str]) -> Vec<(&'g str, bool)> {
    names
        .iter()
        .map(|name| {
            let n = g
                .nodes
                .iter()
                .find(|n| n.name == *name)
                .unwrap_or_else(|| panic!("no node `{name}` in {:?}", rows(g)));
            (n.name.as_str(), n.is_exported)
        })
        .collect()
}

/// A `using` alias is one import: the target with the alias. Neither the
/// alias name nor the bare target is an import of its own. The plain,
/// `static` and `global` forms keep their one record.
#[test]
fn test_csharp_using_alias_emits_one_import_with_alias() {
    let g = parse(
        b"using System;\nusing Json = Newtonsoft.Json.JsonConvert;\nusing static System.Math;\nusing Dict = System.Collections.Generic.Dictionary<string, int>;\nusing N = Newtonsoft;\nglobal using System.Linq;\n",
    );
    assert!(g.nodes.is_empty(), "{:?}", rows(&g));
    assert_eq!(
        imports(&g),
        [
            ("System", "System", None),
            (
                "Newtonsoft.Json.JsonConvert",
                "Newtonsoft.Json.JsonConvert",
                Some("Json")
            ),
            ("System.Math", "System.Math", None),
            (
                "System.Collections.Generic.Dictionary<string, int>",
                "System.Collections.Generic.Dictionary<string, int>",
                Some("Dict")
            ),
            ("Newtonsoft", "Newtonsoft", Some("N")),
            ("System.Linq", "System.Linq", None),
        ]
    );
}

/// A file-scoped namespace covers the rest of the compilation unit, so it
/// spans to the end of the last declaration and owns every top-level type,
/// as a block namespace does. Nested members keep their class.
#[test]
fn test_csharp_file_scoped_namespace_spans_file_and_owns_types() {
    let g = parse(
        b"namespace App.Core;\n\npublic class Dog\n{\n    public void Bark() { }\n    public class Pup { }\n}\n\ninternal interface IPet { }\n",
    );
    let ns = Some("App.Core");
    assert_eq!(
        rows(&g),
        [
            ("App.Core", NodeKind::Namespace, (0, 0, 8, 27), None),
            ("Dog", NodeKind::Class, (2, 0, 6, 1), ns),
            ("Bark", NodeKind::Method, (4, 4, 4, 26), Some("Dog")),
            ("Pup", NodeKind::Class, (5, 4, 5, 24), Some("Dog")),
            ("IPet", NodeKind::Interface, (8, 0, 8, 27), ns),
        ]
    );
}

/// Atypical: `using` lines before the namespace stay outside it; the span
/// starts at the `namespace` keyword.
#[test]
fn test_csharp_file_scoped_namespace_after_usings_starts_at_keyword() {
    let g = parse(b"using System;\nnamespace App.Core;\npublic class Dog { }\n");
    assert_eq!(
        rows(&g),
        [
            ("App.Core", NodeKind::Namespace, (1, 0, 2, 20), None),
            ("Dog", NodeKind::Class, (2, 0, 2, 20), Some("App.Core")),
        ]
    );
    assert_eq!(imports(&g), [("System", "System", None)]);
}

/// Guards: a file-scoped namespace with no declaration keeps its own line,
/// and a block namespace keeps today's records.
#[test]
fn test_csharp_empty_file_scoped_and_block_namespace_records_unchanged() {
    let g = parse(b"namespace App.Core;\n");
    assert_eq!(
        rows(&g),
        [("App.Core", NodeKind::Namespace, (0, 0, 0, 19), None)]
    );

    let g = parse(
        b"namespace App.Core\n{\n    public class Dog\n    {\n        public void Bark() { }\n    }\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("App.Core", NodeKind::Namespace, (0, 0, 6, 1), None),
            ("Dog", NodeKind::Class, (2, 4, 5, 5), Some("App.Core")),
            ("Bark", NodeKind::Method, (4, 8, 4, 30), Some("Dog")),
        ]
    );
}

/// A declaration is exported when code outside its assembly can reach it:
/// `public`, `protected` and `protected internal`. `internal`,
/// `private protected`, `private` and no modifier stay inside (Java exports
/// `protected` too; Kotlin keeps `internal` unexported).
#[test]
fn test_csharp_protected_members_export_internal_and_private_do_not() {
    let g = parse(
        b"namespace App\n{\n    public class Pub\n    {\n        public void A() { }\n        internal void B() { }\n        protected void C() { }\n        protected internal void D() { }\n        private void E() { }\n        void F() { }\n        private protected void G() { }\n    }\n    internal class Inner { }\n    class Bare { }\n}\n",
    );
    assert_eq!(
        exported(
            &g,
            &["App", "Pub", "A", "B", "C", "D", "E", "F", "G", "Inner", "Bare"]
        ),
        [
            ("App", false),
            ("Pub", true),
            ("A", true),
            ("B", false),
            ("C", true),
            ("D", true),
            ("E", false),
            ("F", false),
            ("G", false),
            ("Inner", false),
            ("Bare", false),
        ]
    );
}

/// Atypical: an interface member with no modifier is implicitly `public`,
/// so it is exported with its public interface.
#[test]
fn test_csharp_interface_member_without_modifier_is_exported() {
    let g = parse(b"namespace App\n{\n    public interface IApi { void Run(); }\n}\n");
    assert_eq!(
        exported(&g, &["IApi", "Run"]),
        [("IApi", true), ("Run", true)]
    );
}

/// A field declarator carries its declaration's type, as a property does;
/// two declarators share it.
#[test]
fn test_csharp_field_declarators_carry_declared_type() {
    let g = parse(
        b"class Dog\n{\n    private int a, b;\n    public static readonly List<string> Names = new();\n    public string Name { get; set; }\n}\n",
    );
    let types: Vec<(&str, Option<&str>)> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property)
        .map(|n| (n.name.as_str(), n.type_annotation.as_deref()))
        .collect();
    assert_eq!(
        types,
        [
            ("a", Some("int")),
            ("b", Some("int")),
            ("Names", Some("List<string>")),
            ("Name", Some("string")),
        ]
    );
}
