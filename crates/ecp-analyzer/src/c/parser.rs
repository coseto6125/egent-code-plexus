use super::receiver_types::{
    collect_receiver_methods, declare_macro_bound_vars, extract_c_calls_and_path_literals,
    CReceiverMap,
};
use super::spec::CSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, node, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture, Op, ParseSpec, Step,
};
use crate::indirect_dispatch::{collect_c_cpp_fn_ptr_vars, detect_c_cpp_indirect, FnPtrVars};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use crate::preproc_fallback::{is_guard_spelling, Preproc, PreprocRows};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, LocalGraph, RawFunctionMeta, RawImport, RawNode};
use ecp_core::graph::{FileCategory, NodeKind};
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_c::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
/// True if `name` is a C reserved keyword that tree-sitter-c sometimes
/// mis-captures as an identifier when error-recovering from preprocessor
/// macros. Legal C code never names a variable with any of these, so
/// rejecting them only suppresses parse-recovery noise.
fn is_c_reserved_keyword(name: &str) -> bool {
    matches!(
        name,
        "void"
            | "char"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "signed"
            | "unsigned"
            | "_Bool"
            | "_Complex"
            | "_Imaginary"
            | "const"
            | "volatile"
            | "restrict"
            | "_Atomic"
            | "register"
            | "static"
            | "extern"
            | "auto"
            | "_Thread_local"
            | "struct"
            | "union"
            | "enum"
            | "typedef"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "default"
            | "break"
            | "continue"
            | "return"
            | "goto"
            | "sizeof"
            | "inline"
            | "_Static_assert"
            | "_Alignas"
            | "_Alignof"
    )
}

/// Returns true if `node` (a `function_definition` or a `declaration`) has a
/// `static` storage class specifier among its direct children.
pub(crate) fn has_static_specifier(node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).any(|child| {
        child.kind() == "storage_class_specifier"
            && source
                .get(child.start_byte()..child.end_byte())
                .and_then(|b| std::str::from_utf8(b).ok())
                == Some("static")
    });
    result
}

