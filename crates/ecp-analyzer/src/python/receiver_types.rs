//! Receiver typing for Python call and member-read extraction.
//!
//! The type of a `name.member` receiver comes from the receiver core: this
//! file's [`LocalTypes`] holds the [`FileReceivers`] that `receiver_lower`
//! solved for the file, and both the call extractor and the method-value
//! extractor read it, so `obj.m()` and `map(obj.m, xs)` reach one verdict.
//! A receiver the core does not track (a module name, an import, a class of
//! this file) keeps the rules below.
//!
//! A receiver that names a class of the same file and no import or local
//! binds (`Util.make()`) emits `Util.make`, as a typed receiver does. It
//! cannot reuse [`CallSite::from_receiver`]: for Python that site means a
//! `from`-imported name, which the resolver binds as a submodule, else as a
//! type it names, else by its bare member through the caller's own type and
//! bases.

use super::path_literals::build_raw_path_literal;
use super::receiver_lower::PythonLowering;
use crate::calls::{
    attach_to_enclosing, is_call_callee, is_same_kind_field, node_range, start_pos,
    unparenthesized, CallSiteIndex, Pos,
};
use crate::dynamic_imports::{
    binding_mentions, is_field, may_load, rebound, synthetic_imports, DynamicBinding,
    DynamicImports,
};
use crate::framework_helpers::strip_python_string_quotes;
use crate::receiver::{analyze_file, FileReceivers};
use ecp_core::analyzer::types::{
    BlindSpot, CallSite, RawNode, RawPathLiteral, RawSqlRef, ReadIntent,
};
use ecp_core::graph::NodeKind;
use rustc_hash::FxHashMap;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::Node;

/// A load assigned in one scope: the assigned name, its statement, and the
/// module specifier.
type ScopeLoad<'t, 's> = (Node<'t>, Node<'t>, &'s str);

/// What the Calls hook reads about one file: the receiver states the core
/// solved, and the names its imports bind.
#[derive(Debug, Default)]
pub struct LocalTypes {
    receivers: FileReceivers,
    /// A `from` binding may be an object; only a module import proves that
    /// its receiver names a namespace for import-scoped resolution.
    imported: HashMap<String, ImportedBinding>,
}

#[derive(Debug, Clone, Copy)]
enum ImportedBinding {
    Module,
    Symbol,
}

impl LocalTypes {
    fn imported_receiver(&self, receiver: Node<'_>, source: &[u8]) -> Option<ImportedBinding> {
        let mut root = receiver;
        while root.kind() == "attribute" {
            root = root.child_by_field_name("object")?;
        }
        if root.kind() != "identifier" || self.receivers.receiver_at(root).is_some() {
            return None;
        }
        self.imported.get(root.utf8_text(source).ok()?).copied()
    }

    /// The budget blind spots of the receiver analysis.
    pub fn take_blind_spots(&mut self) -> Vec<BlindSpot> {
        std::mem::take(&mut self.receivers.blind_spots)
    }
}

/// Solve the receiver types of every function in the file, and collect every
/// name an `import` statement binds.
pub fn collect_local_types(root: Node<'_>, source: &[u8], file: &Path) -> LocalTypes {
    let mut imported = HashMap::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "import_statement" | "import_from_statement" => {
                collect_import_bindings(n, source, &mut imported);
                continue;
            }
            // No statement nests inside these.
            "expression_statement" | "assignment" | "return_statement" | "string" | "comment" => {
                continue;
            }
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    LocalTypes {
        receivers: analyze_file(&PythonLowering, &(), root, source, file),
        imported,
    }
}

/// The local names one import statement binds: `import a.b` → `a`,
/// `import a as x` / `from m import n as x` → `x`, `from m import n` → `n`.
fn collect_import_bindings(
    stmt: Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, ImportedBinding>,
) {
    let mut c = stmt.walk();
    for name in stmt.children_by_field_name("name", &mut c) {
        let bound = match name.kind() {
            "aliased_import" => name.child_by_field_name("alias"),
            "dotted_name" => name.named_child(0),
            _ => None,
        };
        if let Some(text) = bound.and_then(|b| b.utf8_text(source).ok()) {
            let binding = if stmt.kind() == "import_statement" {
                ImportedBinding::Module
            } else {
                ImportedBinding::Symbol
            };
            out.insert(text.to_string(), binding);
        }
    }
}

/// The module whose `import_module` function loads a module by name.
const LOADER_MODULE: &str = "importlib";
const LOADER_FUNCTION: &str = "import_module";

/// Whether `node` only reads the value of its name.
fn is_read_position(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "attribute" => is_field(parent, "object", node),
        "call" => is_field(parent, "function", node),
        "keyword_argument"
        | "named_expression"
        | "default_parameter"
        | "typed_default_parameter" => is_field(parent, "value", node),
        "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => {
            is_field(parent, "right", node)
        }
        "lambda" => is_field(parent, "body", node),
        "if_statement" | "elif_clause" | "while_statement" => is_field(parent, "condition", node),
        // A container reads its items when it is itself read: `del (m, x)`
        // and a destructuring target do not.
        "parenthesized_expression"
        | "tuple"
        | "list"
        | "set"
        | "expression_list"
        | "list_splat"
        | "dictionary_splat" => is_read_position(parent),
        "argument_list"
        | "assert_statement"
        | "await"
        | "binary_operator"
        | "boolean_operator"
        | "comparison_operator"
        | "conditional_expression"
        | "decorator"
        | "dictionary_comprehension"
        | "expression_statement"
        | "generator_expression"
        | "if_clause"
        | "interpolation"
        | "list_comprehension"
        | "not_operator"
        | "pair"
        | "raise_statement"
        | "return_statement"
        | "set_comprehension"
        | "slice"
        | "subscript"
        | "unary_operator"
        | "yield" => true,
        _ => false,
    }
}

