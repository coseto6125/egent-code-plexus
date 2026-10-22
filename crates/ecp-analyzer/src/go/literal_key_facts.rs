//! Classify Go composite-literal keys without resolving types for each name.

use crate::calls::LiteralKeyFacts;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

#[derive(Clone)]
enum TypeShape {
    Map(Box<TypeShape>),
    Sequence(Box<TypeShape>),
    Named(String),
    Other,
}

/// Walk a Go file once to mark keyed elements whose keys are expressions.
///
/// A keyed element in a map or array/slice literal has an expression key. A
/// keyed element in a struct literal has a field name. Named types and aliases
/// are resolved from declarations in this file. Missing, qualified, and cyclic
/// names stay unknown, so their keys remain conservatively skipped.
pub fn collect_literal_key_facts(root: Node<'_>, source: &[u8]) -> LiteralKeyFacts {
    let definitions = collect_type_definitions(root, source);
    let mut facts = LiteralKeyFacts::default();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "composite_literal" {
            if let (Some(ty), Some(body)) = (
                node.child_by_field_name("type"),
                node.child_by_field_name("body"),
            ) {
                mark_literal_keys(body, type_shape(ty, source), &definitions, &mut facts);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    facts
}

fn collect_type_definitions(root: Node<'_>, source: &[u8]) -> HashMap<String, TypeShape> {
    let mut definitions = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "type_spec" | "type_alias") && is_file_type_definition(node) {
            if let (Some(name), Some(ty)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("type"),
            ) {
                if let Some(name) = node_text(name, source) {
                    definitions.insert(name, type_shape(ty, source));
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    definitions
}

fn is_file_type_definition(node: Node<'_>) -> bool {
    node.parent()
        .and_then(|declaration| declaration.parent())
        .is_some_and(|parent| parent.kind() == "source_file")
}

fn type_shape(ty: Node<'_>, source: &[u8]) -> TypeShape {
    let element = match ty.kind() {
        "map_type" => ty.child_by_field_name("value").map(Box::new),
        "slice_type" | "array_type" | "implicit_length_array_type" => {
            ty.child_by_field_name("element").map(Box::new)
        }
        _ => None,
    };
    if let Some(element) = element {
        let element = type_shape(*element, source);
        return if ty.kind() == "map_type" {
            TypeShape::Map(Box::new(element))
        } else {
            TypeShape::Sequence(Box::new(element))
        };
    }
    if ty.kind() == "type_identifier" {
        if let Some(name) = node_text(ty, source) {
            return TypeShape::Named(name);
        }
    }
    TypeShape::Other
}

fn mark_literal_keys(
    body: Node<'_>,
    shape: TypeShape,
    definitions: &HashMap<String, TypeShape>,
    facts: &mut LiteralKeyFacts,
) {
    let Some(element) = element_type(&shape, definitions, &mut HashSet::new()) else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        match child.kind() {
            "keyed_element" => {
                facts.mark_map_key(child);
                if let Some(value) = child.child_by_field_name("value") {
                    mark_elided_value(value, element.clone(), definitions, facts);
                }
            }
            "literal_value" => mark_literal_keys(child, element.clone(), definitions, facts),
            _ => {}
        }
    }
}

fn mark_elided_value(
    value: Node<'_>,
    shape: TypeShape,
    definitions: &HashMap<String, TypeShape>,
    facts: &mut LiteralKeyFacts,
) {
    if value.kind() == "literal_value" {
        mark_literal_keys(value, shape, definitions, facts);
    }
}

fn element_type(
    shape: &TypeShape,
    definitions: &HashMap<String, TypeShape>,
    seen: &mut HashSet<String>,
) -> Option<TypeShape> {
    match shape {
        TypeShape::Map(element) | TypeShape::Sequence(element) => Some((**element).clone()),
        TypeShape::Named(name) if seen.insert(name.clone()) => definitions
            .get(name)
            .and_then(|definition| element_type(definition, definitions, seen)),
        _ => None,
    }
}

fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
        .ok()
        .map(str::to_string)
}