/// Extract a type-annotation string for a param/field/variable declaration by
/// slicing the source from the outer declaration's start byte to the start of
/// the identifier name. This preserves the original spelling — qualifiers
/// (`const`, `static`), storage class, pointer / array operators (`*`, `[]`),
/// and the type specifier itself.
///
/// Convention (documented per task D3):
/// - **Pointer spacing** is preserved as-written. `const char* s` yields
///   `"const char*"`; `int * p` yields `"int *"`. Source is source of truth.
/// - **Qualifier inclusion** is YES. `static const int N` yields
///   `"static const int"`. Downstream consumers can strip storage-class
///   words if they want a bare type; the analyzer surfaces the full
///   declaration prefix because it's the most information-preserving and
///   cheap to compute (one byte-range slice).
/// - **`auto`** is preserved literally; the analyzer does no type
///   deduction. `auto x = 5;` yields `"auto"`.
pub(crate) fn slice_type_before(
    decl: tree_sitter::Node<'_>,
    name: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<String> {
    let start = decl.start_byte();
    let end = name.start_byte();
    if end <= start {
        return None;
    }
    let text = |from: usize, to: usize| std::str::from_utf8(source.get(from..to)?).ok();
    // A specifier with a body (`struct { ... } x`, `struct Foo { ... } x`)
    // names the type by its keyword and tag: the body is no type text.
    let body = decl
        .child_by_field_name("type")
        .and_then(|t| t.child_by_field_name("body"))
        .filter(|b| start <= b.start_byte() && b.end_byte() <= end);
    let slice = match body {
        Some(b) => format!(
            "{} {}",
            text(start, b.start_byte())?.trim_end(),
            text(b.end_byte(), end)?.trim_start()
        ),
        None => text(start, end)?.to_string(),
    };
    Some(slice.trim().to_string()).filter(|s| !s.is_empty())
}

/// The type of `name` in a declaration that may hold several declarators
/// (`int *p, q;`). The first declarator keeps [`slice_type_before`]; a later
/// one takes the shared specifier before the first declarator plus its own
/// pointer prefix, so `q` is `int`, never `int *p,`.
pub(crate) fn declarator_type(
    decl: tree_sitter::Node<'_>,
    name: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<String> {
    let mut top = name;
    while let Some(p) = top.parent() {
        if p.id() == decl.id() {
            break;
        }
        // A name inside parentheses is a function-pointer member
        // (`void (*open)(int)`): the text before it is no type.
        if p.kind() == "parenthesized_declarator" {
            return None;
        }
        top = p;
    }
    let first = decl.child_by_field_name("declarator")?;
    if first.id() == top.id() {
        return slice_type_before(decl, name, source);
    }
    let shared = slice_type_before(decl, first, source)?;
    let prefix = std::str::from_utf8(source.get(top.start_byte()..name.start_byte())?)
        .ok()?
        .trim();
    Some(if prefix.is_empty() {
        shared
    } else {
        format!("{shared} {prefix}")
    })
}

/// The alias a `typedef` declarator names: the `type_identifier` inside its
/// pointer, C++ reference, array, function and parenthesized declarators.
///
/// - `typedef int Counter;`        → the `type_identifier` itself
/// - `typedef char** StrArray;`    → inside `pointer_declarator`
/// - `typedef int IntArr[10];`     → inside `array_declarator`
/// - `typedef int (*FnPtr)(int);`  → `function_declarator >
///   parenthesized_declarator > pointer_declarator > type_identifier`
///
/// `None` for an abstract declarator, which names none.
pub(crate) fn typedef_alias(n: Node<'_>) -> Option<Node<'_>> {
    match n.kind() {
        "type_identifier" => (n.end_byte() > n.start_byte()).then_some(n),
        "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "function_declarator"
        | "parenthesized_declarator" => {
            let mut cursor = n.walk();
            let found = n.named_children(&mut cursor).find_map(typedef_alias);
            found
        }
        _ => None,
    }
}

/// Heuristic: detect `#define FOO_H` / `#define FOO_H 1` style include
/// guards so they don't drown out real bindings.
///
/// Conservative: matches when the name is spelt like a guard
/// ([`is_guard_spelling`]) AND the body is absent or a bare `1`. Real
/// constants like `MAX_SIZE` (no suffix match) or `BUFFER_SIZE 4096` (body
/// not `1`) pass through.
fn is_include_guard(name: &str, body: Option<&str>) -> bool {
    is_guard_spelling(name.as_bytes()) && matches!(body.map(str::trim), None | Some("") | Some("1"))
}

/// True when `decl` sits at file scope: its parent chain up to the root
/// holds only preprocessor conditionals, `extern "C"` blocks and ERROR
/// nodes. Error recovery can put a function's locals straight under a
/// top-level ERROR, or under the root once an ERROR swallowed the body's
/// `{` (`rb_gen(...)` with no `;` before a function), so in a tree with
/// errors the source's brace depth decides; a declaration inside a body
/// node is never file scope. A comment in a multi-line `#define` ends its
/// `preproc_arg`, and the rest of the macro body parses as code: a
/// declaration that starts on a directive row is never file scope.
fn is_file_scope(decl: Node<'_>, f: &mut CFile, source: &[u8]) -> bool {
    if f.preproc
        .rows(source)
        .in_directive(decl.start_position().row)
    {
        return false;
    }
    let mut recovered = false;
    let mut cur = decl.parent();
    while let Some(p) = cur {
        match p.kind() {
            "translation_unit" => {
                recovered |= p.has_error();
                break;
            }
            "ERROR" => recovered = true,
            "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
            | "preproc_elifdef"
            | "linkage_specification" => {}
            "declaration_list"
                if p.parent()
                    .is_some_and(|g| g.kind() == "linkage_specification") => {}
            _ => return false,
        }
        cur = p.parent();
    }
    !recovered || f.preproc.depth_at(source, decl.start_byte()) == 0
}

/// True if the variable `name` that the file-scope `declaration` `decl`
/// declares is itself constant: the declaration is `constexpr`, or `const`
/// qualifies the name's own object. With no pointer declarator around the
/// name that is a `const` on the declaration (`const int N`); with one, it
/// is a `const` on the pointer declarator nearest the name (`T *const p`).
/// `static const T *p` points at constants, and `p` itself is mutable.
///
/// This is intentionally scoped to the `@var` use-site (file-scope
/// `declaration` nodes) of C and C++, so one declaration in a `.c` and a
/// `.h` gets one kind. Class fields use `field_declaration` and function
/// parameters use `parameter_declaration` — neither reaches this helper.
pub(crate) fn is_const_qualified(decl: Node<'_>, name: Node<'_>, source: &[u8]) -> bool {
    has_qualifier(decl, "constexpr", source)
        || has_qualifier(nearest_pointer(decl, name).unwrap_or(decl), "const", source)
}

/// Whether `node` has a `type_qualifier` child that reads `word`.
fn has_qualifier(node: Node<'_>, word: &str, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|child| {
        child.kind() == "type_qualifier" && child.utf8_text(source).is_ok_and(|t| t.trim() == word)
    });
    found
}

/// The pointer declarator nearest `name` inside `decl`, if any. `name` is
/// the identifier [`declared_variable`] reads from one of `decl`'s
/// declarators.
pub(crate) fn nearest_pointer<'t>(decl: Node<'t>, name: Node<'t>) -> Option<Node<'t>> {
    // Down from `decl`: `parent()` walks from the root on every call.
    let mut cursor = decl.walk();
    let mut cur = decl
        .children_by_field_name("declarator", &mut cursor)
        .find(|d| d.start_byte() <= name.start_byte() && name.end_byte() <= d.end_byte())?;
    let mut nearest = None;
    while cur.id() != name.id() {
        if cur.kind() == "pointer_declarator" {
            nearest = Some(cur);
        }
        cur = match cur.kind() {
            "reference_declarator" => cur.named_child(0)?,
            _ => cur.child_by_field_name("declarator")?,
        };
    }
    nearest
}

/// The empty statement `;` that ends `node`'s declaration after the
/// conditionals and comments between them, if any.
fn lone_semicolon_after(node: Node<'_>) -> Option<Node<'_>> {
    let mut next = node.next_named_sibling();
    while let Some(n) = next.filter(|n| n.kind().starts_with("preproc_") || n.kind() == "comment") {
        next = n.next_named_sibling();
    }
    next.filter(|n| n.kind() == "expression_statement" && n.named_child_count() == 0)
}

/// Whether the one error in `decl` is its missing `;`, and the `;` follows
/// after a conditional: an attribute macro sits between them
/// (`const char *je_malloc_conf` / `#ifndef _WIN32` / `JEMALLOC_ATTR(weak)`
/// / `#endif` / `;`, jemalloc `jemalloc.c:35`).
fn semicolon_after_conditional(decl: Node<'_>) -> bool {
    let mut cursor = decl.walk();
    let children: Vec<Node<'_>> = decl.children(&mut cursor).collect();
    let cut = children.split_last().is_some_and(|(last, rest)| {
        is_missing_token(*last, ";") && rest.iter().all(|c| !c.has_error())
    });
    cut && lone_semicolon_after(decl).is_some()
}

/// The `;` that ends a prototype tree-sitter-c left as an ERROR because a
/// macro follows its parameters (`void __libc_free(void *ptr)
/// PREALIAS(je_free);`, jemalloc `jemalloc.c:1210`): the ERROR holds a
/// `type` and one function declarator and nothing else, and the `;` is the
/// next statement. `None` for any other ERROR.
///
/// `child_by_field_name` finds no field on an ERROR node (it reads the
/// production's field map, which an ERROR lacks); the per-child field name
/// the query also matches on is still there.
fn recovered_prototype_end(error: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = error.walk();
    let mut types = 0;
    let mut declarators = 0;
    for (i, child) in error.named_children(&mut cursor).enumerate() {
        match child.kind() {
            "function_declarator" => declarators += 1,
            "storage_class_specifier" | "type_qualifier" | "comment" => {}
            _ if error.field_name_for_named_child(i as u32) == Some("type") => types += 1,
            _ => return None,
        }
    }
    (types == 1 && declarators == 1).then_some(())?;
    error
        .next_named_sibling()
        .filter(|n| n.kind() == "expression_statement" && n.named_child_count() == 0)
}

/// The `typedef` keyword that tree-sitter-c read as the declarator of the
/// declaration before `decl`, so `decl` is the rest of the typedef:
/// `TEST_END` / `typedef rb_tree(node_t) tree_t;` reads as `TEST_END
/// typedef` and `rb_tree(node_t) tree_t;` (jemalloc `test/unit/rb.c:1000`).
fn split_typedef_keyword<'tree>(decl: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    let prev = decl
        .prev_named_sibling()
        .filter(|p| p.kind() == "declaration")?;
    let mut cursor = prev.walk();
    let last = prev
        .children_by_field_name("declarator", &mut cursor)
        .last()?;
    (last.kind() == "identifier" && last.utf8_text(source) == Ok("typedef")).then_some(last)
}

/// The identifier a variable declarator names through pointer, C++
/// reference and array declarators (`*p`, `**pp`, `&r`, `a[4]`, `*names[]`,
/// `m[2][2]`); `None` for a function, parenthesized or qualified declarator.
pub(crate) fn declared_variable(declarator: Node<'_>) -> Option<Node<'_>> {
    match declarator.kind() {
        "identifier" => Some(declarator),
        "pointer_declarator" | "array_declarator" => {
            declared_variable(declarator.child_by_field_name("declarator")?)
        }
        "reference_declarator" => declared_variable(declarator.named_child(0)?),
        _ => None,
    }
}

/// Find the storage_class_specifier child with text `"extern"`.
fn extern_specifier<'a>(decl: Node<'a>, source: &[u8]) -> Option<Node<'a>> {
    let mut cursor = decl.walk();
    let found = decl.children(&mut cursor).find(|child| {
        child.kind() == "storage_class_specifier"
            && source
                .get(child.start_byte()..child.end_byte())
                .and_then(|b| std::str::from_utf8(b).ok())
                == Some("extern")
    });
    found
}