/// Whether the identifier `node` names no variable: an attribute name, or the
/// module of a `from` import.
fn names_no_variable(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "attribute" => is_field(parent, "attribute", node),
        "dotted_name" => parent.parent().is_some_and(|g| {
            g.kind() == "import_from_statement" && is_field(g, "module_name", parent)
        }),
        _ => false,
    }
}

/// Whether the leaf `node` is an identifier that binds or writes its name.
/// Any mention that is not a plain read counts, so a parameter, a loop
/// target, an `import`, a `global` and a `del` all do.
fn binds_name(node: Node<'_>) -> bool {
    node.kind() == "identifier" && !names_no_variable(node) && !is_read_position(node)
}

/// The one identifier in the file under `root` that binds or writes `name`,
/// when exactly one does ([`binds_name`]).
pub(super) fn sole_binding<'t>(root: Node<'t>, name: &str, source: &[u8]) -> Option<Node<'t>> {
    match binding_mentions(root, &[name], source, binds_name)
        .get(name)?
        .as_slice()
    {
        [only] => Some(*only),
        _ => None,
    }
}

/// Where a loader name is bound: the byte range it is visible in, and the
/// byte offset from which it is bound.
struct LoaderBinding {
    scope: (usize, usize),
    from: usize,
}

impl LoaderBinding {
    fn covers(&self, call: Node<'_>) -> bool {
        self.scope.0 <= call.start_byte()
            && call.end_byte() <= self.scope.1
            && self.from <= call.start_byte()
    }
}

