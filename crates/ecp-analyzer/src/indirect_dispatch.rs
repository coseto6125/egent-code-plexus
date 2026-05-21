//! Indirect-call dispatch detection for C, C++, Rust, JavaScript/TypeScript, Python.
//!
//! Produces `RawCallMeta` entries that annotate non-direct call sites in a
//! `LocalGraph`. Only non-direct calls get an entry; direct calls default to
//! `FLAG_DIRECT` by the builder's sparse-population contract.
//!
//! Each per-language function takes the already-parsed tree-sitter tree and
//! the fully-populated `nodes` (post call-extraction) so it can compute the
//! correct `call_index` by re-walking in the same DFS order the call extractor
//! uses and counting how many calls have been attached to each caller.

use ecp_core::analyzer::types::{RawCallMeta, RawNode};
use ecp_core::graph::{CallMeta, NodeKind};
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

// ── shared helpers ────────────────────────────────────────────────────────────

/// Return the index of the innermost Function/Method/Constructor whose span
/// contains `line`. Innermost = smallest row span. Used by both `record_indirect`
/// (for emit + counter advance) and `advance_direct` (counter advance only).
fn find_enclosing_caller(line: u32, nodes: &[RawNode]) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_span: u32 = u32::MAX;
    for (i, n) in nodes.iter().enumerate() {
        if !matches!(
            n.kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constructor
        ) {
            continue;
        }
        if n.span.0 <= line && n.span.2 >= line {
            let width = n.span.2 - n.span.0;
            if width < best_span {
                best_span = width;
                best = Some(i);
            }
        }
    }
    best
}

/// Emit a `RawCallMeta` for a non-direct call site: locate the enclosing
/// function/method/constructor node, compute the `call_index` from the
/// current-call-counter for that node, then push the entry into `out`.
///
/// Returns whether a caller was found (to allow the counter to advance).
fn record_indirect(
    line: u32,
    flags: u8,
    dispatch_type: &str,
    nodes: &[RawNode],
    call_counts: &mut FxHashMap<usize, u32>,
    out: &mut Vec<RawCallMeta>,
) -> bool {
    let Some(caller_idx) = find_enclosing_caller(line, nodes) else {
        return false;
    };
    let count = call_counts.entry(caller_idx).or_insert(0);
    let call_index = *count;
    *count += 1;
    out.push(RawCallMeta {
        caller_name: nodes[caller_idx].name.clone(),
        caller_span: nodes[caller_idx].span,
        call_index,
        flags,
        dispatch_type: dispatch_type.to_string(),
    });
    true
}

/// Advance the call counter for a direct call (does not emit a `RawCallMeta`).
fn advance_direct(line: u32, nodes: &[RawNode], call_counts: &mut FxHashMap<usize, u32>) {
    if let Some(idx) = find_enclosing_caller(line, nodes) {
        *call_counts.entry(idx).or_insert(0) += 1;
    }
}

#[inline]
fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.start_byte()..node.end_byte())?).ok()
}

// ── Rust ──────────────────────────────────────────────────────────────────────