/// Find the bound identifier in an `extern` declaration. For
/// `extern int g_counter;` it's the plain `identifier`; for
/// `extern void func(int);` it's the `identifier` inside the
/// `function_declarator`. Pointer wrappers are skipped.
fn extern_bound_identifier<'a>(decl: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = decl.walk();
    for child in decl.named_children(&mut cursor) {
        match child.kind() {
            "identifier" => return Some(child),
            "function_declarator"
            | "pointer_declarator"
            | "array_declarator"
            | "init_declarator" => {
                if let Some(found) = extern_bound_identifier(child) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// Walk the translation unit and emit `RawImport` entries for the C "named
/// binding" constructs: `#define` macros (object-like and function-like) and
/// `extern` declarations. Include-guard `#define`s are filtered via
/// [`is_include_guard`]. A `typedef` is no import: its `Typedef` node carries
/// the aliased type ([`typedef_aliased_type`]).
///
/// `RawImport` shape per construct:
/// - `#define MAX 100`     → `{ source: "100", alias: Some("MAX"), imported_name: "MAX" }`
/// - `#define ADD(a,b) ..` → `{ source: "(a,b) ..", alias: Some("ADD"), imported_name: "ADD" }`
/// - `extern int g;`       → `{ source: "external", alias: Some("g"), imported_name: "g" }`
fn extract_named_bindings(
    root: Node<'_>,
    source: &[u8],
    rows: &PreprocRows,
    imports: &mut Vec<RawImport>,
) {
    // The stack holds children in reverse so each pop yields source order.
    let mut cursor = root.walk();
    let mut stack: Vec<Node<'_>> = root.named_children(&mut cursor).collect();
    stack.reverse();
    while let Some(node) = stack.pop() {
        match node.kind() {
            "preproc_def" => emit_object_macro_binding(node, source, rows, imports),
            "preproc_function_def" => emit_function_macro_binding(node, source, imports),
            "declaration" => emit_extern_binding(node, source, imports),
            // Descend into conditional preproc blocks so guarded defines
            // (and the typical pattern of `#ifdef X / typedef ... / #endif`)
            // still get visited.
            "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif" => {
                let mut c = node.walk();
                let children: Vec<Node<'_>> = node.named_children(&mut c).collect();
                stack.extend(children.into_iter().rev());
            }
            _ => {}
        }
    }
}

/// Classify an object-like `#define` body into a `BindingKind`.
///
/// Rules (in priority order):
/// 1. Empty body → `Flag`
/// 2. Numeric literal (decimal, hex, float, suffixed) or string literal → `Constant`
/// 3. Single C identifier → `Alias`
/// 4. Everything else (operators, parenthesized expressions, etc.) → `Macro`
fn classify_define_body(body: &str) -> BindingKind {
    let body = body.trim();
    if body.is_empty() {
        return BindingKind::Flag;
    }
    // Numeric literal: decimal/float with optional suffix, or hex.
    let is_numeric = {
        let s = body.trim_start_matches('-');
        (s.starts_with("0x") || s.starts_with("0X"))
            && s[2..]
                .trim_end_matches(['u', 'U', 'l', 'L'])
                .chars()
                .all(|c| c.is_ascii_hexdigit())
            || {
                let stripped = s.trim_end_matches(['u', 'U', 'l', 'L', 'f', 'F']);
                let mut parts = stripped.splitn(2, '.');
                let integer_part = parts.next().unwrap_or("");
                let frac_part = parts.next().unwrap_or("");
                !integer_part.is_empty()
                    && integer_part.chars().all(|c| c.is_ascii_digit())
                    && frac_part.chars().all(|c| c.is_ascii_digit())
            }
    };
    if is_numeric {
        return BindingKind::Constant;
    }
    // String literal.
    if body.starts_with('"') && body.ends_with('"') && body.len() >= 2 {
        return BindingKind::Constant;
    }
    // Single C identifier.
    if body
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false)
        && body.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return BindingKind::Alias;
    }
    BindingKind::Macro
}

/// The `*` prefix of a typedef alias whose declarator is a pointer chain
/// ending at the alias (`**` for `typedef char **A;`). `None` for any other
/// declarator shape, so a function-pointer alias keeps its return type.
fn pointer_prefix(alias_root: Node<'_>, source: &[u8]) -> Option<String> {
    let mut inner = alias_root;
    while inner.kind() == "pointer_declarator" {
        inner = inner.child_by_field_name("declarator")?;
    }
    let stars = std::str::from_utf8(source.get(alias_root.start_byte()..inner.start_byte())?)
        .ok()?
        .trim();
    (!stars.is_empty()).then(|| stars.to_string())
}

/// Whether the `type_definition` `def` is cut short by a macro call in its
/// type, so its parenthesized declarator holds a macro argument and never
/// the alias: the `;` is missing and the declarator is `(...)`.
/// tree-sitter-cpp reads `typedef ql_head(T) list_t;` as `typedef ql_head
/// (T)`, and both grammars read `typedef __attribute__((aligned(1))) T x;`
/// as `typedef __attribute__ ((aligned(1)))`.
pub(crate) fn is_cut_typedef(def: Node<'_>) -> bool {
    has_missing(def, ";")
        && def
            .child_by_field_name("declarator")
            .is_some_and(|d| d.kind() == "parenthesized_declarator")
}

/// Whether `node` has a missing `kind` token, which error recovery inserted.
pub(crate) fn has_missing(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| is_missing_token(c, kind));
    found
}