/// The binding of `name` (`importlib` or `import_module`) when exactly one
/// plain import statement binds it in the whole file and nothing else binds
/// or writes it: `import importlib`, `from importlib import import_module`.
/// An alias, a `from` import of a different module, a class-body import or a
/// second binding gives `None`.
fn loader_binding(root: Node<'_>, source: &[u8], name: &str) -> Option<LoaderBinding> {
    let mention = sole_binding(root, name, source)?;
    let dotted = mention.parent().filter(|p| p.kind() == "dotted_name")?;
    let statement = dotted.parent()?;
    let plain = match (name, statement.kind()) {
        (LOADER_MODULE, "import_statement") => dotted
            .named_child(0)
            .is_some_and(|first| first.id() == mention.id()),
        (LOADER_FUNCTION, "import_from_statement") => {
            !is_field(statement, "module_name", dotted)
                && statement
                    .child_by_field_name("module_name")
                    .and_then(|module| module.utf8_text(source).ok())
                    == Some(LOADER_MODULE)
        }
        _ => false,
    };
    if !plain {
        return None;
    }
    let mut scope = (root.start_byte(), root.end_byte());
    let mut ancestor = statement.parent();
    while let Some(a) = ancestor {
        match a.kind() {
            "function_definition" => {
                scope = (a.start_byte(), a.end_byte());
                break;
            }
            "class_definition" => return None,
            _ => ancestor = a.parent(),
        }
    }
    Some(LoaderBinding {
        scope,
        from: statement.end_byte(),
    })
}

/// The module name a string literal spells when it is an absolute dotted name
/// written without prefix, escape or interpolation.
fn literal_module_name(literal: Node<'_>, source: &[u8]) -> Option<String> {
    if literal.kind() != "string" {
        return None;
    }
    let text = literal.utf8_text(source).ok()?;
    if !matches!(text.as_bytes().first(), Some(b'"' | b'\'')) {
        return None;
    }
    let inner = strip_python_string_quotes(text)?;
    (!inner.contains('\\')
        && inner
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphanumeric() || c == '_')))
    .then(|| inner.to_string())
}

/// A call `importlib.import_module("<lit>")` (`via_module`) or
/// `import_module("<lit>")` with one positional literal argument: its
/// specifier. Whether the callee really is the loader is [`loader_binding`]'s.
fn loader_call(call: Node<'_>, source: &[u8]) -> Option<(bool, String)> {
    let function = call.child_by_field_name("function")?;
    let text = |node: Node<'_>| node.utf8_text(source).ok();
    let via_module = match function.kind() {
        "attribute" => {
            let object = function.child_by_field_name("object")?;
            let attribute = function.child_by_field_name("attribute")?;
            if object.kind() != "identifier"
                || text(object) != Some(LOADER_MODULE)
                || text(attribute) != Some(LOADER_FUNCTION)
            {
                return None;
            }
            true
        }
        "identifier" if text(function) == Some(LOADER_FUNCTION) => false,
        _ => return None,
    };
    let arguments = call.child_by_field_name("arguments")?;
    if arguments.kind() != "argument_list" || arguments.named_child_count() != 1 {
        return None;
    }
    Some((
        via_module,
        literal_module_name(arguments.named_child(0)?, source)?,
    ))
}

/// A literal load that is the whole right side of `name = <load>`, a direct
/// statement of a function or module body: its scope, name and statement end.
fn direct_assignment(call: Node<'_>) -> Option<(Node<'_>, Node<'_>, Node<'_>)> {
    let assignment = call.parent().filter(|a| {
        a.kind() == "assignment"
            && is_field(*a, "right", call)
            && a.child_by_field_name("type").is_none()
    })?;
    let target = assignment
        .child_by_field_name("left")
        .filter(|left| left.kind() == "identifier")?;
    let statement = assignment
        .parent()
        .filter(|s| s.kind() == "expression_statement")?;
    let container = statement.parent()?;
    let scope = match container.kind() {
        "module" => container,
        "block" => container
            .parent()
            .filter(|f| f.kind() == "function_definition" && is_field(*f, "body", container))?,
        _ => return None,
    };
    Some((scope, target, statement))
}