/// Collect parameter types for indirect-call detection, including types that
/// `bare_type_name` (used by LocalTypes) rejects: `fn(...)` function types,
/// `dyn Trait`, `Box<dyn Trait>`, `Arc<dyn T>`, etc.
///
/// Returns a map of `var_name → type_text_as_source`.
pub fn collect_rust_indirect_param_types(
    root: Node<'_>,
    source: &[u8],
) -> FxHashMap<String, String> {
    let mut map = FxHashMap::default();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        // Collect from function_item parameters only.
        if n.kind() == "function_item" {
            if let Some(params) = n.child_by_field_name("parameters") {
                let mut c = params.walk();
                for p in params.children(&mut c) {
                    if p.kind() != "parameter" {
                        continue;
                    }
                    let Some(pat) = p.child_by_field_name("pattern") else {
                        continue;
                    };
                    let Some(ty_node) = p.child_by_field_name("type") else {
                        continue;
                    };
                    let var_name = match pat.kind() {
                        "identifier" => node_text(pat, source).map(str::to_string),
                        "mut_pattern" => pat
                            .named_child(0)
                            .filter(|n| n.kind() == "identifier")
                            .and_then(|n| node_text(n, source))
                            .map(str::to_string),
                        _ => None,
                    };
                    let Some(name) = var_name else { continue };
                    // Get the full type text for fn-ptr and dyn-trait detection.
                    if let Some(ty_text) = node_text(ty_node, source) {
                        map.insert(name, ty_text.to_string());
                    }
                }
            }
        }
        // Also collect from let_declaration with explicit types.
        if n.kind() == "let_declaration" {
            if let Some(ty_node) = n.child_by_field_name("type") {
                if let Some(pat) = n.child_by_field_name("pattern") {
                    let var_name = match pat.kind() {
                        "identifier" => node_text(pat, source).map(str::to_string),
                        "mut_pattern" => pat
                            .named_child(0)
                            .filter(|c| c.kind() == "identifier")
                            .and_then(|c| node_text(c, source))
                            .map(str::to_string),
                        _ => None,
                    };
                    if let (Some(name), Some(ty)) = (var_name, node_text(ty_node, source)) {
                        map.insert(name, ty.to_string());
                    }
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    map
}

/// Walk the Rust AST in the same DFS order as `extract_rust_calls` and emit
/// `RawCallMeta` for every non-direct call site.
///
/// Detection rules:
/// - `&dyn T` / `Box<dyn T>` / `Arc<dyn T>` / `Rc<dyn T>` receiver → `FLAG_DYNAMIC_DISPATCH`
/// - `fn(...)` / `Fn(...)` / `FnMut(...)` / `FnOnce(...)` typed variable called → `FLAG_CALLBACK`
/// - Generic `T: Trait` receiver (cannot resolve impl) → `FLAG_DYNAMIC_DISPATCH`
/// - Constructor call (`X::new` or `X { }`) → `FLAG_DIRECT | FLAG_CONSTRUCTOR_CALL`
pub fn detect_rust_indirect(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    // Maps var_name → resolved type string (from LocalTypes)
    param_types: &FxHashMap<String, String>,
) -> Vec<RawCallMeta> {
    let mut out = Vec::new();
    let mut call_counts: FxHashMap<usize, u32> = FxHashMap::default();
    detect_rust_node(root, source, nodes, param_types, &mut call_counts, &mut out);
    out
}

fn detect_rust_node(
    node: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    param_types: &FxHashMap<String, String>,
    call_counts: &mut FxHashMap<usize, u32>,
    out: &mut Vec<RawCallMeta>,
) {
    if node.kind() == "call_expression" {
        let line = node.start_position().row as u32;
        let (flags, dispatch_type, is_real_call) = classify_rust_call(node, source, param_types);
        if is_real_call {
            if flags & CallMeta::FLAG_DIRECT == 0 || flags & CallMeta::FLAG_CONSTRUCTOR_CALL != 0 {
                // Non-direct or constructor call — emit meta only for non-direct
                if flags & CallMeta::FLAG_DIRECT == 0 {
                    record_indirect(line, flags, &dispatch_type, nodes, call_counts, out);
                } else {
                    advance_direct(line, nodes, call_counts);
                }
            } else {
                advance_direct(line, nodes, call_counts);
            }
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        detect_rust_node(child, source, nodes, param_types, call_counts, out);
    }
}

/// Classify a Rust `call_expression` node.
/// Returns `(flags, dispatch_type_string, is_a_real_call)`.
fn classify_rust_call(
    call: Node<'_>,
    source: &[u8],
    param_types: &FxHashMap<String, String>,
) -> (u8, String, bool) {
    let Some(function) = call.child_by_field_name("function") else {
        return (CallMeta::FLAG_DIRECT, String::new(), false);
    };
    match function.kind() {
        "identifier" => {
            // Plain function call: `foo()` — check if it's a fn-ptr variable.
            let name = node_text(function, source).unwrap_or_default();
            if let Some(ty) = param_types.get(name) {
                if is_fn_type(ty) {
                    return (CallMeta::FLAG_CALLBACK, ty.to_string(), true);
                }
            }
            (CallMeta::FLAG_DIRECT, String::new(), true)
        }
        "field_expression" => {
            // Method call: `obj.method()` — inspect receiver type.
            if let Some(value) = function.child_by_field_name("value") {
                let obj = node_text(value, source).unwrap_or_default();
                let var_name = obj.trim_start_matches(['*', '&', ' ']);
                if let Some(ty) = param_types.get(var_name) {
                    // `&dyn Trait`, `dyn Trait`, `Box<dyn Trait>`, `Arc<dyn Trait>` etc.
                    if is_dyn_type(ty) {
                        let dispatch_str = extract_dyn_trait_name(ty);
                        return (CallMeta::FLAG_DYNAMIC_DISPATCH, dispatch_str, true);
                    }
                }
            }
            (CallMeta::FLAG_DIRECT, String::new(), true)
        }
        "scoped_identifier" => {
            // `Type::new(...)` — constructor pattern.
            let text = node_text(function, source).unwrap_or_default();
            if text.ends_with("::new") || text.ends_with("::default") {
                return (
                    CallMeta::FLAG_DIRECT | CallMeta::FLAG_CONSTRUCTOR_CALL,
                    String::new(),
                    true,
                );
            }
            (CallMeta::FLAG_DIRECT, String::new(), true)
        }
        _ => (CallMeta::FLAG_DIRECT, String::new(), true),
    }
}

/// Returns true if the type string represents a dyn-dispatch receiver.
fn is_dyn_type(ty: &str) -> bool {
    let t = ty.trim();
    t.starts_with("dyn ")
        || t.starts_with("&dyn ")
        || t.starts_with("&mut dyn ")
        || t.starts_with("Box<dyn ")
        || t.starts_with("Arc<dyn ")
        || t.starts_with("Rc<dyn ")
}

/// Extract the trait name from a dyn-dispatch type string.
fn extract_dyn_trait_name(ty: &str) -> String {
    // "Box<dyn Handler>" → "dyn Handler"
    // "&dyn Handler" → "dyn Handler"
    let t = ty.trim();
    if let Some(inner) = t
        .strip_prefix("Box<")
        .or_else(|| t.strip_prefix("Arc<"))
        .or_else(|| t.strip_prefix("Rc<"))
    {
        // Remove trailing `>` or `>`+bounds.
        let inner = inner.trim_end_matches('>').trim();
        return inner.to_string();
    }
    // Strip leading `&` / `&mut`.
    t.trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim()
        .to_string()
}

fn is_fn_type(ty: &str) -> bool {
    ty.starts_with("fn(")
        || ty.starts_with("Fn(")
        || ty.starts_with("FnMut(")
        || ty.starts_with("FnOnce(")
        || ty.starts_with("Box<dyn Fn")
        || ty.starts_with("Box<dyn FnMut")
        || ty.starts_with("Box<dyn FnOnce")
}

// ── C / C++ ───────────────────────────────────────────────────────────────────

/// Detect indirect calls in C/C++ source. Covers:
/// - Function-pointer variable call: `fp(arg)` where receiver is not a plain identifier
///   naming a defined function (i.e., the callee node is a parenthesized `*fp` or a
///   plain identifier typed as a function pointer in `fn_ptr_vars`).
/// - Struct-of-fn-pointers call: `ops->open(...)` through a known struct type.
/// - C++ virtual method call through base pointer/reference (detected via
///   `fn_ptr_vars` populated from known base types).
///
/// `fn_ptr_vars` maps local variable names to their type strings for disambiguation.
pub fn detect_c_cpp_indirect(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    fn_ptr_vars: &FxHashMap<String, String>,
    is_cpp: bool,
) -> Vec<RawCallMeta> {
    let mut out = Vec::new();
    let mut call_counts: FxHashMap<usize, u32> = FxHashMap::default();
    detect_c_cpp_node(
        root,
        source,
        nodes,
        fn_ptr_vars,
        is_cpp,
        &mut call_counts,
        &mut out,
    );
    out
}

fn detect_c_cpp_node(
    node: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    fn_ptr_vars: &FxHashMap<String, String>,
    is_cpp: bool,
    call_counts: &mut FxHashMap<usize, u32>,
    out: &mut Vec<RawCallMeta>,
) {
    if node.kind() == "call_expression" {
        let line = node.start_position().row as u32;
        let (flags, dispatch_type) = classify_c_cpp_call(node, source, fn_ptr_vars, is_cpp);
        if flags & CallMeta::FLAG_DIRECT == 0 {
            record_indirect(line, flags, &dispatch_type, nodes, call_counts, out);
        } else {
            advance_direct(line, nodes, call_counts);
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        detect_c_cpp_node(child, source, nodes, fn_ptr_vars, is_cpp, call_counts, out);
    }
}

fn classify_c_cpp_call(
    call: Node<'_>,
    source: &[u8],
    fn_ptr_vars: &FxHashMap<String, String>,
    is_cpp: bool,
) -> (u8, String) {
    let Some(function) = call.child_by_field_name("function") else {
        return (CallMeta::FLAG_DIRECT, String::new());
    };

    match function.kind() {
        "identifier" => {
            let name = node_text(function, source).unwrap_or_default();
            if let Some(ty) = fn_ptr_vars.get(name) {
                // Variable typed as function pointer.
                let flags = if ty.contains("struct ") || ty.contains("_ops") {
                    CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH
                } else {
                    CallMeta::FLAG_CALLBACK
                };
                return (flags, ty.to_string());
            }
            (CallMeta::FLAG_DIRECT, String::new())
        }
        "parenthesized_expression" => {
            // `(*fp)(args)` — dereferenced function pointer.
            let text = node_text(function, source)
                .unwrap_or_default()
                .trim()
                .to_string();
            (CallMeta::FLAG_CALLBACK, text)
        }
        "field_expression" | "pointer_expression" => {
            // `obj->method()` or `obj.method()` through a struct of fn-pointers.
            if let Some(obj) = function
                .child_by_field_name("value")
                .or_else(|| function.child_by_field_name("argument"))
            {
                let obj_name = node_text(obj, source).unwrap_or_default();
                if let Some(ty) = fn_ptr_vars.get(obj_name.trim_start_matches('*')) {
                    let flags = CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH;
                    return (flags, ty.to_string());
                }
            }
            // In C++: `base_ptr->virtual_method()` — check if receiver is a
            // known base type pointer.
            if is_cpp {
                if let Some(obj) = function.child_by_field_name("value") {
                    let obj_name = node_text(obj, source).unwrap_or_default();
                    let var_name = obj_name.trim_start_matches('*').trim();
                    if let Some(ty) = fn_ptr_vars.get(var_name) {
                        return (CallMeta::FLAG_DYNAMIC_DISPATCH, ty.to_string());
                    }
                }
            }
            (CallMeta::FLAG_DIRECT, String::new())
        }
        _ => (CallMeta::FLAG_DIRECT, String::new()),
    }
}

/// Build a map of `var_name → type_string` from C/C++ local declarations and
/// function parameters, focusing on pointer-to-struct and function-pointer types.
///
/// This is a lightweight heuristic scan of `let_declaration`-equivalent C nodes
/// (`declaration`, `parameter_declaration`) to populate the type map for indirect
/// call detection.
pub fn collect_c_cpp_fn_ptr_vars(root: Node<'_>, source: &[u8]) -> FxHashMap<String, String> {
    let mut map = FxHashMap::default();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "declaration" | "parameter_declaration" => {
                if let Some((var_name, ty)) = c_var_type_pair(n, source) {
                    map.insert(var_name, ty);
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    map
}

fn c_var_type_pair(decl: Node<'_>, source: &[u8]) -> Option<(String, String)> {
    // Look for a declarator child; if the type contains `(*)` it's a fn-ptr.
    let type_node = decl.child_by_field_name("type")?;
    let ty_text = node_text(type_node, source)?;

    // Find the declarator (name of the variable).
    let declarator = decl.child_by_field_name("declarator")?;
    let var_name = find_leaf_identifier(declarator, source)?;

    // Only track struct pointers and function-pointer types for our purposes.
    let full_decl_text = node_text(decl, source).unwrap_or_default();
    if full_decl_text.contains("(*)")
        || full_decl_text.contains("* ")
        || ty_text.contains("struct ")
        || ty_text.contains("_ops")
        || ty_text.ends_with("_t")
    {
        Some((var_name, ty_text.to_string()))
    } else {
        None
    }
}

fn find_leaf_identifier(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() == "identifier" || node.kind() == "type_identifier" {
        return node_text(node, source).map(str::to_string);
    }
    // Descend into pointer / function declarators.
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if let Some(name) = find_leaf_identifier(child, source) {
            return Some(name);
        }
    }
    None
}

// ── JavaScript / TypeScript ───────────────────────────────────────────────────

/// Detect indirect calls in JavaScript/TypeScript.
///
/// Detection rules:
/// - Callback parameter invoked: `cb(...)` where `cb` is a parameter (not a resolved name).
/// - Method call on `any`/`unknown`-typed or untyped receiver.
/// - `Function.prototype.call/apply/bind` invocation.
///
/// `param_names` contains the parameter names of every function in scope (to
/// distinguish callback parameters from local variables).
pub fn detect_js_ts_indirect(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    param_names: &FxHashSet<String>, // name → true if it's a param (not a declared fn)
) -> Vec<RawCallMeta> {
    let mut out = Vec::new();
    let mut call_counts: FxHashMap<usize, u32> = FxHashMap::default();
    detect_js_ts_node(root, source, nodes, param_names, &mut call_counts, &mut out);
    out
}

fn detect_js_ts_node(
    node: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    param_names: &FxHashSet<String>,
    call_counts: &mut FxHashMap<usize, u32>,
    out: &mut Vec<RawCallMeta>,
) {
    if node.kind() == "call_expression" {
        let line = node.start_position().row as u32;
        let (flags, dispatch_type) = classify_js_call(node, source, param_names);
        if flags & CallMeta::FLAG_DIRECT == 0 {
            record_indirect(line, flags, &dispatch_type, nodes, call_counts, out);
        } else {
            advance_direct(line, nodes, call_counts);
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        detect_js_ts_node(child, source, nodes, param_names, call_counts, out);
    }
}

fn classify_js_call(
    call: Node<'_>,
    source: &[u8],
    param_names: &FxHashSet<String>,
) -> (u8, String) {
    let Some(function) = call.child_by_field_name("function") else {
        return (CallMeta::FLAG_DIRECT, String::new());
    };

    match function.kind() {
        "identifier" => {
            let name = node_text(function, source).unwrap_or_default();
            if param_names.contains(name) {
                return (CallMeta::FLAG_CALLBACK, String::new());
            }
            (CallMeta::FLAG_DIRECT, String::new())
        }
        "member_expression" => {
            // `obj.method()` — check for `.call` / `.apply` / `.bind` patterns.
            if let Some(property) = function.child_by_field_name("property") {
                let prop = node_text(property, source).unwrap_or_default();
                match prop {
                    "call" | "apply" | "bind" => {
                        let dispatch = format!("Function.prototype.{prop}");
                        return (
                            CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH,
                            dispatch,
                        );
                    }
                    _ => {}
                }
                // Check if the object is a parameter (callback invoked as method).
                if let Some(obj) = function.child_by_field_name("object") {
                    let obj_name = node_text(obj, source).unwrap_or_default();
                    if param_names.contains(obj_name) {
                        return (CallMeta::FLAG_CALLBACK, String::new());
                    }
                }
            }
            (CallMeta::FLAG_DIRECT, String::new())
        }
        _ => (CallMeta::FLAG_DIRECT, String::new()),
    }
}

/// Collect all parameter names from function/method definitions in the file,
/// marking them as potential callback identifiers.
pub fn collect_js_param_names(root: Node<'_>, source: &[u8]) -> FxHashSet<String> {
    let mut map: FxHashSet<String> = FxHashSet::default();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_declaration"
            | "function"
            | "arrow_function"
            | "method_definition"
            | "function_expression" => {
                collect_params_js(n, source, &mut map);
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    map
}

fn collect_params_js(fn_node: Node<'_>, source: &[u8], out: &mut FxHashSet<String>) {
    let Some(params) = fn_node
        .child_by_field_name("parameters")
        .or_else(|| fn_node.child_by_field_name("parameter"))
    else {
        return;
    };
    let mut c = params.walk();
    for p in params.children(&mut c) {
        match p.kind() {
            "identifier" => {
                if let Some(name) = node_text(p, source) {
                    out.insert(name.to_string());
                }
            }
            "required_parameter" | "optional_parameter" | "formal_parameters" => {
                if let Some(pat) = p.child_by_field_name("pattern") {
                    if let Some(name) = node_text(pat, source) {
                        out.insert(name.to_string());
                    }
                }
            }
            _ => {}
        }
    }
}

// ── Python ────────────────────────────────────────────────────────────────────

/// Detect indirect calls in Python source.
///
/// Detection rules:
/// - Callback parameter call: `cb(...)` where `cb` is a function parameter.
/// - `getattr(obj, name)(...)` dispatch.
/// - `functools.partial(f)` / `functools.wraps(f)` wrapper call.
pub fn detect_python_indirect(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    param_names: &FxHashSet<String>,
) -> Vec<RawCallMeta> {
    let mut out = Vec::new();
    let mut call_counts: FxHashMap<usize, u32> = FxHashMap::default();
    detect_python_node(root, source, nodes, param_names, &mut call_counts, &mut out);
    out
}

fn detect_python_node(
    node: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    param_names: &FxHashSet<String>,
    call_counts: &mut FxHashMap<usize, u32>,
    out: &mut Vec<RawCallMeta>,
) {
    if node.kind() == "call" {
        let line = node.start_position().row as u32;
        let (flags, dispatch_type) = classify_python_call(node, source, param_names);
        if flags & CallMeta::FLAG_DIRECT == 0 {
            record_indirect(line, flags, &dispatch_type, nodes, call_counts, out);
        } else {
            advance_direct(line, nodes, call_counts);
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        detect_python_node(child, source, nodes, param_names, call_counts, out);
    }
}

fn classify_python_call(
    call: Node<'_>,
    source: &[u8],
    param_names: &FxHashSet<String>,
) -> (u8, String) {
    // Python: call node has `function` field.
    let Some(function) = call.child_by_field_name("function") else {
        return (CallMeta::FLAG_DIRECT, String::new());
    };

    match function.kind() {
        "identifier" => {
            let name = node_text(function, source).unwrap_or_default();
            // `getattr(...)()` — already unwrapped to a call; but literal `getattr` call.
            if name == "getattr" {
                return (CallMeta::FLAG_DYNAMIC_DISPATCH, String::new());
            }
            if param_names.contains(name) {
                return (CallMeta::FLAG_CALLBACK, String::new());
            }
            (CallMeta::FLAG_DIRECT, String::new())
        }
        "attribute" => {
            // `obj.method()` — check for `functools.partial` / `functools.wraps`.
            let text = node_text(function, source).unwrap_or_default();
            if text == "functools.partial" || text == "functools.wraps" {
                let attr = function
                    .child_by_field_name("attribute")
                    .and_then(|n| node_text(n, source))
                    .unwrap_or_default();
                return (CallMeta::FLAG_CALLBACK, format!("functools.{attr}"));
            }
            // Callback parameter invoked as method: `cb.method()`.
            if let Some(obj) = function.child_by_field_name("object") {
                let obj_name = node_text(obj, source).unwrap_or_default();
                if param_names.contains(obj_name) {
                    return (CallMeta::FLAG_DYNAMIC_DISPATCH, String::new());
                }
            }
            (CallMeta::FLAG_DIRECT, String::new())
        }
        // `call()(args)` — result of a call being called (dynamic).
        "call" => (CallMeta::FLAG_DYNAMIC_DISPATCH, String::new()),
        _ => (CallMeta::FLAG_DIRECT, String::new()),
    }
}

/// Collect function parameter names from all function definitions in the file.
pub fn collect_python_param_names(root: Node<'_>, source: &[u8]) -> FxHashSet<String> {
    let mut map: FxHashSet<String> = FxHashSet::default();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition" {
            if let Some(params) = n.child_by_field_name("parameters") {
                collect_python_params(params, source, &mut map);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    map
}

fn collect_python_params(params: Node<'_>, source: &[u8], out: &mut FxHashSet<String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        match p.kind() {
            "identifier" => {
                if let Some(name) = node_text(p, source) {
                    if name != "self" && name != "cls" {
                        out.insert(name.to_string());
                    }
                }
            }
            "typed_parameter" | "typed_default_parameter" | "default_parameter" => {
                if let Some(name_node) = p.child_by_field_name("name") {
                    if let Some(name) = node_text(name_node, source) {
                        if name != "self" && name != "cls" {
                            out.insert(name.to_string());
                        }
                    }
                }
            }
            "list_splat_pattern" | "dictionary_splat_pattern" => {
                if let Some(inner) = p.named_child(0) {
                    if let Some(name) = node_text(inner, source) {
                        out.insert(name.to_string());
                    }
                }
            }
            _ => {}
        }
    }
}