fn is_missing_token(node: Node<'_>, kind: &str) -> bool {
    node.is_missing() && node.kind() == kind
}

/// The alias of a cut-short typedef ([`is_cut_typedef`]) and the statement
/// that holds it, on the row the typedef ends: the lone identifier of an
/// expression statement (tree-sitter-cpp's `list_t;` after `typedef
/// ql_head(T)`) or the name the one declarator of a declaration binds,
/// through pointer and array declarators and past qualifiers (`T name;`,
/// `float v[4];`, `const T *p;` after `typedef
/// __attribute__((aligned(1)))`). `None` for any other shape: no alias is
/// guessed.
pub(crate) fn macro_typedef_tail(def: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let stmt = def
        .next_named_sibling()
        .filter(|s| s.start_position().row == def.end_position().row)?;
    let alias = match stmt.kind() {
        "expression_statement" if stmt.named_child_count() == 1 => stmt.named_child(0),
        "declaration" => lone_declarator(stmt).and_then(declared_variable),
        _ => None,
    }?;
    (alias.kind() == "identifier").then_some((alias, stmt))
}

/// The one declarator of the `declaration` `decl` when the rest of it is its
/// type, qualifiers, storage classes and attributes; `None` otherwise.
fn lone_declarator(decl: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = decl.walk();
    let mut declarator = None;
    for (i, child) in decl.named_children(&mut cursor).enumerate() {
        match decl.field_name_for_named_child(i as u32) {
            Some("type") => {}
            Some("declarator") if declarator.is_none() => declarator = Some(child),
            Some("declarator") => return None,
            _ if matches!(
                child.kind(),
                "type_qualifier" | "storage_class_specifier" | "attribute_specifier"
            ) => {}
            _ => return None,
        }
    }
    declarator
}

