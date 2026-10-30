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

/// Joins source lines; the tests below read as the file they stand for.
fn lines(src: &[&str]) -> Vec<u8> {
    let mut joined = src.join("\n");
    joined.push('\n');
    joined.into_bytes()
}

fn node_row<'g>(g: &'g LocalGraph, name: &str) -> Row<'g> {
    *rows(g)
        .iter()
        .find(|row| row.0 == name)
        .unwrap_or_else(|| panic!("no node `{name}` in {:?}", rows(g)))
}

/// `(name, owner_class)` of every node that is no namespace or type.
fn member_owners(g: &LocalGraph) -> Vec<(&str, Option<&str>)> {
    g.nodes
        .iter()
        .filter(|n| {
            !matches!(
                n.kind,
                NodeKind::Namespace | NodeKind::Class | NodeKind::Struct | NodeKind::Interface
            )
        })
        .map(|n| (n.name.as_str(), n.owner_class.as_deref()))
        .collect()
}

// Guard for FU-2026-10-30 round 2, item C1. The `#if` that splits the base
// list of Newtonsoft.Json `Linq/JValue.cs:46-49` is not what truncates that
// class: tree-sitter keeps the span and the members' owner across it. The
// brace-splitting `#if` groups further down the file do (see the tests after
// the guards); this shape must stay as it is once those are patched.
#[test]
fn test_csharp_if_endif_in_base_list_keeps_class_span_and_member_owner() {
    let g = parse(&lines(&[
        "namespace N",
        "{",
        "    public partial class JValue : JToken, IEquatable<JValue>",
        "#if HAVE_ICONVERTIBLE",
        "        , IConvertible",
        "#endif",
        "    {",
        "        private int _a;",
        "        internal JValue() { }",
        "        public void M() { }",
        "    }",
        "}",
    ]));
    assert_eq!(
        node_row(&g, "N"),
        ("N", NodeKind::Namespace, (0, 0, 11, 1), None)
    );
    assert_eq!(
        node_row(&g, "JValue"),
        ("JValue", NodeKind::Class, (2, 4, 10, 5), Some("N"))
    );
    assert_eq!(
        member_owners(&g),
        [
            ("_a", Some("JValue")),
            ("JValue", Some("JValue")),
            ("M", Some("JValue")),
        ]
    );
}

// C1 guard: `#if` / `#else` / `#endif` with a base in each branch.
#[test]
fn test_csharp_if_else_endif_in_base_list_keeps_class_span_and_member_owner() {
    let g = parse(&lines(&[
        "class A : B",
        "#if X",
        "    , IX",
        "#else",
        "    , IY",
        "#endif",
        "{",
        "    void M() { }",
        "}",
    ]));
    assert_eq!(
        rows(&g),
        [
            ("A", NodeKind::Class, (0, 0, 8, 1), None),
            ("M", NodeKind::Method, (7, 4, 7, 16), Some("A")),
        ]
    );
}

// C1 guard: the directive wraps the whole base list, colon included.
#[test]
fn test_csharp_if_around_whole_base_list_keeps_class_span_and_member_owner() {
    let g = parse(&lines(&[
        "class A",
        "#if X",
        "    : B",
        "#endif",
        "{",
        "    void M() { }",
        "}",
    ]));
    assert_eq!(
        rows(&g),
        [
            ("A", NodeKind::Class, (0, 0, 6, 1), None),
            ("M", NodeKind::Method, (5, 4, 5, 16), Some("A")),
        ]
    );
}

// C1 guard: two split classes side by side; each owns its own member.
#[test]
fn test_csharp_two_classes_with_split_base_lists_own_their_members() {
    let g = parse(&lines(&[
        "namespace N",
        "{",
        "    class A : B",
        "#if X",
        "        , IX",
        "#endif",
        "    {",
        "        void M() { }",
        "    }",
        "    class C : D",
        "#if Y",
        "        , IY",
        "#endif",
        "    {",
        "        void N() { }",
        "    }",
        "}",
    ]));
    let ns = Some("N");
    assert_eq!(
        rows(&g),
        [
            ("N", NodeKind::Namespace, (0, 0, 16, 1), None),
            ("A", NodeKind::Class, (2, 4, 8, 5), ns),
            ("M", NodeKind::Method, (7, 8, 7, 20), Some("A")),
            ("C", NodeKind::Class, (9, 4, 15, 5), ns),
            ("N", NodeKind::Method, (14, 8, 14, 20), Some("C")),
        ]
    );
}

