//! PHP receiver-type binding for method call sites.
//!
//! Handles three patterns that PHP 7+ makes statically knowable:
//!
//! - `$this->method()`  → receiver = enclosing class name
//! - `parent::method()` → receiver = first heritage class of the enclosing class
//! - `self::method()` / `static::method()` → receiver = enclosing class name
//!   (`static` is late-static-binding; for graph purposes we treat it the
//!   same as `self` since the graph is per-file and the resolver will follow
//!   the concrete class).
//!
//! Typed `$var->method()` where `$var` is not `$this` is intentionally left
//! unbound: PHP 7 property/param type hints require a second pass to propagate
//! types through the scope and are deferred to a later improvement task.

use crate::calls::attach_to_enclosing;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use tree_sitter::Node;

/// Enclosing-class context built by a pre-walk over the node list.
/// Maps `(start_row, end_row)` span → `(class_name, Option<parent_name>)`.
pub struct ClassContext {
    entries: Vec<((u32, u32), String, Option<String>)>,
}

impl ClassContext {
    pub fn from_nodes(nodes: &[RawNode]) -> Self {
        let entries = nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Interface))
            .map(|n| {
                let span = (n.span.0, n.span.2);
                let parent = n.heritage.first().cloned();
                (span, n.name.clone(), parent)
            })
            .collect();
        Self { entries }
    }

    /// Return the name of the innermost class enclosing `line`.
    fn enclosing_class_name(&self, line: u32) -> Option<&str> {
        let mut best: Option<(&str, u32)> = None;
        for ((start, end), name, _) in &self.entries {
            if *start <= line && line <= *end {
                let width = end - start;
                if best.is_none_or(|(_, w)| width < w) {
                    best = Some((name.as_str(), width));
                }
            }
        }
        best.map(|(n, _)| n)
    }

    /// Return the first heritage (parent) class of the class enclosing `line`.
    fn enclosing_parent_name(&self, line: u32) -> Option<&str> {
        let mut best: Option<(Option<&str>, u32)> = None;
        for ((start, end), _, parent) in &self.entries {
            if *start <= line && line <= *end {
                let width = end - start;
                if best.is_none_or(|(_, w)| width < w) {
                    best = Some((parent.as_deref(), width));
                }
            }
        }
        best.and_then(|(p, _)| p)
    }
}

/// Walk the PHP AST and attach callees to enclosing nodes, with receiver-type
/// binding applied for `$this->`, `parent::`, `self::`, and `static::` call sites.
pub fn extract_php_calls(root: Node<'_>, source: &[u8], nodes: &mut [RawNode]) {
    let ctx = ClassContext::from_nodes(nodes);
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "member_call_expression" => {
                if let Some(callee) = php_member_callee(n, source, &ctx) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "scoped_call_expression" => {
                if let Some(callee) = php_scoped_callee(n, source, &ctx) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// Resolve the callee for `$obj->method(args)`.
/// Only `$this` is bound to the enclosing class; other receivers fall back
/// to the bare method name.
fn php_member_callee(call: Node<'_>, source: &[u8], ctx: &ClassContext) -> Option<String> {
    let name_node = call.child_by_field_name("name")?;
    let method_name = name_node.utf8_text(source).ok()?;

    let obj_node = call.child_by_field_name("object")?;
    let line = call.start_position().row as u32;

    if obj_node.kind() == "variable_name" {
        if let Ok(var_text) = obj_node.utf8_text(source) {
            if var_text == "$this" {
                if let Some(class_name) = ctx.enclosing_class_name(line) {
                    return Some(format!("{class_name}.{method_name}"));
                }
            }
        }
    }

    // Unresolved receiver — emit bare method name as fallback.
    Some(method_name.to_string())
}

/// Resolve the callee for `Scope::method(args)`.
///
/// - `parent::method` → `ParentClass.method` (using heritage)
/// - `self::method` / `static::method` → `EnclosingClass.method`
/// - `ClassName::method` → `ClassName.method` (explicit named scope)
fn php_scoped_callee(call: Node<'_>, source: &[u8], ctx: &ClassContext) -> Option<String> {
    let name_node = call.child_by_field_name("name")?;
    let method_name = name_node.utf8_text(source).ok()?;

    let scope_node = call.child_by_field_name("scope")?;
    let line = call.start_position().row as u32;

    match scope_node.kind() {
        "relative_scope" => {
            let scope_text = scope_node.utf8_text(source).ok()?;
            match scope_text {
                "parent" => {
                    if let Some(parent_name) = ctx.enclosing_parent_name(line) {
                        return Some(format!("{parent_name}.{method_name}"));
                    }
                    // parent:: with no known heritage — emit bare name
                    Some(method_name.to_string())
                }
                "self" | "static" => {
                    if let Some(class_name) = ctx.enclosing_class_name(line) {
                        return Some(format!("{class_name}.{method_name}"));
                    }
                    Some(method_name.to_string())
                }
                _ => Some(method_name.to_string()),
            }
        }
        "name" | "qualified_name" => {
            // Explicit class name: `Foo::bar()` → `Foo.bar`
            let class_name = scope_node.utf8_text(source).ok()?;
            Some(format!("{class_name}.{method_name}"))
        }
        _ => Some(method_name.to_string()),
    }
}