/// Whether the `declaration` `decl` is the tail of a cut-short typedef
/// before it ([`macro_typedef_tail`]): it names the alias, no variable.
pub(crate) fn is_typedef_tail(decl: Node<'_>) -> bool {
    decl.prev_named_sibling()
        .filter(|p| p.kind() == "type_definition" && is_cut_typedef(*p))
        .and_then(macro_typedef_tail)
        .is_some_and(|(_, stmt)| stmt.id() == decl.id())
}

/// The type a typedef `declarator` aliases: the text between `typedef` and
/// the first declarator, a struct body collapsed to its tag, plus the
/// declarator's own leading stars (`typedef char **A;` aliases `char **`,
/// `typedef void *(*F)(int);` the return type `void *`). A typedef cut short
/// by a macro call takes the type of the statement `tail` that holds its
/// alias. `None` when no type text is left.
fn typedef_aliased_type(
    td: Node<'_>,
    declarator: Option<Node<'_>>,
    tail: Option<Node<'_>>,
    source: &[u8],
) -> Option<String> {
    if tail.is_none() && td.kind() != "type_definition" {
        return None;
    }
    let (shared, declarator) = match tail {
        Some(stmt) => (
            stmt.child_by_field_name("type")?
                .utf8_text(source)
                .ok()?
                .trim()
                .to_string(),
            stmt.child_by_field_name("declarator"),
        ),
        None => {
            let first = td.child_by_field_name("declarator")?;
            let text = slice_type_before(td, first, source)?;
            // `__extension__ typedef ..` (glibc, kernel uapi) leads with the
            // GNU keyword the grammar allows before `typedef`.
            let shared = text
                .trim_start_matches("__extension__")
                .trim_start()
                .strip_prefix("typedef")?
                .trim()
                .to_string();
            (shared, declarator)
        }
    };
    if shared.is_empty() {
        return None;
    }
    Some(match declarator.and_then(|d| pointer_prefix(d, source)) {
        Some(stars) => format!("{shared} {stars}"),
        None => shared,
    })
}

fn emit_object_macro_binding(
    def: Node<'_>,
    source: &[u8],
    rows: &PreprocRows,
    imports: &mut Vec<RawImport>,
) {
    let mut cursor = def.walk();
    let mut name: Option<&str> = None;
    let mut body: Option<&str> = None;
    for child in def.named_children(&mut cursor) {
        match child.kind() {
            "identifier" if name.is_none() => {
                name = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
            }
            "preproc_arg" => {
                body = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    .ok()
                    .map(str::trim);
            }
            _ => {}
        }
    }
    let Some(name) = name else { return };
    // The node pass drops the `#define` of an include guard paired with its
    // `#ifndef`; a guard-spelt name with no `#ifndef` is no symbol either.
    if is_include_guard(name, body) || rows.is_guard_row(def.start_position().row) {
        return;
    }
    let body_str = body.unwrap_or("");
    let kind = classify_define_body(body_str);
    imports.push(driver::imports::raw(
        body_str.to_string(),
        name.to_string(),
        Some(name.to_string()),
        Some(kind),
    ));
}

fn emit_function_macro_binding(def: Node<'_>, source: &[u8], imports: &mut Vec<RawImport>) {
    let mut cursor = def.walk();
    let mut name: Option<&str> = None;
    let mut params: Option<&str> = None;
    let mut body: Option<&str> = None;
    for child in def.named_children(&mut cursor) {
        match child.kind() {
            "identifier" if name.is_none() => {
                name = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
            }
            "preproc_params" => {
                params = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
            }
            "preproc_arg" => {
                body = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    .ok()
                    .map(str::trim);
            }
            _ => {}
        }
    }
    let Some(name) = name else { return };
    // `source` carries `(params) body` so call sites can distinguish a
    // function-like macro from an object-like one without an extra flag.
    let combined = match (params, body) {
        (Some(p), Some(b)) => format!("{p} {b}"),
        (Some(p), None) => p.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => String::new(),
    };
    imports.push(driver::imports::raw(
        combined,
        name.to_string(),
        Some(name.to_string()),
        Some(BindingKind::Macro),
    ));
}