/// Collect the synthetic imports of every literal
/// `importlib.import_module("<absolute name>")`, and bind the names that hold
/// a loaded module ([`DynamicImports`]).
///
/// A call counts only when `importlib` (or `import_module`) is bound by one
/// plain import that precedes it and nothing else in the file binds the name.
/// A name binds only when its assignment `m = <load>` is a direct statement
/// of a function or module body and `m` is declared or written nowhere else
/// in that scope's subtree. A use before the assignment never binds. Every
/// other shape (`__import__`, a relative name, a `package` argument, a
/// non-literal argument) keeps today's result.
pub fn collect_dynamic_imports(root: Node<'_>, source: &[u8]) -> DynamicImports {
    // The walk below costs a pass over the tree, and every load spells its loader.
    if !may_load(source, &[LOADER_FUNCTION]) {
        return DynamicImports::default();
    }
    let mut loads: Vec<(Node<'_>, bool, String)> = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            // A star import may rebind any name, loader included.
            "wildcard_import" => return DynamicImports::default(),
            "call" => {
                if let Some((via_module, specifier)) = loader_call(n, source) {
                    loads.push((n, via_module, specifier));
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }

    let binding_of = |via_module: bool| {
        let name = if via_module {
            LOADER_MODULE
        } else {
            LOADER_FUNCTION
        };
        loads
            .iter()
            .any(|(_, via, _)| *via == via_module)
            .then(|| loader_binding(root, source, name))
            .flatten()
    };
    let (module_binding, function_binding) = (binding_of(true), binding_of(false));
    loads.retain(|(call, via_module, _)| {
        let binding = if *via_module {
            &module_binding
        } else {
            &function_binding
        };
        binding.as_ref().is_some_and(|b| b.covers(*call))
    });
    if loads.is_empty() {
        return DynamicImports::default();
    }
    loads.sort_by_key(|(call, ..)| call.start_byte());
    let (imports, aliases) = synthetic_imports(loads.iter().map(|(.., spec)| spec.as_str()));

    // One walk per scope checks every name assigned a load there.
    let mut scopes: Vec<(Node<'_>, Vec<ScopeLoad<'_, '_>>)> = Vec::new();
    for (call, _, specifier) in &loads {
        let Some((scope, target, statement)) = direct_assignment(*call) else {
            continue;
        };
        let entry = (target, statement, specifier.as_str());
        match scopes.iter_mut().find(|(s, _)| s.id() == scope.id()) {
            Some((_, entries)) => entries.push(entry),
            None => scopes.push((scope, vec![entry])),
        }
    }
    let mut bindings = Vec::new();
    for (scope, entries) in &scopes {
        let names: Vec<&str> = entries
            .iter()
            .filter_map(|(target, ..)| target.utf8_text(source).ok())
            .collect();
        let mentions = binding_mentions(*scope, &names, source, binds_name);
        for (target, statement, specifier) in entries {
            let Ok(name) = target.utf8_text(source) else {
                continue;
            };
            if !rebound(mentions.get(name), (target.start_byte(), target.end_byte())) {
                bindings.push(DynamicBinding {
                    name: name.to_string(),
                    range: (node_range(*statement).1, node_range(*scope).1),
                    alias: aliases[*specifier].clone(),
                    member: None,
                });
            }
        }
    }
    DynamicImports {
        imports,
        bindings,
        loads: loads.iter().map(|(call, ..)| node_range(*call)).collect(),
    }
}

/// Walk the Python AST once, attaching callees to their holders (`calls::attach_to_enclosing`)
/// (with receiver-type binding) and collecting path-shaped string literals
/// as `RawPathLiteral` side-table entries.
///
/// Calls: identifiers/attributes are handled here; other call-target shapes
/// (subscript, lambda, ...) emit no edge, matching the previous catch-all
/// behavior's "last identifier segment" rule for those rare cases.
///
/// Path literals and SQL refs: every `string` node is fed through
/// `path_literals::build_raw_path_literal` (which itself filters out
/// f-strings by checking for `interpolation` children) and through
/// `sql_literal::is_sql_shaped`/`parse_tables` for SQL extraction.
pub fn extract_python_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    locals: &LocalTypes,
    dynamic: &DynamicImports,
    call_sites: &mut CallSiteIndex,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let classes: HashSet<String> = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Enum))
        .map(|n| n.name.clone())
        .collect();
    let mut namespaces = ClassNamespaces::default();
    let mut stack: Vec<(Node<'_>, Option<Node<'_>>, Option<Node<'_>>)> = vec![(root, None, None)];
    while let Some((n, parent, class)) = stack.pop() {
        match n.kind() {
            "call" => {
                if let Some(callee) = python_callee_name(n, source, locals, dynamic, &classes) {
                    if let Some(site) = attach_to_enclosing(n, source, callee, nodes) {
                        bind_class_body_call(n, class, source, site, nodes, &mut namespaces);
                        call_sites.insert(n.id(), site);
                    }
                }
            }
            "attribute" => {
                if let Some(entry) = bound_method_value(n, parent, source, locals) {
                    push_member_read(n, source, entry, nodes);
                }
            }
            "string" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                // Skip f-strings (they have `interpolation` children).
                let has_interpolation = {
                    let mut c = n.walk();
                    let x = n.children(&mut c).any(|ch| ch.kind() == "interpolation");
                    x
                };
                if !has_interpolation {
                    let raw_bytes = &source[n.start_byte()..n.end_byte()];
                    if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                        if let Some(value) = strip_python_string_quotes(raw) {
                            if let Some(mut sql_ref) =
                                crate::sql_literal::try_sql_ref(value, n, (None, None))
                            {
                                (sql_ref.enclosing_symbol, sql_ref.enclosing_owner) =
                                    enclosing_symbol_and_owner(n, source);
                                sql_refs.push(sql_ref);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        let (special, special_scope, other_scope) = child_scopes(n, class);
        let mut c = n.walk();
        for child in n.children(&mut c) {
            let scope = if Some(child) == special {
                special_scope
            } else {
                other_scope
            };
            stack.push((child, Some(n), scope));
        }
    }
    (path_literals, sql_refs)
}

/// The `field_reads` entry of `obj.m` in value position (`map(obj.m, xs)`,
/// `f = self.m`, `c.total`) where the receiver core types `obj`: a
/// [`MemberRead`](ecp_core::analyzer::types::MemberRead) of `T.m`. Pass 2
/// decides the edge from the target: a property accessor is a read, any
/// other method a function reference, a data attribute nothing. A callee, the
/// object of a longer attribute, an assignment or `del` target and a receiver
/// of unproven type give no entry: a method value never guesses.
fn bound_method_value(
    attribute: Node<'_>,
    parent: Option<Node<'_>>,
    source: &[u8],
    locals: &LocalTypes,
) -> Option<String> {
    let object = attribute
        .child_by_field_name("object")
        .filter(|object| object.kind() == "identifier")?;
    let member = attribute
        .child_by_field_name("attribute")
        .filter(|member| member.kind() == "identifier")?;
    let parent = parent?;
    let written = match parent.kind() {
        "assignment" | "augmented_assignment" => {
            parent.child_by_field_name("left") == Some(attribute)
        }
        "delete_statement" => true,
        _ => false,
    };
    if written
        || is_same_kind_field(attribute, parent, "object")
        || is_call_callee(attribute, Some(parent))
    {
        return None;
    }
    locals
        .receivers
        .receiver_at(object)?
        .member_read_entry(member.utf8_text(source).ok()?, ReadIntent::Unknown)
}

/// A bare call that a class body makes outside every function calls the
/// member the class defined above it: Python looks a name up in the class
/// namespace first, so `x = target()` in `class T` after `def target()`
/// calls `T.target`, not a module-level `target`. Rewrites the recorded
/// callee to `T.target` then. `class` is the class namespace the call
/// evaluates in ([`child_scopes`]); a method's decorator and parameter
/// defaults evaluate there too, so a callable holder does not exclude it.
fn bind_class_body_call<'s>(
    call: Node<'_>,
    class: Option<Node<'_>>,
    source: &'s [u8],
    (holder, idx): (usize, u32),
    nodes: &mut [RawNode],
    namespaces: &mut ClassNamespaces<'s>,
) {
    let Some(class) = class else {
        return;
    };
    let bare = call
        .child_by_field_name("function")
        .map(unparenthesized)
        .is_some_and(|f| f.kind() == "identifier");
    let Some(owner) = bare
        .then(|| class.child_by_field_name("name"))
        .flatten()
        .and_then(|name| name.utf8_text(source).ok())
    else {
        return;
    };
    let call_name = &mut nodes[holder].calls[idx as usize];
    if namespaces.binds_before(class, call_name, start_pos(call), source) {
        *call_name = format!("{owner}.{call_name}");
    }
}

/// The names each class body binds, computed once per class: the end of the
/// first statement that binds the name. A call binds the class member when
/// that statement ends before the call starts.
#[derive(Default)]
struct ClassNamespaces<'s> {
    first_binding_end: FxHashMap<usize, FxHashMap<&'s str, Pos>>,
}

impl<'s> ClassNamespaces<'s> {
    fn binds_before(&mut self, class: Node<'_>, name: &str, pos: Pos, source: &'s [u8]) -> bool {
        self.first_binding_end
            .entry(class.id())
            .or_insert_with(|| {
                let mut names = FxHashMap::default();
                if let Some(body) = class.child_by_field_name("body") {
                    collect_bindings(body, source, &mut names);
                }
                names
            })
            .get(name)
            .is_some_and(|end| *end < pos)
    }
}

fn note_binding<'s>(
    names: &mut FxHashMap<&'s str, Pos>,
    name: Node<'_>,
    source: &'s [u8],
    end: Pos,
) {
    if let Ok(text) = name.utf8_text(source) {
        let first = names.entry(text).or_insert(end);
        *first = (*first).min(end);
    }
}

/// The names `target` binds: a bare name, or the names inside a tuple, list
/// or starred pattern. An attribute or subscript target binds no name.
fn note_targets<'s>(
    names: &mut FxHashMap<&'s str, Pos>,
    target: Node<'_>,
    source: &'s [u8],
    end: Pos,
) {
    match target.kind() {
        "identifier" => note_binding(names, target, source, end),
        "pattern_list" | "tuple_pattern" | "list_pattern" | "list_splat_pattern"
        | "as_pattern_target" => {
            let mut cursor = target.walk();
            for child in target.named_children(&mut cursor) {
                note_targets(names, child, source, end);
            }
        }
        _ => {}
    }
}

/// Records what the statements under `node` bind in the class namespace: a
/// `def` or `class`, an assignment that has a value (a bare annotation
/// `x: int` binds nothing), an import, and a `for` or `with` target, also
/// inside `if`, `try`, `with`, `for` and `while`. A nested `def` or `class`
/// body is its own namespace, so the walk stops at it.
fn collect_bindings<'s>(node: Node<'_>, source: &'s [u8], names: &mut FxHashMap<&'s str, Pos>) {
    let end = node_range(node).1;
    match node.kind() {
        "function_definition" | "class_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                note_binding(names, name, source, end);
            }
        }
        "decorated_definition" => {
            if let Some(definition) = node.child_by_field_name("definition") {
                collect_bindings(definition, source, names);
            }
        }
        "expression_statement" => {
            let mut assignment = node.named_child(0).filter(|e| e.kind() == "assignment");
            while let Some(current) = assignment {
                let Some(right) = current.child_by_field_name("right") else {
                    break;
                };
                if let Some(left) = current.child_by_field_name("left") {
                    note_targets(names, left, source, end);
                }
                assignment = Some(right).filter(|r| r.kind() == "assignment");
            }
        }
        "import_statement" | "import_from_statement" => {
            let mut cursor = node.walk();
            for imported in node.children_by_field_name("name", &mut cursor) {
                let bound = match imported.kind() {
                    "aliased_import" => imported.child_by_field_name("alias"),
                    _ => imported.named_child(0),
                };
                if let Some(bound) = bound {
                    note_binding(names, bound, source, end);
                }
            }
        }
        "with_item" => {
            if let Some(alias) = node
                .child_by_field_name("value")
                .filter(|value| value.kind() == "as_pattern")
                .and_then(|value| value.child_by_field_name("alias"))
            {
                note_targets(names, alias, source, end);
            }
        }
        "for_statement" => {
            if let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) {
                note_targets(names, left, source, node_range(right).1);
            }
            collect_children(node, source, names);
        }
        "block"
        | "if_statement"
        | "elif_clause"
        | "else_clause"
        | "try_statement"
        | "except_clause"
        | "except_group_clause"
        | "finally_clause"
        | "with_statement"
        | "with_clause"
        | "while_statement" => collect_children(node, source, names),
        _ => {}
    }
}

