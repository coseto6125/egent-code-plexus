//! `X: TypeAlias = …` (PEP 613) must be classified as Typedef, not Variable —
//! it is a reference target for `ecp find`/impact, not a value binding. Plain
//! and other-annotated module assignments keep the Variable kind.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PythonProvider::new().expect("provider");
    p.parse_file(Path::new("m.py"), src.as_bytes())
        .expect("parse")
}

fn kind_of(g: &LocalGraph, name: &str) -> Option<NodeKind> {
    g.nodes.iter().find(|n| n.name == name).map(|n| n.kind)
}

#[test]
fn bare_typealias_annotation_is_typedef() {
    let g = parse("from typing import TypeAlias\nVector: TypeAlias = list[float]\n");
    assert_eq!(
        kind_of(&g, "Vector"),
        Some(NodeKind::Typedef),
        "TypeAlias-annotated assignment must be Typedef, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn dotted_typealias_annotation_is_typedef() {
    let g = parse("import typing\nMatrix: typing.TypeAlias = list[list[float]]\n");
    assert_eq!(
        kind_of(&g, "Matrix"),
        Some(NodeKind::Typedef),
        "typing.TypeAlias-annotated assignment must be Typedef, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn plain_assignment_stays_variable() {
    let g = parse("count = 5\n");
    assert_eq!(
        kind_of(&g, "count"),
        Some(NodeKind::Variable),
        "plain assignment must stay Variable, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn non_typealias_annotation_stays_variable() {
    let g = parse("count: int = 5\n");
    assert_eq!(
        kind_of(&g, "count"),
        Some(NodeKind::Variable),
        "int-annotated assignment must stay Variable, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn typealias_does_not_double_emit() {
    let g = parse("from typing import TypeAlias\nVector: TypeAlias = list[float]\n");
    let count = g.nodes.iter().filter(|n| n.name == "Vector").count();
    assert_eq!(
        count, 1,
        "Vector must emit exactly one node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn test_parse_pep695_type_statement_returns_typedef() {
    let g = parse("type Al = tuple[int, int]\n");
    assert_eq!(kind_of(&g, "Al"), Some(NodeKind::Typedef), "{:?}", g.nodes);
    assert_eq!(g.nodes.iter().filter(|n| n.name == "Al").count(), 1);
}

#[test]
fn test_parse_bare_generic_subscript_returns_typedef() {
    let g = parse("from typing import Tuple\nAl = Tuple[int, int]\n");
    assert_eq!(kind_of(&g, "Al"), Some(NodeKind::Typedef), "{:?}", g.nodes);
}

#[test]
fn test_parse_value_assignments_stay_variable() {
    for src in [
        "X = 3\n",
        "X = foo()\n",
        "X = [1, 2]\n",
        "X = READ | WRITE\n",
    ] {
        assert_eq!(kind_of(&parse(src), "X"), Some(NodeKind::Variable), "{src}");
    }
}

#[test]
fn test_parse_attribute_flag_union_stays_variable() {
    let g = parse(
        "ITEM_FLAGS = Qt.ItemIsEnabled | Qt.ItemIsSelectable\n\
         Read = 1\nWrite = 2\nMask = Read | Write\n",
    );
    for name in ["ITEM_FLAGS", "Mask"] {
        assert_eq!(kind_of(&g, name), Some(NodeKind::Variable), "{name}");
    }
}

#[test]
fn test_parse_generic_name_rebound_as_value_stays_variable() {
    let g = parse("List = [42]\nAlias = List[0]\n");
    assert_eq!(
        kind_of(&g, "Alias"),
        Some(NodeKind::Variable),
        "{:?}",
        g.nodes
    );
}

#[test]
fn test_parse_real_aliases_are_typedef() {
    for (src, name) in [
        ("type Al = tuple[int, int]\n", "Al"),
        ("from typing import Optional\nAl = Optional[int]\n", "Al"),
        ("Al = int | None\n", "Al"),
        ("IntOrStr = int | str\n", "IntOrStr"),
        ("class Foo: ...\nAl = Foo | None\n", "Al"),
        ("from typing import Dict\nAl = Dict[str, int]\n", "Al"),
    ] {
        assert_eq!(kind_of(&parse(src), name), Some(NodeKind::Typedef), "{src}");
    }
}

#[test]
fn test_parse_shadowed_builtin_union_stays_variable() {
    let g = parse("int = 1\nstr = 2\nMask = int | str\n");
    assert_eq!(
        kind_of(&g, "Mask"),
        Some(NodeKind::Variable),
        "{:?}",
        g.nodes
    );
}

#[test]
fn test_parse_module_class_operands_are_typedef() {
    for (src, name) in [
        (
            "class Circle: ...\nclass Square: ...\nShape = Circle | Square\n",
            "Shape",
        ),
        (
            "class Sequence(Generic[T]): ...\nInts = Sequence[int]\n",
            "Ints",
        ),
        ("@dec\nclass Box: ...\nBoxes = Box | None\n", "Boxes"),
    ] {
        assert_eq!(kind_of(&parse(src), name), Some(NodeKind::Typedef), "{src}");
    }
}

#[test]
fn test_parse_undefined_names_union_stays_variable() {
    let g = parse("Alpha | Beta\nX = Alpha | Beta\n");
    assert_eq!(kind_of(&g, "X"), Some(NodeKind::Variable), "{:?}", g.nodes);
}

#[test]
fn test_parse_class_rebound_as_value_is_not_evidence() {
    let g = parse("class Foo: ...\nFoo = 3\nAl = Foo | Foo\n");
    assert_eq!(kind_of(&g, "Al"), Some(NodeKind::Variable), "{:?}", g.nodes);
}

#[test]
fn test_parse_many_generic_aliases_all_typedef() {
    let mut src = String::from("from typing import List\n");
    for i in 0..2000 {
        src.push_str(&format!("X{i} = List[int]\n"));
    }
    let g = parse(&src);
    assert_eq!(kind_of(&g, "X1999"), Some(NodeKind::Typedef));
    assert_eq!(kind_of(&g, "X0"), Some(NodeKind::Typedef));
}