// C1 guard: the split class is nested one level deeper; the outer class
// keeps the members after it.
#[test]
fn test_csharp_nested_class_with_split_base_list_owns_its_members() {
    let g = parse(&lines(&[
        "class Outer",
        "{",
        "    class Inner : B",
        "#if X",
        "        , IX",
        "#endif",
        "    {",
        "        void M() { }",
        "    }",
        "    void O() { }",
        "}",
    ]));
    assert_eq!(
        rows(&g),
        [
            ("Outer", NodeKind::Class, (0, 0, 10, 1), None),
            ("Inner", NodeKind::Class, (2, 4, 8, 5), Some("Outer")),
            ("M", NodeKind::Method, (7, 8, 7, 20), Some("Inner")),
            ("O", NodeKind::Method, (9, 4, 9, 16), Some("Outer")),
        ]
    );
}

// C1 guard: struct and interface base lists split the same way.
#[test]
fn test_csharp_struct_and_interface_split_base_lists_keep_span_and_owner() {
    let g = parse(&lines(&[
        "struct S : IA",
        "#if X",
        "    , IB",
        "#endif",
        "{",
        "    void M() { }",
        "}",
        "interface I : IA",
        "#if X",
        "    , IB",
        "#endif",
        "{",
        "    void N();",
        "}",
    ]));
    assert_eq!(
        node_row(&g, "S"),
        ("S", NodeKind::Struct, (0, 0, 6, 1), None)
    );
    assert_eq!(
        node_row(&g, "I"),
        ("I", NodeKind::Interface, (7, 0, 13, 1), None)
    );
    assert_eq!(member_owners(&g), [("M", Some("S")), ("N", Some("I"))]);
}

// C1 guard: a class with no base list keeps its records.
#[test]
fn test_csharp_class_without_base_list_records_unchanged() {
    let g = parse(&lines(&["class A", "{", "    void M() { }", "}"]));
    assert_eq!(
        rows(&g),
        [
            ("A", NodeKind::Class, (0, 0, 3, 1), None),
            ("M", NodeKind::Method, (2, 4, 2, 16), Some("A")),
        ]
    );
}

// C1 guard: a directive inside a method body is no part of a base list.
#[test]
fn test_csharp_directive_in_method_body_records_unchanged() {
    let g = parse(&lines(&[
        "class A : B",
        "{",
        "    void M()",
        "    {",
        "#if X",
        "        Foo();",
        "#endif",
        "    }",
        "    void N() { }",
        "}",
    ]));
    assert_eq!(
        rows(&g),
        [
            ("A", NodeKind::Class, (0, 0, 9, 1), None),
            ("M", NodeKind::Method, (2, 4, 7, 5), Some("A")),
            ("N", NodeKind::Method, (8, 4, 8, 16), Some("A")),
        ]
    );
}

// C1 guard: a conditional `using` ahead of a class with an intact base list.
#[test]
fn test_csharp_directive_before_class_with_intact_base_list_records_unchanged() {
    let g = parse(&lines(&[
        "#if X",
        "using System.Numerics;",
        "#endif",
        "class A : B",
        "{",
        "    void M() { }",
        "}",
    ]));
    assert_eq!(
        rows(&g),
        [
            ("A", NodeKind::Class, (3, 0, 6, 1), None),
            ("M", NodeKind::Method, (5, 4, 5, 16), Some("A")),
        ]
    );
}

// C1 guard: an empty file has no records.
#[test]
fn test_csharp_empty_file_has_no_records() {
    assert!(rows(&parse(b"")).is_empty());
}