fn emit_extern_binding(decl: Node<'_>, source: &[u8], imports: &mut Vec<RawImport>) {
    if extern_specifier(decl, source).is_none() {
        return;
    }
    let Some(id_node) = extern_bound_identifier(decl) else {
        return;
    };
    let Ok(name) = std::str::from_utf8(&source[id_node.start_byte()..id_node.end_byte()]) else {
        return;
    };
    imports.push(driver::imports::raw(
        "external".to_string(),
        name.to_string(),
        Some(name.to_string()),
        Some(BindingKind::Alias),
    ));
}

pub struct CProvider {
    compiled: Compiled<CSpec>,
}

impl CProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for CProvider {
    fn name(&self) -> &'static str {
        "c"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures C handles itself. A root capture sets the declaration
/// root; the `.name` kind capture of the same match sets name and kind.
#[derive(Clone, Copy)]
pub enum CRole {
    Root,
    Type,
    ImportSource,
    Field,
    FieldName,
    Var,
    VarName,
}

#[derive(Clone, Copy)]
pub enum CHook {
    Calls,
    Indirect,
    NamedBindings,
    MacroFallback,
    ReceiverOwners,
}

/// Each declaration captures both the outer node (for span) and the
/// identifier (for name + type-slice end byte).
#[derive(Default)]
pub struct CMatch<'t> {
    type_node: Option<Node<'t>>,
    import_src: Option<Node<'t>>,
    field_root: Option<Node<'t>>,
    field_name: Option<Node<'t>>,
    var_root: Option<Node<'t>>,
    var_name: Option<Node<'t>>,
}

#[derive(Default)]
pub struct CFile {
    /// Written by the calls step, read by the receiver-owners step.
    methods: CReceiverMap,
    preproc: Preproc,
}

