//! Classify Go composite-literal keys without resolving types for each name.

use super::receiver_types::LocalTypes;
use crate::calls::{start_pos, LiteralKeyFacts};
use ecp_core::analyzer::types::RawImport;
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

enum TypeShape {
    Map(Box<TypeShape>),
    Sequence(Box<TypeShape>),
    Named { name: String, position: usize },
    Imported,
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
/// are resolved from declarations in this file. An unshadowed imported type can
/// read lowercase keys because another package cannot expose such struct fields.
pub(super) fn collect_literal_key_facts(
    root: Node<'_>,
    source: &[u8],
    imports: &[RawImport],
    local_types: &LocalTypes,
) -> LiteralKeyFacts {
    let definitions = collect_type_definitions(root, source, imports, local_types);
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
                    &type_shape(ty, source, imports, local_types),
                    &definitions,
                    source,
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

fn collect_type_definitions(
    root: Node<'_>,
    source: &[u8],
    imports: &[RawImport],
    local_types: &LocalTypes,
) -> Definitions {
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
                        definitions
                            .file
                            .insert(name, type_shape(ty, source, imports, local_types));
                    } else if let Some(scope) = enclosing_block(node) {
                        definitions.local.push(ScopedDefinition {
                            name,
                            shape: type_shape(ty, source, imports, local_types),
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

fn type_shape(
    ty: Node<'_>,
    source: &[u8],
    imports: &[RawImport],
    local_types: &LocalTypes,
) -> TypeShape {
    let element = match ty.kind() {
        "map_type" => ty.child_by_field_name("value"),
        "slice_type" | "array_type" | "implicit_length_array_type" => {
            ty.child_by_field_name("element")
        }
        _ => None,
    };
    if let Some(element) = element {
        let element = type_shape(element, source, imports, local_types);
        return if ty.kind() == "map_type" {
            TypeShape::Map(Box::new(element))
        } else {
            TypeShape::Sequence(Box::new(element))
        };
    }
    if ty.kind() == "qualified_type" {
        let Some(package) = ty.child_by_field_name("package") else {
            return TypeShape::Other;
        };
        return node_text(package, source)
            .filter(|name| {
                !local_types.binds_local(start_pos(package), name)
                    && imports
                        .iter()
                        .any(|import| import.imported_name.as_str() == name)
            })
            .map_or(TypeShape::Other, |_| TypeShape::Imported);
    }
    if ty.kind() == "type_identifier" {
        if let Some(name) = node_text(ty, source) {
            return TypeShape::Named {
                name,
                position: ty.start_byte(),
            };
        }
    }
    TypeShape::Other
}

fn mark_literal_keys(
    body: Node<'_>,
    shape: &TypeShape,
    definitions: &Definitions,
    source: &[u8],
    facts: &mut LiteralKeyFacts,
) {
    let Some(shape) = resolved_shape(shape, definitions) else {
        return;
    };
    if matches!(shape, TypeShape::Imported) {
        mark_imported_literal_keys(body, source, facts);
        return;
    }
    let Some(element) = element_type(shape) else {
        return;
    };
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        match child.kind() {
            "keyed_element" => {
                facts.mark_map_key(child);
                if let Some(value) = child.child_by_field_name("value") {
                    mark_elided_value(value, element, definitions, source, facts);
                }
            }
            "literal_value" => mark_literal_keys(child, element, definitions, source, facts),
            "literal_element" => mark_elided_value(child, element, definitions, source, facts),
            _ => {}
        }
    }
}

fn mark_elided_value(
    value: Node<'_>,
    shape: &TypeShape,
    definitions: &Definitions,
    source: &[u8],
    facts: &mut LiteralKeyFacts,
) {
    let literal = (value.kind() == "literal_value")
        .then_some(value)
        .or_else(|| {
            let mut cursor = value.walk();
            let literal = value
                .named_children(&mut cursor)
                .find(|child| child.kind() == "literal_value");
            literal
        });
    if let Some(literal) = literal {
        mark_literal_keys(literal, shape, definitions, source, facts);
    }
}

fn mark_imported_literal_keys(body: Node<'_>, source: &[u8], facts: &mut LiteralKeyFacts) {
    let mut cursor = body.walk();
    for keyed in body.named_children(&mut cursor) {
        let Some(key) = (keyed.kind() == "keyed_element")
            .then(|| keyed.child_by_field_name("key"))
            .flatten()
        else {
            continue;
        };
        let identifier = (key.kind() == "identifier").then_some(key).or_else(|| {
            (key.kind() == "literal_element")
                .then(|| key.named_child(0))
                .flatten()
                .filter(|child| child.kind() == "identifier")
        });
        let Some(name) = identifier.and_then(|identifier| node_text(identifier, source)) else {
            continue;
        };
        if name.chars().next().is_some_and(char::is_lowercase) {
            facts.mark_map_key(keyed);
        }
    }
}

fn element_type(shape: &TypeShape) -> Option<&TypeShape> {
    match shape {
        TypeShape::Map(element) | TypeShape::Sequence(element) => Some(element),
        _ => None,
    }
}

fn resolved_shape<'a>(shape: &'a TypeShape, definitions: &'a Definitions) -> Option<&'a TypeShape> {
    let mut current = shape;
    let mut seen = FxHashSet::default();
    while let TypeShape::Named { name, position } = current {
        if !seen.insert(name.as_str()) {
            return None;
        }
        current = definitions.resolve(name, *position)?;
    }
    Some(current)
}

fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
        .ok()
        .map(str::to_string)
}