fn collect_children<'s>(node: Node<'_>, source: &'s [u8], names: &mut FxHashMap<&'s str, Pos>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_bindings(child, source, names);
    }
}

/// How the children of `n` split the class namespace, given the one `n`
/// itself evaluates in: `(child, its namespace, every other child's)`. A
/// class body opens the class namespace and a function or lambda body closes
/// it, while the base list, decorators, parameter defaults and annotations
/// keep the surrounding one. A comprehension evaluates only its outermost
/// iterable in the surrounding namespace.
fn child_scopes<'t>(
    n: Node<'t>,
    scope: Option<Node<'t>>,
) -> (Option<Node<'t>>, Option<Node<'t>>, Option<Node<'t>>) {
    match n.kind() {
        "class_definition" => (n.child_by_field_name("body"), Some(n), scope),
        "function_definition" | "lambda" => (n.child_by_field_name("body"), None, scope),
        "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => {
            let mut cursor = n.walk();
            let outermost = n
                .named_children(&mut cursor)
                .find(|c| c.kind() == "for_in_clause");
            (outermost, scope, None)
        }
        _ => (None, scope, scope),
    }
}

/// Records `entry` in the `field_reads` of the holder of `at`
/// ([`attach_to_enclosing`]), once. `calls.rs` finds the holder only through
/// its call attach helper, so the entry rides through `calls` and moves out
/// at once.
fn push_member_read(at: Node<'_>, source: &[u8], entry: String, nodes: &mut [RawNode]) {
    let Some((holder, _)) = attach_to_enclosing(at, source, entry, nodes) else {
        return;
    };
    if let Some(entry) = nodes[holder].calls.pop() {
        if !nodes[holder].field_reads.contains(&entry) {
            nodes[holder].field_reads.push(entry);
        }
    }
}