impl ParseSpec for CSpec {
    type Role = CRole;
    type Match<'t> = CMatch<'t>;
    type File<'t> = CFile;
    type Hook = CHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// P7 covers C dispatch sites that `indirect_dispatch.rs` doesn't
    /// already flag as CallMeta (function-pointer invocation is already
    /// covered there).
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[(
        "blind.dlsym",
        (
            "c-dlsym",
            "dlsym(<handle>, <name>) — runtime symbol resolution from a dlopen'd library; the returned function pointer's target is not statically determinable",
        ),
    )];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["field_expression"],
        names: NameRefSpec {
            ident_kinds: &["identifier"],
            kind: NameRefKind::Value,
            skip_kinds: &[
                "preproc_include",
                "preproc_def",
                "preproc_function_def",
                "preproc_call",
                "attribute_specifier",
            ],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        // C names enumerators bare: `return RED;`, `case GREEN:`.
        path_kinds: &[],
        label_parents: &["case_statement"],
        bare_label_parents: &[],
        bare_expressions: true,
        token_trees: false,
    });
    /// C has no bind step, so the macro-fallback nodes may follow the calls.
    const STEPS: &'static [Step<CHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Calls, Effect::Nodes, CHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Walk, Effect::Neutral, CHook::Indirect),
        Step::hook(Pass::Imports, Effect::Neutral, CHook::NamedBindings),
        Step::hook(Pass::Walk, Effect::Nodes, CHook::MacroFallback),
        Step::hook(Pass::Build, Effect::Nodes, CHook::ReceiverOwners),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::fixed(Pass::Build, Op::StampOwners),
    ];

    fn query() -> anyhow::Result<Query> {
        Ok(Query::new(
            &tree_sitter_c::LANGUAGE.into(),
            include_str!("queries.scm"),
        )?)
    }

    fn role(capture: &str) -> Option<CRole> {
        Some(match capture {
            "function" | "struct" | "union" | "enum" | "enumerator_node" | "typedef" | "macro" => {
                CRole::Root
            }
            "type" => CRole::Type,
            "import.source" => CRole::ImportSource,
            "field" => CRole::Field,
            "field.name" => CRole::FieldName,
            "var" => CRole::Var,
            "var.name" => CRole::VarName,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: CRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut CMatch<'t>,
        _f: &mut CFile,
        _cx: &mut FileCx<'t>,
    ) {
        match r {
            CRole::Root => d.root = Some(node),
            CRole::Type => m.type_node = Some(node),
            CRole::ImportSource => m.import_src = Some(node),
            CRole::Field => m.field_root = Some(node),
            CRole::FieldName => m.field_name = Some(node),
            CRole::Var => m.var_root = Some(node),
            CRole::VarName => m.var_name = Some(node),
        }
    }

    fn end_match<'t>(d: DeclSlot<'t>, m: &mut CMatch<'t>, f: &mut CFile, cx: &mut FileCx<'t>) {
        let source = cx.source;
        let CMatch {
            type_node,
            import_src,
            field_root,
            field_name,
            var_root,
            var_name,
        } = std::mem::take(m);

        // A typedef cut short by an attribute and a prototype recovered from
        // an ERROR end in the next statement.
        let mut tail: Option<Node<'t>> = None;
        let skip_root = d.root.is_some_and(|r| match r.kind() {
            "declaration" => !is_file_scope(r, f, source),
            "preproc_def" => f.preproc.rows(source).is_guard_row(r.start_position().row),
            "ERROR" => {
                tail = recovered_prototype_end(r);
                tail.is_none() || !is_file_scope(r, f, source)
            }
            _ => false,
        });
        // A typedef captures its whole declarator; the alias is the name
        // inside it (`typedef void *(*F)(int);`). An abstract one names none.
        let name = d.name.and_then(|n| match d.kind {
            Some(NodeKind::Typedef) if d.root.is_some_and(is_cut_typedef) => {
                let (alias, stmt) = macro_typedef_tail(d.root?)?;
                tail = Some(stmt);
                Some(alias)
            }
            Some(NodeKind::Typedef) => typedef_alias(n),
            _ => Some(n),
        });
        if let (Some(n), Some(k), Some(root), false) = (name, d.kind, d.root, skip_root) {
            // K&R-style multi-line function decl can confuse tree-sitter-c:
            //   XXH_PUBLIC_API XXH_errorcode
            //   XXH64_update(int x, int y) { ... }
            // gets parsed as function_declarator with `XXH_errorcode` as
            // the first identifier (captured by @function.name) and the
            // real name `XXH64_update` wrapped in an ERROR sibling.
            // Walk into the next ERROR sibling for the real identifier.
            let recovered_name = if matches!(k, NodeKind::Function | NodeKind::Method) {
                n.next_sibling().and_then(|err| {
                    if err.kind() != "ERROR" {
                        return None;
                    }
                    // Two-step extraction: copy out primitive byte offsets
                    // while cursor is alive, then re-borrow source outside.
                    let mut cur = err.walk();
                    let bytes = err
                        .children(&mut cur)
                        .find(|c| c.kind() == "identifier")
                        .map(|id| (id.start_byte(), id.end_byte()));
                    bytes.and_then(|(s, e)| {
                        std::str::from_utf8(&source[s..e]).ok().map(String::from)
                    })
                })
            } else {
                None
            };
            let name_owned: Option<String> = recovered_name.or_else(|| {
                std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                    .ok()
                    .map(|s| s.to_string())
            });
            if let Some(name_str) = name_owned {
                let type_annotation = if k == NodeKind::Typedef {
                    typedef_aliased_type(root, d.name, tail, source)
                } else {
                    type_node.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    })
                };

                // A function with `static` storage class is translation-unit private.
                let is_exported = !has_static_specifier(root, source);

                let decorators = if matches!(k, NodeKind::Function | NodeKind::Method) {
                    crate::framework_helpers::collect_cpp_attributes(root, source)
                } else {
                    vec![]
                };

                let end = tail.unwrap_or(root);
                cx.nodes.push(RawNode {
                    decorators,
                    is_exported,
                    type_annotation,
                    ..node::decl(
                        name_str,
                        k,
                        node::span_from(root, end),
                        &source[root.start_byte()..end.end_byte()],
                    )
                });
            }
        }

        // Struct / union field → Property node with type slice.
        if let (Some(f_root), Some(f_name)) = (field_root, field_name) {
            if let Ok(name_str) =
                std::str::from_utf8(&source[f_name.start_byte()..f_name.end_byte()])
            {
                cx.nodes.push(RawNode {
                    is_exported: true,
                    type_annotation: declarator_type(f_root, f_name, source),
                    ..node::decl(
                        name_str.to_string(),
                        NodeKind::Property,
                        node::span(f_root),
                        &source[f_root.start_byte()..f_root.end_byte()],
                    )
                });
            }
        }

        // Top-level variable / const declaration → Variable or Const node,
        // by the rule C and C++ share (`is_const_qualified`).
        // Type slice includes storage-class + qualifiers (see
        // `slice_type_before` doc comment).
        if let (Some(v_root), Some(v_name)) = (var_root, var_name.and_then(declared_variable)) {
            if let Ok(name_str) =
                std::str::from_utf8(&source[v_name.start_byte()..v_name.end_byte()])
            {
                // tree-sitter-c can mis-parse multi-line preprocessor
                // macros (verified on ziplist.c lines 408-443 where the
                // `ZIP_DECODE_LENGTH` `do{...}while(0)` macro causes the
                // following function decl to be partially re-parsed as
                // `(translation_unit (declaration ...))`, capturing
                // function parameters AND type keywords as @var.name).
                // Two-layer guard:
                //   1. v_root.has_error() — the declaration node itself
                //      (or any descendant) is an ERROR/MISSING node, so
                //      the @var capture is recovery noise. Real var
                //      decls in well-formed code have has_error=false.
                //   2. C reserved keyword check — defensive even when
                //      has_error is somehow false; `unsigned`/`const`/
                //      etc. are never legal C variable names.
                // The parent walk of `is_file_scope` rejects a local before
                // the sibling walk of `is_typedef_tail` runs.
                if (!v_root.has_error() || semicolon_after_conditional(v_root))
                    && !is_c_reserved_keyword(name_str)
                    && is_file_scope(v_root, f, source)
                    && !is_typedef_tail(v_root)
                {
                    let typedef_keyword = split_typedef_keyword(v_root, source);
                    let start = typedef_keyword.unwrap_or(v_root);
                    let (kind, type_annotation) = if typedef_keyword.is_some() {
                        (NodeKind::Typedef, None)
                    } else if is_const_qualified(v_root, v_name, source) {
                        (NodeKind::Const, declarator_type(v_root, v_name, source))
                    } else {
                        (NodeKind::Variable, declarator_type(v_root, v_name, source))
                    };
                    cx.nodes.push(RawNode {
                        is_exported: !has_static_specifier(v_root, source),
                        type_annotation,
                        ..node::decl(
                            name_str.to_string(),
                            kind,
                            node::span_from(start, v_root),
                            &source[start.start_byte()..v_root.end_byte()],
                        )
                    });
                }
            }
        }

        if let Some(i_src) = import_src {
            if let Ok(src_str) = std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
            {
                cx.imports.push(driver::imports::raw(
                    src_str.to_string(),
                    "*".to_string(),
                    None,
                    None,
                ));
            }
        }
    }

    fn hook<'t>(h: CHook, _c: &Compiled<Self>, f: &mut CFile, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            CHook::Calls => {
                // Extract call sites with C-convention receiver binding: functions
                // taking a `(struct T *self, ...)`-shaped first param are treated
                // as methods on `T`, so call sites rewrite to `T.fn` for the
                // resolver's Tier 2.5 qualifier-scoped lookup. Convention-driven,
                // not language-mandated — see `RECEIVER_NAMES` for the gate.
                f.methods = collect_receiver_methods(root, source);
                let (raw_path_literals, raw_sql_refs) = extract_c_calls_and_path_literals(
                    root,
                    source,
                    &mut cx.nodes,
                    &f.methods,
                    &mut cx.call_sites,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            CHook::Indirect => {
                let mut fn_ptr_vars = FnPtrVars::default();
                let depths = root.has_error().then(|| f.preproc.depths(source));
                collect_c_cpp_fn_ptr_vars(root, source, depths, &mut fn_ptr_vars);
                if root.has_error() {
                    declare_macro_bound_vars(root, source, &mut fn_ptr_vars);
                }
                cx.call_metas = detect_c_cpp_indirect(
                    root,
                    source,
                    &cx.nodes,
                    &fn_ptr_vars,
                    false,
                    &cx.call_sites,
                );
            }
            // Named bindings: `#define`, `extern` declarations. A `typedef`
            // is a Typedef node, no binding.
            // Emitted as `RawImport` with `alias = Some(short_name)` mirroring
            // Java's static-import convention so downstream resolvers can
            // qualifier-scope-lookup C aliases the same way.
            CHook::NamedBindings => {
                extract_named_bindings(root, source, f.preproc.rows(source), &mut cx.imports)
            }
            // `#define NAME` regex fallback — tree-sitter-c (0.24.x) ERROR-
            // recovers around multi-line `\` continuations + `##` token-paste
            // and drops the `preproc_def` wrapper, leaving the (preproc_def …)
            // query empty for those regions. The fallback scans raw source as
            // a safety net and emits Macro nodes for any `#define NAME` not
            // already captured. Verified `.sample_repo`: ecp went from 11/29
            // macros in `tsd.h` to full recall after this pass.
            CHook::MacroFallback => emit_macro_fallback(source, &mut f.preproc, &mut cx.nodes),
            CHook::ReceiverOwners => {
                // Stamp owner_class from the receiver-convention map (self/this
                // first-param pattern). C has no class syntax; the receiver
                // convention is the closest proxy for struct membership.
                for node in cx.nodes.iter_mut() {
                    if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                        if let Some(recv_ty) = f.methods.get(&node.name) {
                            node.owner_class = Some(recv_ty.to_string());
                        }
                    }
                }
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::c::extract(root, source, nodes, category)
    }
}

/// Augment `nodes` with `#define NAME` Macros that tree-sitter ERROR-
/// recovery dropped (the C and C++ parsers share it). See `preproc_fallback`
/// module docs for the failure shape this covers.
pub(crate) fn emit_macro_fallback(source: &[u8], preproc: &mut Preproc, nodes: &mut Vec<RawNode>) {
    let existing: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Macro)
        .map(|n| n.name.clone())
        .collect();
    let guards = preproc.rows(source).guards().to_vec();
    // `end_match` drops each include guard's `#define`; the fallback must
    // not add it back.
    for hit in preproc.macros(source) {
        if existing.contains(&hit.name) || guards.binary_search(&hit.line).is_ok() {
            continue;
        }
        nodes.push(RawNode {
            decorators: vec![],
            is_exported: true,
            heritage: vec![],
            type_annotation: None,
            name: hit.name.clone(),
            kind: NodeKind::Macro,
            span: (hit.line, hit.col_start, hit.line, hit.col_end),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        });
    }
}
