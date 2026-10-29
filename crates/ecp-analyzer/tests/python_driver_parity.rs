//! `PythonProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move; where the order of two records depends on query match
//! order rather than source position, the test compares sorted rows.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawNode, SchemaType};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span4 = (u32, u32, u32, u32);

fn parse_at(path: &str, src: &[u8]) -> LocalGraph {
    PythonProvider::new()
        .expect("PythonProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn parse(src: &[u8]) -> LocalGraph {
    parse_at("a.py", src)
}

fn node<'g>(g: &'g LocalGraph, name: &str) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node `{name}` in {:?}", g.nodes))
}

/// `(callable name, framework)` of each tx scope, in emitted order.
fn tx(g: &LocalGraph) -> Vec<(&str, FrameworkId)> {
    g.tx_scopes
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| (g.nodes[s.node_idx() as usize].name.as_str(), s.framework()))
        .collect()
}

#[test]
fn test_python_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_eq!(g.file_path, Path::new("a.py"));
    assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
    assert!(g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}

/// The pre-scan collects the imports in source order; the last step rewrites
/// module paths and appends the root binding of the unaliased dotted import.
/// A relative source keeps its dots; an aliased dotted import binds no root.
#[test]
fn test_python_driver_imports_only_file_rewrites_sources_and_appends_root() {
    let g = parse(b"from . import x\nimport a.b as c\nimport os.path\nfrom pkg.mod import *\n");
    assert!(g.nodes.is_empty(), "{:?}", g.nodes);
    let imports: Vec<(&str, &str, Option<&str>)> = g
        .imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        imports,
        [
            (".", "x", None),
            ("a/b", "*", Some("c")),
            ("os/path", "*", Some("os.path")),
            ("pkg/mod", "*", None),
            ("os", "*", Some("os")),
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
}

/// The schema re-query gates on the pre-scan imports and reads both configs.
#[test]
fn test_python_driver_pydantic_model_emits_schema_fields() {
    let g = parse(
        b"from pydantic import BaseModel\n\nclass User(BaseModel):\n    name: str\n    age: int\n",
    );
    let user = node(&g, "User");
    assert_eq!(user.kind, NodeKind::Class);
    assert_eq!(user.heritage, ["BaseModel"]);
    let fields = g.schema_fields.as_deref().expect("schema_fields");
    let mut rows: Vec<(&str, SchemaType, &str, FrameworkId)> = fields
        .iter()
        .map(|f| (&*f.name, f.type_class, &*f.owner_class, f.framework))
        .collect();
    rows.sort_by_key(|r| r.0);
    assert_eq!(
        rows,
        [
            ("age", SchemaType::Int, "User", FrameworkId::Pydantic),
            ("name", SchemaType::String, "User", FrameworkId::Pydantic),
        ]
    );
    assert!(fields.iter().all(|f| f.endpoint.is_none()));
}

/// A decorated function binds its own tx site; the decorator text is kept.
#[test]
fn test_python_driver_transaction_atomic_function_binds_tx_scope() {
    let g = parse(b"from django.db import transaction\n\n@transaction.atomic\ndef place_order():\n    pass\n\ndef list_orders():\n    pass\n");
    assert_eq!(tx(&g), [("place_order", FrameworkId::DjangoAtomic)]);
    let f = node(&g, "place_order");
    assert_eq!(f.kind, NodeKind::Function);
    assert_eq!(f.decorators, ["transaction.atomic"]);
    assert_eq!(f.span, (3, 0, 4, 8));
}

/// The tx step runs after the enum pass retags `RED` and `GREEN`; each site
/// still binds its innermost callable, the method and the def nested in it.
#[test]
fn test_python_driver_enum_class_nested_decorated_def_binds_innermost() {
    let src: &[u8] = b"from enum import Enum
from django.db import transaction


class Color(Enum):
    RED = 1
    GREEN = 2

    @transaction.atomic
    def save(self):
        @transaction.atomic
        def inner():
            pass

        return inner
";
    let g = parse(src);
    assert_eq!(
        tx(&g),
        [
            ("save", FrameworkId::DjangoAtomic),
            ("inner", FrameworkId::DjangoAtomic),
        ]
    );
    assert_eq!(node(&g, "Color").kind, NodeKind::Class);
    assert_eq!(node(&g, "save").kind, NodeKind::Method);
    assert_eq!(node(&g, "inner").kind, NodeKind::Function);
    let mut variants: Vec<(&str, Option<&str>)> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .map(|n| (n.name.as_str(), n.owner_class.as_deref()))
        .collect();
    variants.sort();
    assert_eq!(variants, [("GREEN", Some("Color")), ("RED", Some("Color"))]);
    assert!(
        !g.blind_spots
            .iter()
            .any(|b| b.kind == "python-class-as-enum"),
        "{:?}",
        g.blind_spots
    );
}

/// Only an all-marker base list promotes to Interface; a method in the
/// promoted class still binds its tx site.
#[test]
fn test_python_driver_protocol_class_promotes_only_all_marker_bases() {
    let src: &[u8] = b"from typing import Protocol
from pony.orm import db_session


class Greeter(Protocol):
    @db_session
    def greet(self) -> str:
        ...


class Mixed(Protocol, Base):
    pass
";
    let g = parse(src);
    let greeter = node(&g, "Greeter");
    assert_eq!(greeter.kind, NodeKind::Interface);
    assert_eq!(greeter.heritage, ["Protocol"]);
    assert_eq!(node(&g, "Mixed").kind, NodeKind::Class);
    let greet = node(&g, "greet");
    assert_eq!(greet.kind, NodeKind::Method);
    assert_eq!(greet.type_annotation.as_deref(), Some("str"));
    assert_eq!(greet.decorators, ["db_session"]);
    assert_eq!(tx(&g), [("greet", FrameworkId::PonyDbSession)]);
}

/// A lambda argument gets an `<anonymous>` node only when its body calls.
#[test]
fn test_python_driver_lambda_argument_with_call_emits_one_anonymous() {
    let g = parse(b"sorted(xs, key=lambda i: f(i))\nsorted(xs, key=lambda i: i)\n");
    let got: Vec<(&str, NodeKind, Span4, bool)> = g
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.kind, n.span, n.is_exported))
        .collect();
    assert_eq!(
        got,
        [(
            "<anonymous:1:15>",
            NodeKind::Function,
            (0, 15, 0, 29),
            false
        )]
    );
    assert_eq!(g.nodes[0].content_hash, xxh3_64_bytes(b"lambda i: f(i)"));
}