/// `(name, kind, span, owner_class)` of every namespace, type and method.
fn shape_rows(g: &LocalGraph) -> Vec<Row<'_>> {
    rows(g)
        .into_iter()
        .filter(|row| {
            matches!(
                row.1,
                NodeKind::Namespace | NodeKind::Class | NodeKind::Method
            )
        })
        .collect()
}

// FU-2026-10-30 round 2, item C1 (real cause). A conditional group whose
// branches leave different brace depths cuts a method body apart:
// `if (a)` / `{` in the `#if` branch, an unrelated statement in the `#else`
// branch, the closing `}` outside. Error recovery then ends the class at the
// method and `After` takes the file's namespace or nothing as owner. The
// group reads as its first branch, as if only that symbol were defined.
#[test]
fn test_csharp_if_group_opening_a_brace_in_one_branch_keeps_class_span() {
    let g = parse(&lines(&[
        "class C",
        "{",
        "    void M(bool a)",
        "    {",
        "#if X",
        "        if (a)",
        "{",
        "#else",
        "        Alt();",
        "#endif",
        "            One();",
        "        }",
        "    }",
        "",
        "    void After() { }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&g),
        [
            ("C", NodeKind::Class, (0, 0, 15, 1), None),
            ("M", NodeKind::Method, (2, 4, 12, 5), Some("C")),
            ("After", NodeKind::Method, (14, 4, 14, 20), Some("C")),
        ]
    );
}

// C1 shape from Newtonsoft.Json `Linq/JValue.cs:328-362` (the `Date` arm of
// `Compare`): three groups interlock, an `#if` / `#else` that opens a brace
// in one branch, and an `#if` that closes it again and continues with
// `else {`. The class, its namespace and the method after it keep their
// whole spans.
#[test]
fn test_csharp_interlocking_if_groups_in_switch_arm_keep_class_span_and_owner() {
    let g = parse(&lines(&[
        "namespace N",
        "{",
        "    public class C",
        "    {",
        "        public int Compare(object a, object b)",
        "        {",
        "            switch (kind)",
        "            {",
        "                case Kind.Date:",
        "#if HAS_OFFSET",
        "                    if (a is DateTime dateA)",
        "{",
        "#else",
        "                    DateTime dateA = (DateTime)a;",
        "#endif",
        "                        DateTime dateB;",
        "                        return dateA.CompareTo(dateB);",
        "#if HAS_OFFSET",
        "                    }",
        "                    else",
        "{",
        "                        return 0;",
        "                    }",
        "#endif",
        "                default:",
        "                    return 1;",
        "            }",
        "        }",
        "",
        "        public void After() { }",
        "    }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&g),
        [
            ("N", NodeKind::Namespace, (0, 0, 31, 1), None),
            ("C", NodeKind::Class, (2, 4, 30, 5), Some("N")),
            ("Compare", NodeKind::Method, (4, 8, 27, 9), Some("C")),
            ("After", NodeKind::Method, (29, 8, 29, 31), Some("C")),
        ]
    );
}

// C1 atypical: two such methods side by side, each with its own symbol.
#[test]
fn test_csharp_two_brace_splitting_if_groups_keep_every_method_and_owner() {
    let g = parse(&lines(&[
        "class C",
        "{",
        "    void M(bool a)",
        "    {",
        "#if X",
        "        if (a)",
        "{",
        "#else",
        "        Alt();",
        "#endif",
        "            One();",
        "        }",
        "    }",
        "",
        "    void N(bool a)",
        "    {",
        "#if Y",
        "        if (a)",
        "{",
        "#else",
        "        Alt();",
        "#endif",
        "            Two();",
        "        }",
        "    }",
        "",
        "    void After() { }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&g),
        [
            ("C", NodeKind::Class, (0, 0, 27, 1), None),
            ("M", NodeKind::Method, (2, 4, 12, 5), Some("C")),
            ("N", NodeKind::Method, (14, 4, 24, 5), Some("C")),
            ("After", NodeKind::Method, (26, 4, 26, 20), Some("C")),
        ]
    );
}

// C1 atypical: CRLF line ends. The blanked directive lines keep their breaks.
#[test]
fn test_csharp_brace_splitting_if_group_with_crlf_keeps_class_span() {
    let text = "class C\r\n{\r\n    void M(bool a)\r\n    {\r\n#if X\r\n        if (a)\r\n{\r\n#else\r\n        Alt();\r\n#endif\r\n            One();\r\n        }\r\n    }\r\n\r\n    void After() { }\r\n}\r\n";
    let g = parse(text.as_bytes());
    assert_eq!(
        shape_rows(&g),
        [
            ("C", NodeKind::Class, (0, 0, 15, 1), None),
            ("M", NodeKind::Method, (2, 4, 12, 5), Some("C")),
            ("After", NodeKind::Method, (14, 4, 14, 20), Some("C")),
        ]
    );
}

// C1 guard: a group that ends in `else` and a chain of `else if` split by an
// `#if` keep every method record.
#[test]
fn test_csharp_if_group_around_else_keeps_method_records() {
    let dangling = parse(&lines(&[
        "class C",
        "{",
        "    void M(bool a, bool b)",
        "    {",
        "#if X",
        "        if (a)",
        "        {",
        "            One();",
        "        }",
        "        else",
        "#endif",
        "        {",
        "            Two();",
        "        }",
        "    }",
        "",
        "    void After() { }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&dangling),
        [
            ("C", NodeKind::Class, (0, 0, 17, 1), None),
            ("M", NodeKind::Method, (2, 4, 14, 5), Some("C")),
            ("After", NodeKind::Method, (16, 4, 16, 20), Some("C")),
        ]
    );
    let chain = parse(&lines(&[
        "class C",
        "{",
        "    void M(bool a, bool b)",
        "    {",
        "        if (a)",
        "        {",
        "            One();",
        "        }",
        "#if X",
        "        else if (b)",
        "        {",
        "            Two();",
        "        }",
        "#endif",
        "        else",
        "        {",
        "            Three();",
        "        }",
        "    }",
        "",
        "    void After() { }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&chain),
        [
            ("C", NodeKind::Class, (0, 0, 21, 1), None),
            ("M", NodeKind::Method, (2, 4, 18, 5), Some("C")),
            ("After", NodeKind::Method, (20, 4, 20, 20), Some("C")),
        ]
    );
}

// C1 guard: branches that leave the same brace depth keep both branches'
// members: neither `A` nor `B` is blanked.
#[test]
fn test_csharp_if_else_group_with_balanced_branches_keeps_both_branches() {
    let g = parse(&lines(&[
        "class C",
        "{",
        "#if X",
        "    void A() { }",
        "#else",
        "    void B() { }",
        "#endif",
        "",
        "    void After() { }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&g),
        [
            ("C", NodeKind::Class, (0, 0, 9, 1), None),
            ("A", NodeKind::Method, (3, 4, 3, 16), Some("C")),
            ("B", NodeKind::Method, (5, 4, 5, 16), Some("C")),
            ("After", NodeKind::Method, (8, 4, 8, 20), Some("C")),
        ]
    );
}

// C1 guard: a brace inside a string or char literal is no brace. `A` has
// a `"{"` and `B` a `@"}"`; counted as braces the branches would differ and
// `B` would be blanked.
#[test]
fn test_csharp_braces_in_string_literals_do_not_split_if_group() {
    let g = parse(&lines(&[
        "class C",
        "{",
        "#if X",
        "    void A() { Foo(\"{\"); }",
        "#else",
        "    void B() { Foo(@\"}\"); }",
        "#endif",
        "",
        "    void After() { }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&g),
        [
            ("C", NodeKind::Class, (0, 0, 9, 1), None),
            ("A", NodeKind::Method, (3, 4, 3, 26), Some("C")),
            ("B", NodeKind::Method, (5, 4, 5, 27), Some("C")),
            ("After", NodeKind::Method, (8, 4, 8, 20), Some("C")),
        ]
    );
}

/// A brace in a string inside an interpolation hole is no brace: the group
/// stays whole and the members of both branches keep their records.
#[test]
fn test_csharp_interpolated_hole_brace_keeps_both_branches_members() {
    let g = parse(&lines(&[
        "class C",
        "{",
        "#if X",
        r#"    string A() => $"{F("}")}";"#,
        "#else",
        "    void B() { }",
        "#endif",
        "    string F(string s) => s;",
        "}",
    ]));
    assert_eq!(
        shape_rows(&g),
        [
            ("C", NodeKind::Class, (0, 0, 8, 1), None),
            ("A", NodeKind::Method, (3, 4, 3, 30), Some("C")),
            ("B", NodeKind::Method, (5, 4, 5, 16), Some("C")),
            ("F", NodeKind::Method, (7, 4, 7, 28), Some("C")),
        ]
    );
}

/// Two branches that each open the same method body read as the first one:
/// the namespace, the class and the method after it keep spans and owners.
#[test]
fn test_csharp_if_group_with_equal_open_branches_keeps_later_member_owner() {
    let g = parse(&lines(&[
        "namespace N",
        "{",
        "    class A",
        "    {",
        "#if NET6",
        "        public void Foo(int a) {",
        "#else",
        "        public void Foo(string a) {",
        "#endif",
        "            Bar();",
        "        }",
        "        public void Later() { }",
        "    }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&g),
        [
            ("N", NodeKind::Namespace, (0, 0, 13, 1), None),
            ("A", NodeKind::Class, (2, 4, 12, 5), Some("N")),
            ("Foo", NodeKind::Method, (5, 8, 10, 9), Some("A")),
            ("Later", NodeKind::Method, (11, 8, 11, 31), Some("A")),
        ]
    );
}

/// A one-branch group that opens a brace and a later one that closes it
/// are read together: the method after them stays a member of the class.
#[test]
fn test_csharp_open_and_close_if_groups_keep_later_member_owner() {
    let g = parse(&lines(&[
        "namespace N",
        "{",
        "    class A",
        "    {",
        "        public void M()",
        "        {",
        "#if X",
        "            lock (o) {",
        "#endif",
        "                Bar();",
        "#if X",
        "            }",
        "#endif",
        "        }",
        "        public void Later() { }",
        "    }",
        "}",
    ]));
    assert_eq!(
        shape_rows(&g),
        [
            ("N", NodeKind::Namespace, (0, 0, 16, 1), None),
            ("A", NodeKind::Class, (2, 4, 15, 5), Some("N")),
            ("M", NodeKind::Method, (4, 8, 13, 9), Some("A")),
            ("Later", NodeKind::Method, (14, 8, 14, 31), Some("A")),
        ]
    );
}

/// `catch` clauses split by `#if` continue the `try`: the method calls what
/// the kept clauses call, and `catch` / `finally` are never callees or reads.
#[test]
fn test_csharp_if_group_of_catch_clauses_emits_no_catch_callee() {
    let g = parse(&lines(&[
        "class C",
        "{",
        "    void M()",
        "    {",
        "        try { Bar(); }",
        "#if X",
        "        catch (IOException) { Baz(); }",
        "#else",
        "        catch (Exception) { Qux(); }",
        "#endif",
        "        finally { }",
        "    }",
        "}",
    ]));
    let m = g
        .nodes
        .iter()
        .find(|n| n.name == "M")
        .unwrap_or_else(|| panic!("no `M` in {:?}", rows(&g)));
    let mut calls: Vec<&str> = m.calls.iter().map(String::as_str).collect();
    calls.sort_unstable();
    assert_eq!(calls, ["Bar", "Baz"]);
    assert!(
        !m.field_reads
            .iter()
            .any(|r| r.ends_with(" catch") || r.ends_with(" finally")),
        "{:?}",
        m.field_reads
    );
}