/// Climb from a string literal to find the innermost enclosing
/// `function_definition` (free function or method) and `class_definition`
/// (owner). Returns `(function_name, owner_class)`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_definition" if function_name.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "class_definition" if owner.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    owner =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            _ => {}
        }
        cur = n.parent();
    }
    (function_name, owner)
}

/// `classes`: the names of the classes this file declares.
fn python_callee_name(
    call: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    dynamic: &DynamicImports,
    classes: &HashSet<String>,
) -> Option<String> {
    let function = unparenthesized(call.child_by_field_name("function")?);
    match function.kind() {
        "identifier" => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
        "attribute" => {
            let attr = function.child_by_field_name("attribute")?;
            let attr_name =
                std::str::from_utf8(&source[attr.start_byte()..attr.end_byte()]).ok()?;
            if let Some(obj) = function.child_by_field_name("object") {
                if obj.kind() == "identifier" {
                    let obj_name =
                        std::str::from_utf8(&source[obj.start_byte()..obj.end_byte()]).ok()?;
                    let pos = start_pos(call);
                    if let Some(callee) = dynamic.member_callee(pos, obj_name, attr_name) {
                        return Some(callee);
                    }
                    if let Some(receiver) = locals.receivers.receiver_at(obj) {
                        return Some(receiver.call_entry(attr_name));
                    }
                }
                if is_super_call(obj, source) {
                    return Some(format!("{}.{attr_name}", CallSite::SUPER_RECEIVER));
                }
                // Only a module receiver (`widget.Widget()`) can name a class
                // to construct; any other untyped receiver calls a method,
                // and a class may be spelled in any case (`class widget`).
                match locals.imported_receiver(obj, source) {
                    Some(ImportedBinding::Module) => {
                        let mut segments = Vec::new();
                        let mut node = function;
                        while node.kind() == "attribute" {
                            segments.push(
                                node.child_by_field_name("attribute")?
                                    .utf8_text(source)
                                    .ok()?,
                            );
                            node = node.child_by_field_name("object")?;
                        }
                        segments.push(node.utf8_text(source).ok()?);
                        segments.reverse();
                        return Some(segments.join("."));
                    }
                    // A `from` binding may name a submodule, which only the
                    // resolver can tell from an object. Keep nested
                    // attributes in the receiver path so the resolver can
                    // distinguish `mod.client.m()` from `mod.m()`.
                    Some(ImportedBinding::Symbol) => {
                        return match attribute_receiver_path(obj, source) {
                            Some(receiver) => Some(CallSite::from_receiver(&receiver, attr_name)),
                            // `sym.attr.m()`: a member call on a value the import
                            // names; a bare `m` would read as receiverless.
                            None => Some(CallSite::untyped_member(attr_name)),
                        };
                    }
                    None => {
                        let class = (obj.kind() == "identifier")
                            .then(|| obj.utf8_text(source).ok())
                            .flatten()
                            .filter(|name| classes.contains(*name));
                        return Some(match class {
                            Some(class) => format!("{class}.{attr_name}"),
                            // `make().bar()`: the resolver types the receiver
                            // by the callee's return type or constructed class.
                            None if obj.kind() == "call" => {
                                let untyped = CallSite::untyped_member(attr_name);
                                // A callee that a parameter or local binds
                                // (`make = lambda: Beta()`) is a value, not
                                // the same-named function that would type it.
                                let calls_local = obj
                                    .child_by_field_name("function")
                                    .filter(|f| f.kind() == "identifier")
                                    .is_some_and(|f| locals.receivers.receiver_at(f).is_some());
                                if calls_local {
                                    return Some(untyped);
                                }
                                match CallSite::nested_chain(|| {
                                    python_callee_name(obj, source, locals, dynamic, classes)
                                }) {
                                    Some(inner) => CallSite::chained_member(&inner, &untyped),
                                    None => untyped,
                                }
                            }
                            None => CallSite::untyped_member(attr_name),
                        });
                    }
                }
            }
            Some(attr_name.to_string())
        }
        _ => None,
    }
}

/// The identifier-led attribute path of `node`, joined for an internal call
/// receiver. A slash is not valid in a Python identifier, so it preserves the
/// final call member separator in [`CallSite::from_receiver`].
fn attribute_receiver_path(mut node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut segments = Vec::new();
    while node.kind() == "attribute" {
        segments.push(
            node.child_by_field_name("attribute")?
                .utf8_text(source)
                .ok()?,
        );
        node = node.child_by_field_name("object")?;
    }
    if node.kind() != "identifier" {
        return None;
    }
    segments.push(node.utf8_text(source).ok()?);
    segments.reverse();
    Some(segments.join("/"))
}

/// Zero-argument `super()`: the receiver of a call through the caller
/// class's bases, which the resolver binds from its heritage. `super(C,
/// self)` starts the lookup after `C`, not after the caller class, so it
/// stays an untyped member call.
fn is_super_call(receiver: Node<'_>, source: &[u8]) -> bool {
    receiver.kind() == "call"
        && receiver
            .child_by_field_name("function")
            .is_some_and(|f| f.kind() == "identifier" && f.utf8_text(source) == Ok("super"))
        && receiver
            .child_by_field_name("arguments")
            .is_some_and(|args| args.named_child_count() == 0)
}
