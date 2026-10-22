//! Classify Go composite-literal keys without resolving types for each name.

use crate::calls::LiteralKeyFacts;
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

#[derive(Clone)]
enum TypeShape {
    Map(Box<TypeShape>),
    Sequence(Box<TypeShape>),
    Named(String),
    Other,
}

struct ScopedDefinition {
    name: String,
    shape: TypeShape,
    scope_start: usize,
    scope_end: usize,
    declared_at: usize,
}

struct Definitions {
    file: FxHashMap<String, TypeShape>,
    local: Vec<ScopedDefinition>,
}

impl Definitions {
    fn resolve(&self, name: &str, position: usize) -> Option<&TypeShape> {
        self.local
            .iter()
            .filter(|definition| {
                definition.name == name
                    && definition.declared_at <= position
                    && definition.scope_start <= position
                    && position < definition.scope_end
            })
            .min_by_key(|definition| definition.scope_end - definition.scope_start)
            .map(|definition| &definition.shape)
            .or_else(|| self.file.get(name))
    }
}

/// Walk a Go file once to mark keyed elements whose keys are expressions.
///
/// A keyed element in a map or array/slice literal has an expression key. A
/// keyed element in a struct literal has a field name. Named types and aliases
/// are resolved from declarations in this file. Missing, qualified, and cyclic
/// names stay unknown, so their keys remain conservatively skipped.
pub(super) fn collect_literal_key_facts(root: Node<'_>, source: &[u8]) -> LiteralKeyFacts {
    let definitions = collect_type_definitions(root, source);
    let mut facts = LiteralKeyFacts::default();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "composite_literal" {
            if let (Some(ty), Some(body)) = (
                node.child_by_field_name("type"),
                node.child_by_field_name("body"),
            ) {
                mark_literal_keys(
                    body,
                    &type_shape(ty, source),
                    &definitions,
                    node.start_byte(),
                    &mut facts,
                );
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    facts
}

fn collect_type_definitions(root: Node<'_>, source: &[u8]) -> Definitions {
    let mut definitions = Definitions {
        file: FxHashMap::default(),
        local: Vec::new(),
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "type_spec" | "type_alias") {
            if let (Some(name), Some(ty)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("type"),
            ) {
                if let Some(name) = node_text(name, source) {
                    if is_file_type_definition(node) {
                        definitions.file.insert(name, type_shape(ty, source));
                    } else if let Some(scope) = enclosing_block(node) {
                        definitions.local.push(ScopedDefinition {
                            name,
                            shape: type_shape(ty, source),
                            scope_start: scope.start_byte(),
                            scope_end: scope.end_byte(),
                            declared_at: node.start_byte(),
                        });
                    }
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

fn enclosing_block(mut node: Node<'_>) -> Option<Node<'_>> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "block" {
            return Some(parent);
        }
        node = parent;
    }
    None
}

fn type_shape(ty: Node<'_>, source: &[u8]) -> TypeShape {
    let element = match ty.kind() {
        "map_type" => ty.child_by_field_name("value"),
        "slice_type" | "array_type" | "implicit_length_array_type" => {
            ty.child_by_field_name("element")
        }
        _ => None,
    };
    if let Some(element) = element {
        let element = type_shape(element, source);
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
    shape: &TypeShape,
    definitions: &Definitions,
    position: usize,
    facts: &mut LiteralKeyFacts,
) {
    let Some(element) = element_type(shape, definitions, position) else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        match child.kind() {
            "keyed_element" => {
                facts.mark_map_key(child);
                if let Some(value) = child.child_by_field_name("value") {
                    mark_elided_value(value, element, definitions, position, facts);
                }
            }
            "literal_value" => mark_literal_keys(child, element, definitions, position, facts),
            _ => {}
        }
    }
}

fn mark_elided_value(
    value: Node<'_>,
    shape: &TypeShape,
    definitions: &Definitions,
    position: usize,
    facts: &mut LiteralKeyFacts,
) {
    let literal = (value.kind() == "literal_value")
        .then_some(value)
        .or_else(|| {
            let mut cursor = value.walk();
            value
                .named_children(&mut cursor)
                .find(|child| child.kind() == "literal_value")
        });
    if let Some(literal) = literal {
        mark_literal_keys(literal, shape, definitions, position, facts);
    }
}

fn element_type<'a>(
    shape: &'a TypeShape,
    definitions: &'a Definitions,
    position: usize,
) -> Option<&'a TypeShape> {
    let mut current = shape;
    let mut seen = FxHashSet::default();
    loop {
        match current {
            TypeShape::Map(element) | TypeShape::Sequence(element) => return Some(element),
            TypeShape::Named(name) if seen.insert(name.as_str()) => {
                current = definitions.resolve(name, position)?;
            }
            _ => return None,
        }
    }
}

fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
        .ok()
        .map(str::to_string)
}