const CLIENT_SRC: &[u8] =
    b"from flask import Flask\n\ndef test_index(client):\n    client.get(\"/x\")\n    eval(code)\n";

/// `test_*.py` under `tests/` is a test path: the `client` receiver route
/// drops and the blind spot carries `is_test`.
#[test]
fn test_python_driver_test_file_path_drops_client_route_and_marks_blind_spot() {
    let g = parse_at("tests/test_views.py", CLIENT_SRC);
    assert!(g.routes.is_empty(), "{:?}", g.routes);
    let blind: Vec<(&str, Span4, bool)> = g
        .blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.span, b.is_test))
        .collect();
    assert_eq!(blind, [("python-eval", (4, 4, 4, 8), true)]);
    assert_eq!(g.blind_spots[0].file_path, Path::new("tests/test_views.py"));
}

/// The same source at a production path keeps the route.
#[test]
fn test_python_driver_source_file_path_keeps_client_route() {
    let g = parse_at("views.py", CLIENT_SRC);
    let routes: Vec<(&str, &str, Option<&str>, Span4)> = g
        .routes
        .iter()
        .map(|r| (r.method.as_str(), r.path.as_str(), r.handler_name(), r.span))
        .collect();
    assert_eq!(routes, [("get", "/x", None, (3, 4, 3, 20))]);
    let blind: Vec<(&str, bool)> = g
        .blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.is_test))
        .collect();
    assert_eq!(blind, [("python-eval", false)]);
}
