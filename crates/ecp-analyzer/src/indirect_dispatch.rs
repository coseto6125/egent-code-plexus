//! Indirect-call dispatch detection for C, C++, Rust, JavaScript/TypeScript, Python.
//!
//! Produces `RawCallMeta` entries that annotate non-direct call sites in a
//! `LocalGraph`. Only non-direct calls get an entry; direct calls default to
//! `FLAG_DIRECT` by the builder's sparse-population contract.
//!
//! Each per-language function takes the already-parsed tree-sitter tree, the
//! fully-populated `nodes` and the `CallSiteIndex` the call extractor filled
//! as it pushed each call. The index names the exact caller and `calls` slot of
//! a call node, so `call_index` never depends on this walk matching the
//! extractor's traversal order or its skip rules.

use crate::calls::{node_range, range_width, start_pos, CallSiteIndex, Pos, PosRange};
use crate::preproc_fallback::{brace_depths, depth_at};
use ecp_core::analyzer::types::{RawCallMeta, RawNode};
use ecp_core::graph::CallMeta;
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

// ── shared helpers ────────────────────────────────────────────────────────────

/// Emit a `RawCallMeta` for a non-direct call node. A call the extractor did
/// not record has no Calls edge to annotate, so it emits nothing.
fn record_indirect(
    call: Node<'_>,
    flags: u8,
    dispatch_type: &str,
    nodes: &[RawNode],
    call_sites: &CallSiteIndex,
    out: &mut Vec<RawCallMeta>,
) {
    let Some(&(caller_idx, call_index)) = call_sites.get(&call.id()) else {
        return;
    };
    out.push(RawCallMeta {
        caller_name: nodes[caller_idx].name.clone(),
        caller_span: nodes[caller_idx].span,
        call_index,
        flags,
        dispatch_type: dispatch_type.to_string(),
    });
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

/// Walk the Rust AST and emit `RawCallMeta` for every non-direct call site.
///
/// Detection rules:
/// - `&dyn T` / `Box<dyn T>` / `Arc<dyn T>` / `Rc<dyn T>` receiver → `FLAG_DYNAMIC_DISPATCH`
/// - `fn(...)` / `Fn(...)` / `FnMut(...)` / `FnOnce(...)` typed variable called → `FLAG_CALLBACK`
/// - Generic `T: Trait` receiver (cannot resolve impl) → `FLAG_DYNAMIC_DISPATCH`
pub fn detect_rust_indirect(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    // Maps var_name → resolved type string (from LocalTypes)
    param_types: &FxHashMap<String, String>,
    call_sites: &CallSiteIndex,
) -> Vec<RawCallMeta> {
    let mut out = Vec::new();
    detect_rust_node(root, source, nodes, param_types, call_sites, &mut out);
    out
}

fn detect_rust_node(
    node: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    param_types: &FxHashMap<String, String>,
    call_sites: &CallSiteIndex,
    out: &mut Vec<RawCallMeta>,
) {
    if node.kind() == "call_expression" {
        let (flags, dispatch_type, is_real_call) = classify_rust_call(node, source, param_types);
        if is_real_call && flags & CallMeta::FLAG_DIRECT == 0 {
            record_indirect(node, flags, &dispatch_type, nodes, call_sites, out);
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        detect_rust_node(child, source, nodes, param_types, call_sites, out);
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
/// `fn_ptr_vars` holds each declared name's type in its own scope.
pub fn detect_c_cpp_indirect(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    fn_ptr_vars: &FnPtrVars,
    is_cpp: bool,
    call_sites: &CallSiteIndex,
) -> Vec<RawCallMeta> {
    let mut out = Vec::new();
    detect_c_cpp_node(
        root,
        source,
        nodes,
        fn_ptr_vars,
        is_cpp,
        call_sites,
        &mut out,
    );
    out
}

fn detect_c_cpp_node(
    node: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    fn_ptr_vars: &FnPtrVars,
    is_cpp: bool,
    call_sites: &CallSiteIndex,
    out: &mut Vec<RawCallMeta>,
) {
    if node.kind() == "call_expression" {
        let (flags, dispatch_type) = classify_c_cpp_call(node, source, fn_ptr_vars, is_cpp);
        if flags & CallMeta::FLAG_DIRECT == 0 {
            record_indirect(node, flags, &dispatch_type, nodes, call_sites, out);
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        detect_c_cpp_node(child, source, nodes, fn_ptr_vars, is_cpp, call_sites, out);
    }
}

fn classify_c_cpp_call(
    call: Node<'_>,
    source: &[u8],
    fn_ptr_vars: &FnPtrVars,
    is_cpp: bool,
) -> (u8, String) {
    let Some(function) = call.child_by_field_name("function") else {
        return (CallMeta::FLAG_DIRECT, String::new());
    };
    let pos = start_pos(call);

    match function.kind() {
        "identifier" => {
            let name = node_text(function, source).unwrap_or_default();
            if let Some(ty) = fn_ptr_vars.type_at(name, pos) {
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
                if let Some(ty) = fn_ptr_vars.type_at(obj_name.trim_start_matches('*'), pos) {
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
                    if let Some(ty) = fn_ptr_vars.type_at(var_name, pos) {
                        return (CallMeta::FLAG_DYNAMIC_DISPATCH, ty.to_string());
                    }
                }
            }
            (CallMeta::FLAG_DIRECT, String::new())
        }
        _ => (CallMeta::FLAG_DIRECT, String::new()),
    }
}

/// The scope every name is visible in that no function or lambda declares.
const FILE_SCOPE: PosRange = ((0, 0), (u32::MAX, u32::MAX));

/// Declared types of the C/C++ names indirect-call detection reads, each
/// visible in its own scope: the function or lambda of a parameter or
/// local, the whole file otherwise. A `None` type marks a declaration the
/// detector does not track; it still hides a same-name one further out, so
/// one function's parameter never types another function's call.
#[derive(Debug, Default)]
pub struct FnPtrVars {
    decls: FxHashMap<String, Vec<(PosRange, Option<String>)>>,
}

impl FnPtrVars {
    pub fn declare(&mut self, name: String, scope: PosRange, ty: Option<String>) {
        self.decls.entry(name).or_default().push((scope, ty));
    }

    /// The type of `name` at `pos`, from the innermost scope that declares
    /// it. At an equal scope a tracked type wins over an untracked one, and
    /// the later of two tracked ones wins.
    fn type_at(&self, name: &str, pos: Pos) -> Option<&str> {
        let mut best: Option<(&Option<String>, (u32, i64))> = None;
        for (scope, ty) in self.decls.get(name)? {
            if !(scope.0 <= pos && pos <= scope.1) {
                continue;
            }
            let width = range_width(*scope);
            if best.is_none_or(|(_, w)| width < w || (width == w && ty.is_some())) {
                best = Some((ty, width));
            }
        }
        best?.0.as_deref()
    }
}

/// Record each C/C++ variable declaration and parameter in `vars`: tracked
/// with its type when it is a pointer or struct-like shape, untracked
/// otherwise. A prototype's parameter names nothing outside it and is left
/// out; so is a prototype, which declares a function, not a variable.
pub fn collect_c_cpp_fn_ptr_vars(root: Node<'_>, source: &[u8], vars: &mut FnPtrVars) {
    let depths = root.has_error().then(|| brace_depths(source));
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "declaration" | "parameter_declaration") {
            if let Some((scope, (var_name, ty))) =
                c_decl_scope(n, depths.as_deref()).zip(c_var_type_pair(n, source))
            {
                vars.declare(var_name, scope, ty);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// The range a C/C++ declaration is visible in: its function or lambda,
/// else the file. `None` for a parameter of a prototype, a member
/// declaration or a function-pointer type, which no call can name.
///
/// `depths` are the source's brace depths, read when the tree has errors.
/// Recovery can let a function body swallow the class members after it
/// (`#ifdef` groups that split an `else {` from its `}`): a declaration
/// the source's braces put outside that body belongs to the class around
/// it, else to the file.
fn c_decl_scope(decl: Node<'_>, depths: Option<&[(usize, i32)]>) -> Option<PosRange> {
    let is_param = decl.kind() == "parameter_declaration";
    let mut lifted = false;
    let mut cur = decl.parent();
    while let Some(p) = cur {
        match p.kind() {
            "function_definition" | "lambda_expression" if !lifted => {
                if is_param || !swallowed_by_body(decl, p, depths) {
                    return Some(node_range(p));
                }
                lifted = true;
            }
            "class_specifier" | "struct_specifier" | "union_specifier" if lifted => {
                return Some(node_range(p));
            }
            "declaration" | "field_declaration" | "type_definition" if is_param => return None,
            _ => {}
        }
        cur = p.parent();
    }
    (!is_param).then_some(FILE_SCOPE)
}

/// Whether `decl` sits inside `function`'s body in the tree but not in the
/// source: its brace depth is no deeper than the body's own `{`.
fn swallowed_by_body(decl: Node<'_>, function: Node<'_>, depths: Option<&[(usize, i32)]>) -> bool {
    let (Some(depths), Some(body)) = (depths, function.child_by_field_name("body")) else {
        return false;
    };
    body.start_byte() < decl.start_byte()
        && depth_at(depths, decl.start_byte()) <= depth_at(depths, body.start_byte())
}

/// The name a C/C++ declaration or parameter binds, with its type text when
/// detection tracks it: a pointer (`list *l`, `char **argv`, `int
/// (*fp)(int)`) or a struct-like type (`struct s`, `*_ops`, `*_t`). The
/// declarator node decides pointer-ness, never the spacing around `*`.
/// A variable a lambda initialises is tracked too: `f(1)` then calls
/// through the variable, never a function named `f`.
/// `None` for a prototype or a method declaration, which binds no variable.
fn c_var_type_pair(decl: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    let ty = node_text(decl.child_by_field_name("type")?, source)?;
    let declarator = decl.child_by_field_name("declarator")?;
    let (name, pointer) = c_declared_name(declarator)?;
    let var_name = node_text(name, source)?.to_string();
    let lambda = declarator.kind() == "init_declarator"
        && declarator
            .child_by_field_name("value")
            .is_some_and(|v| v.kind() == "lambda_expression");
    // A deduced `auto` names no type; only a lambda variable keeps it, as the
    // marker of a call through a variable.
    let deduced = matches!(ty, "auto" | "decltype(auto)");
    let tracked = lambda
        || (!deduced
            && (pointer || ty.contains("struct ") || ty.contains("_ops") || ty.ends_with("_t")));
    Some((var_name, tracked.then(|| ty.to_string())))
}

/// The identifier a C/C++ variable declarator binds, and whether a pointer
/// declarator wraps it. A function declarator binds a variable only as a
/// function pointer (`(*fp)(int)`); any other one is a prototype.
fn c_declared_name(declarator: Node<'_>) -> Option<(Node<'_>, bool)> {
    let mut n = declarator;
    let mut pointer = false;
    loop {
        n = match n.kind() {
            "identifier" => return Some((n, pointer)),
            "init_declarator" | "array_declarator" => n.child_by_field_name("declarator")?,
            "pointer_declarator" => {
                pointer = true;
                n.child_by_field_name("declarator")?
            }
            "reference_declarator" | "parenthesized_declarator" => n.named_child(0)?,
            "function_declarator" => n
                .child_by_field_name("declarator")
                .filter(|d| d.kind() == "parenthesized_declarator")?,
            _ => return None,
        };
    }
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
    call_sites: &CallSiteIndex,
) -> Vec<RawCallMeta> {
    let mut out = Vec::new();
    detect_js_ts_node(root, source, nodes, param_names, call_sites, &mut out);
    out
}

fn detect_js_ts_node(
    node: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    param_names: &FxHashSet<String>,
    call_sites: &CallSiteIndex,
    out: &mut Vec<RawCallMeta>,
) {
    if node.kind() == "call_expression" {
        let (flags, dispatch_type) = classify_js_call(node, source, param_names);
        if flags & CallMeta::FLAG_DIRECT == 0 {
            record_indirect(node, flags, &dispatch_type, nodes, call_sites, out);
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        detect_js_ts_node(child, source, nodes, param_names, call_sites, out);
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
    call_sites: &CallSiteIndex,
) -> Vec<RawCallMeta> {
    let mut out = Vec::new();
    detect_python_node(root, source, nodes, param_names, call_sites, &mut out);
    out
}

fn detect_python_node(
    node: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    param_names: &FxHashSet<String>,
    call_sites: &CallSiteIndex,
    out: &mut Vec<RawCallMeta>,
) {
    if node.kind() == "call" {
        let (flags, dispatch_type) = classify_python_call(node, source, param_names);
        if flags & CallMeta::FLAG_DIRECT == 0 {
            record_indirect(node, flags, &dispatch_type, nodes, call_sites, out);
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        detect_python_node(child, source, nodes, param_names, call_sites, out);
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
