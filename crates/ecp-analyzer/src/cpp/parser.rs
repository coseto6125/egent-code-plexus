use super::receiver_types::{
    collect_bindings, extract_cpp_calls_and_path_literals, scope_type_name, CppBindings,
};
use super::recovery::{prepare_source, reduce_type_name, stamp_lexical_owners};
use super::spec::CppSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::imports::{self, ImportOrder};
use crate::driver::{
    self, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture,
    Op, ParseSpec, PrepareSource, Step,
};
use crate::framework_confidence;
use crate::framework_helpers::{detect_ast_framework_patterns, FrameworkPatternSpec, Span};
use crate::indirect_dispatch::{collect_c_cpp_fn_ptr_vars, detect_c_cpp_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFunctionMeta, RawNode, OVERRIDE_SENTINEL};
use ecp_core::graph::{FileCategory, NodeKind};
use rustc_hash::FxHashSet;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

/// True if `node` (a `function_definition`) is defined inside a
/// `field_declaration_list`, which means it is an inline class/struct member
/// function. Tree-sitter aliases `constructor_or_destructor_definition` to
/// `function_definition`, so this check also covers constructors and
/// destructors defined inline.
fn is_inline_class_member(node: tree_sitter::Node<'_>) -> bool {
    let mut cursor = node.parent();
    while let Some(p) = cursor {
        match p.kind() {
            "field_declaration_list" => return true,
            // Stop at translation-unit scope or namespace/linkage boundaries.
            "translation_unit" | "namespace_definition" | "linkage_specification" => return false,
            _ => cursor = p.parent(),
        }
    }
    false
}

/// The bases of a class / struct definition `root`, each as
/// [`reduce_type_name`] names it: `Base`, `Proto<CharType>` → `Proto`,
/// `detail::foo`. A dependent or `decltype` base names no project type.
fn cpp_heritage(root: tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut c = root.walk();
    let Some(clause) = root
        .children(&mut c)
        .find(|ch| ch.kind() == "base_class_clause")
    else {
        return Vec::new();
    };
    let mut bc = clause.walk();
    let bases = clause
        .named_children(&mut bc)
        .filter(|b| {
            matches!(
                b.kind(),
                "type_identifier" | "template_type" | "qualified_identifier"
            )
        })
        .filter_map(|b| b.utf8_text(source).ok().and_then(reduce_type_name))
        .collect();
    bases
}

/// The whole qualifier before the innermost `name` of a qualified
/// declarator: `a::b` for `a::b::h`.
fn qualified_scope_text(name: tree_sitter::Node<'_>, source: &[u8]) -> String {
    let mut outer = name;
    while let Some(p) = outer
        .parent()
        .filter(|p| p.kind() == "qualified_identifier")
    {
        outer = p;
    }
    std::str::from_utf8(&source[outer.start_byte()..name.start_byte()])
        .unwrap_or_default()
        .trim_end()
        .trim_end_matches("::")
        .trim()
        .to_string()
}

/// The innermost name of a qualified method name (`Foo::bar` → `bar`); any
/// other node as it is.
fn innermost_name(mut n: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    while n.kind() == "qualified_identifier" {
        match n.child_by_field_name("name") {
            Some(inner) => n = inner,
            None => break,
        }
    }
    n
}

/// True if `name` is a C/C++ reserved keyword that tree-sitter sometimes
/// mis-captures as an identifier during error-recovery from preprocessor
/// macros. Legal C++ code never names a variable with these.
fn is_cpp_reserved_keyword(name: &str) -> bool {
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
            | "bool"
            | "wchar_t"
            | "char8_t"
            | "char16_t"
            | "char32_t"
            | "const"
            | "volatile"
            | "constexpr"
            | "consteval"
            | "constinit"
            | "mutable"
            | "static"
            | "extern"
            | "auto"
            | "thread_local"
            | "register"
            | "inline"
            | "struct"
            | "union"
            | "enum"
            | "class"
            | "typedef"
            | "namespace"
            | "using"
            | "template"
            | "typename"
            | "concept"
            | "requires"
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
            | "new"
            | "delete"
            | "throw"
            | "try"
            | "catch"
            | "noexcept"
            | "public"
            | "private"
            | "protected"
            | "virtual"
            | "override"
            | "final"
            | "this"
            | "nullptr"
            | "true"
            | "false"
            | "operator"
            | "and"
            | "or"
            | "not"
            | "xor"
            | "bitand"
            | "bitor"
            | "compl"
    )
}

/// True if a file-scope `declaration` node carries a `const` or `constexpr`
/// qualifier. In tree-sitter-cpp a qualifying keyword appears as a
/// `type_qualifier` named child of the `declaration` with text `"const"` or
/// `"constexpr"`. Mutable globals have no such child.
///
/// This is intentionally scoped to the `@var` use-site (translation-unit
/// level `declaration` nodes). Class fields use `field_declaration` and
/// function parameters use `parameter_declaration` — neither reaches this
/// helper.
fn is_const_qualified(decl: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut cursor = decl.walk();
    for child in decl.children(&mut cursor) {
        if child.kind() == "type_qualifier" {
            if let Ok(text) = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]) {
                let text = text.trim();
                if text == "const" || text == "constexpr" {
                    return true;
                }
            }
        }
    }
    false
}

/// Per upstream `c-cpp.ts:414-431` `cppProvider.astFrameworkPatterns`.
/// Note: upstream's `cProvider` has no `astFrameworkPatterns`, so this is
/// C++-only.
const CPP_FRAMEWORKS: &[FrameworkPatternSpec] = &[FrameworkPatternSpec {
    framework: "qt",
    reason: "qt-macro",
    confidence: framework_confidence::QT_HINT,
    patterns: &[
        "Q_OBJECT",
        "Q_INVOKABLE",
        "Q_PROPERTY",
        "Q_SIGNALS",
        "Q_SLOTS",
        "Q_SIGNAL",
        "Q_SLOT",
        "QWidget",
        "QApplication",
    ],
}];

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_cpp::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}

/// Slice the source between a declaration's start byte and its identifier
/// name's start byte to recover the type-annotation text.
///
/// Convention (documented per task D3 for both C and C++):
/// - **Pointer / reference spacing** is preserved as-written. `char* s`
///   yields `"char*"`; `const std::string& s` yields `"const std::string&"`.
///   Source is the source of truth.
/// - **Qualifier inclusion** is YES — full prefix including storage class
///   (`static`, `extern`) and cv-qualifiers (`const`, `volatile`).
/// - **`auto`** is preserved literally; the analyzer doesn't do type
///   deduction. `auto x = 5;` → `Some("auto")`.
fn slice_type_before(
    decl: tree_sitter::Node<'_>,
    name: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<String> {
    let start = decl.start_byte();
    let end = name.start_byte();
    if end <= start {
        return None;
    }
    std::str::from_utf8(source.get(start..end)?)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The type of `name` in a declaration that may hold several declarators
/// (`int *p, q;`). The first declarator keeps [`slice_type_before`]; a later
/// one takes the shared specifier before the first declarator plus its own
/// pointer / reference prefix, so `q` is `int`, never `int *p,`.
fn declarator_type(
    decl: tree_sitter::Node<'_>,
    name: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<String> {
    let mut top = name;
    while let Some(p) = top.parent() {
        if p.id() == decl.id() {
            break;
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

pub struct CppProvider {
    compiled: Compiled<CppSpec>,
}

impl CppProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for CppProvider {
    fn name(&self) -> &'static str {
        "cpp"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures C++ handles itself. A root capture sets the declaration
/// root; the `name.*` kind capture of the same match sets name and kind.
#[derive(Clone, Copy)]
pub enum CppRole {
    Root,
    Type,
    OverrideMarker,
    Alias,
    ImportSource,
    Import,
    Field,
    FieldName,
    Var,
    VarName,
}

#[derive(Clone, Copy)]
pub enum CppHook {
    OverridePreScan,
    Owners,
    Calls,
    Indirect,
    MacroFallback,
    FriendOwners,
}

#[derive(Default)]
pub struct CppMatch<'t> {
    type_node: Option<Node<'t>>,
    import_src_node: Option<Node<'t>>,
    import_alias_node: Option<Node<'t>>,
    is_import: bool,
    // Buffers for param / field / var declarations (D3 type annotations).
    field_root: Option<Node<'t>>,
    field_name: Option<Node<'t>>,
    var_root: Option<Node<'t>>,
    var_name: Option<Node<'t>>,
}

#[derive(Default)]
pub struct CppFile {
    /// Set by the first step, before any reader runs.
    is_header: bool,
    /// `function_definition` ids that carry an `override` marker. The
    /// marker fires in another match than the declaration's name, so the
    /// pre-scan collects them before the main loop reads them.
    override_func_ids: FxHashSet<usize>,
    /// Out-of-class definitions whose qualifier may name a namespace:
    /// `(node index, innermost qualifier, whole qualifier)`.
    qualified_owners: Vec<(usize, String, String)>,
    /// Functions declared or defined as `friend` in a class body: members
    /// of the enclosing namespace, not of the class.
    friend_spans: FxHashSet<Span>,
    recovered_classes: Vec<(Span, String)>,
    bindings: CppBindings,
}

fn is_header(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext == "h" || ext == "hpp" || ext == "hxx" || ext == "hh")
        .unwrap_or(false)
}

impl ParseSpec for CppSpec {
    type Role = CppRole;
    type Match<'t> = CppMatch<'t>;
    type File<'t> = CppFile;
    type Hook = CppHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// Anonymous lambda passed as a call argument. Emit an `<anonymous>`
    /// Function node only when the body contains a call, so
    /// attach_to_enclosing can host those calls instead of dropping them;
    /// empty lambdas stay out of the graph. A lambda that matches the query
    /// twice (nested argument_list patterns) yields one node per byte range.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call_expression"],
        key: AnonKey::Span,
    });
    /// P7 covers C++ dispatch sites that `indirect_dispatch.rs` doesn't
    /// already flag as CallMeta (virtual / function-pointer dispatch is
    /// already covered there).
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[(
        "blind.dlsym",
        (
            "cpp-dlsym",
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
                "attribute_declaration",
                "qualified_identifier",
                "template_argument_list",
                "using_declaration",
                "alias_declaration",
                "namespace_alias_definition",
            ],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        // `Color::Red` for scoped enums; unscoped enumerators are bare.
        path_kinds: &["qualified_identifier"],
        label_parents: &["case_statement"],
        bare_label_parents: &[],
        bare_expressions: true,
        token_trees: false,
    });
    /// Node offsets index `source` unchanged: the patches only overwrite
    /// macro names and inactive initializer-list branches with blanks, so
    /// every later step reads `source`.
    const PREPARE_SOURCE: Option<PrepareSource> = Some(prepare_source);
    const STEPS: &'static [Step<CppHook>] = &[
        Step::hook(Pass::Walk, Effect::Neutral, CppHook::OverridePreScan),
        Step::fixed(Pass::Query, Op::Query),
        Step::fixed(
            Pass::Build,
            Op::SortDedupImports(ImportOrder::NameSourceAlias),
        ),
        Step::hook(Pass::Build, Effect::Nodes, CppHook::Owners),
        Step::hook(Pass::Calls, Effect::Nodes, CppHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Walk, Effect::Neutral, CppHook::Indirect),
        // `#define NAME` regex fallback — tree-sitter-cpp (0.23.x) ERROR-
        // recovers around deeply nested templates / `JEMALLOC_ALWAYS_INLINE`-
        // style attribute macros stacked on function declarations and drops
        // the `preproc_def` wrapper. Verified on `.sample_repo`: ecp
        // emitted 137/673 macros in `doctest.h` and 11/29 in `tsd.h`; the
        // fallback restores full recall. C++ has no bind step, so a node
        // pushed after the calls binds nothing out of place.
        Step::hook(Pass::Walk, Effect::Nodes, CppHook::MacroFallback),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::fixed(Pass::Build, Op::StampOwners),
        Step::hook(Pass::Build, Effect::Nodes, CppHook::FriendOwners),
    ];

    fn query() -> anyhow::Result<Query> {
        Ok(Query::new(
            &tree_sitter_cpp::LANGUAGE.into(),
            include_str!("queries.scm"),
        )?)
    }

    fn role(capture: &str) -> Option<CppRole> {
        Some(match capture {
            "function" | "class" | "struct" | "method" | "macro" | "namespace" | "enum_node"
            | "enumerator_node" | "typedef_node" => CppRole::Root,
            "type" => CppRole::Type,
            "override_marker" => CppRole::OverrideMarker,
            "alias" => CppRole::Alias,
            "import.source" => CppRole::ImportSource,
            "import" => CppRole::Import,
            "field" => CppRole::Field,
            "field.name" => CppRole::FieldName,
            "var" => CppRole::Var,
            "var.name" => CppRole::VarName,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: CppRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut CppMatch<'t>,
        _f: &mut CppFile,
        _cx: &mut FileCx<'t>,
    ) {
        match r {
            CppRole::Root => d.root = Some(node),
            CppRole::Type => m.type_node = Some(node),
            CppRole::OverrideMarker => {}
            CppRole::Alias => m.import_alias_node = Some(node),
            CppRole::ImportSource => m.import_src_node = Some(node),
            CppRole::Import => m.is_import = true,
            CppRole::Field => m.field_root = Some(node),
            CppRole::FieldName => m.field_name = Some(node),
            CppRole::Var => m.var_root = Some(node),
            CppRole::VarName => m.var_name = Some(node),
        }
    }

    fn end_match<'t>(d: DeclSlot<'t>, m: &mut CppMatch<'t>, f: &mut CppFile, cx: &mut FileCx<'t>) {
        let source = cx.source;
        let CppMatch {
            type_node,
            import_src_node,
            import_alias_node,
            is_import,
            field_root,
            field_name,
            var_root,
            var_name,
        } = std::mem::take(m);

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            let n = innermost_name(n);
            if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                let is_friend = matches!(k, NodeKind::Function | NodeKind::Method)
                    && root
                        .parent()
                        .is_some_and(|p| p.kind() == "friend_declaration");
                // Promote free-function to Method when the definition is
                // lexically inside a class/struct body.
                let k = if k == NodeKind::Function && !is_friend && is_inline_class_member(root) {
                    NodeKind::Method
                } else {
                    k
                };
                // An out-of-class member definition belongs to its
                // qualifier (`void ns::Foo::bar() {}` → `Foo`), not to
                // the namespace around the definition. The grammar
                // cannot tell `Foo::bar` from `ns::bar`: a template
                // qualifier or a constructor / destructor name proves a
                // type, any other qualifier waits for a namespace of that
                // name in this file (`qualified_owners` in the owners hook).
                let qualifier = match k {
                    NodeKind::Method => n
                        .parent()
                        .filter(|p| p.kind() == "qualified_identifier")
                        .and_then(|p| p.child_by_field_name("scope"))
                        .and_then(|scope| {
                            let name = scope_type_name(scope, source)?;
                            let is_type = scope.kind() == "template_type"
                                || name_str.trim_start_matches('~') == name;
                            Some((name.to_string(), is_type))
                        }),
                    _ => None,
                };
                let owner_class = match qualifier {
                    Some((owner, true)) => Some(owner),
                    Some((owner, false)) => {
                        let scope = qualified_scope_text(n, source);
                        f.qualified_owners.push((cx.nodes.len(), owner, scope));
                        None
                    }
                    None => None,
                };

                let type_annotation = type_node.and_then(|t| {
                    std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                        .ok()
                        .map(|s| s.trim().to_string())
                });

                let heritage = match k {
                    NodeKind::Class | NodeKind::Struct => cpp_heritage(root, source),
                    _ => Vec::new(),
                };

                let mut decorators = if f.override_func_ids.contains(&root.id()) {
                    vec![OVERRIDE_SENTINEL.to_string()]
                } else {
                    vec![]
                };
                if matches!(k, NodeKind::Function | NodeKind::Method) {
                    decorators.extend(crate::framework_helpers::collect_cpp_attributes(
                        root, source,
                    ));
                }
                let span = node::span(root);
                if is_friend {
                    f.friend_spans.insert(span);
                }
                cx.nodes.push(RawNode {
                    decorators,
                    is_exported: f.is_header,
                    heritage,
                    type_annotation,
                    owner_class,
                    ..node::decl(
                        name_str.to_string(),
                        k,
                        span,
                        &source[root.start_byte()..root.end_byte()],
                    )
                });
            }
        }

        // Class / struct data-member → Property node with type slice.
        if let (Some(f_root), Some(f_name)) = (field_root, field_name) {
            if let Ok(name_str) =
                std::str::from_utf8(&source[f_name.start_byte()..f_name.end_byte()])
            {
                cx.nodes.push(RawNode {
                    is_exported: f.is_header,
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

        // Top-level variable / `auto` declaration → Variable node.
        // Guard with `has_error` + C++ reserved-keyword check: tree-sitter
        // C/C++ can re-parse function bodies as `(declaration ...)` after
        // recovering from complex preprocessor macros, capturing function
        // parameters and type keywords as @var.name. Real var decls in
        // well-formed code carry has_error=false and never use keywords
        // as identifier names.
        //
        // const/constexpr globals → NodeKind::Const: a file-scope
        // `declaration` whose first named child is a `type_qualifier`
        // with text "const" or "constexpr" is a compile-time constant.
        // The `@var` query already anchors to `(translation_unit
        // (declaration ...))`, so `v_root` is always translation-unit
        // level; const-qualified class fields use `field_declaration`
        // (→ @field) and const params use `parameter_declaration`
        // (→ @param) — neither reaches this branch.
        if let (Some(v_root), Some(v_name)) = (var_root, var_name) {
            if let Ok(name_str) =
                std::str::from_utf8(&source[v_name.start_byte()..v_name.end_byte()])
            {
                if !v_root.has_error() && !is_cpp_reserved_keyword(name_str) {
                    let kind = if is_const_qualified(v_root, source) {
                        NodeKind::Const
                    } else {
                        NodeKind::Variable
                    };
                    cx.nodes.push(RawNode {
                        is_exported: f.is_header,
                        type_annotation: declarator_type(v_root, v_name, source),
                        ..node::decl(
                            name_str.to_string(),
                            kind,
                            node::span(v_root),
                            &source[v_root.start_byte()..v_root.end_byte()],
                        )
                    });
                }
            }
        }

        if is_import {
            if let Some(src_node) = import_src_node {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()])
                {
                    let mut src_s = src_str.to_string();
                    if (src_s.starts_with('"') && src_s.ends_with('"'))
                        || (src_s.starts_with('<') && src_s.ends_with('>'))
                    {
                        src_s = src_s[1..src_s.len() - 1].to_string();
                    }

                    let alias = import_alias_node.and_then(|a| {
                        std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });

                    let imported_name = src_s.clone();

                    cx.imports
                        .push(imports::raw(src_s, imported_name, alias, None));
                }
            }
        }
    }

    fn hook<'t>(h: CppHook, c: &Compiled<Self>, f: &mut CppFile, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            CppHook::OverridePreScan => {
                // Pre-pass: collect function_definition node IDs that carry an
                // `override` virtual_specifier. These are collected in a separate
                // QueryCursor pass so we can cross-reference against the main parse
                // loop (where the override pattern fires on a different match than
                // the @name.method capture, making per-match `has_override` unreliable).
                f.is_header = is_header(cx.path);
                let mut pre_cursor = QueryCursor::new();
                let mut pre_matches = pre_cursor.matches(c.query(), root, source);
                while let Some(m) = pre_matches.next() {
                    for cap in m.captures() {
                        if matches!(c.role_at(cap.index), Some(CppRole::OverrideMarker)) {
                            let func = std::iter::successors(cap.node.parent(), |n| n.parent())
                                .find(|n| n.kind() == "function_definition");
                            if let Some(func) = func {
                                f.override_func_ids.insert(func.id());
                            }
                        }
                    }
                }
            }
            CppHook::Owners => {
                let nodes = &mut cx.nodes;
                // tree-sitter lost a class or cut it short (a macro before `class`,
                // an `#if` inside a declaration): owners come from matched braces.
                f.recovered_classes = if root.has_error() {
                    stamp_lexical_owners(source, nodes, f.is_header)
                } else {
                    Vec::new()
                };

                // C++ has no reserved constructor name; the convention is that a
                // method whose name equals its enclosing class name is a constructor.
                // Inline ctors are already Method (via `is_inline_class_member`);
                // out-of-line `Foo::Foo()` captures the unqualified `Foo` as @name.method.
                // Post-process: collect Class/Struct names, then promote any Method
                // whose name appears in that set to Constructor.
                // Why: parser has no per-node enclosing-class context at emit time;
                // file-scope name matching is the cheapest approximation that works
                // for the common single-class-per-file and multi-class-per-file cases.
                // False-positive risk: a method that happens to share a name with a
                // class in the same file (e.g. `class Foo {}; class Bar { void Foo() {} }`)
                // — this is an edge case and the naming convention itself is the signal.
                let class_names: std::collections::HashSet<String> = nodes
                    .iter()
                    .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Struct))
                    .map(|n| n.name.clone())
                    .collect();
                for node in nodes.iter_mut() {
                    if node.kind == NodeKind::Method && class_names.contains(&node.name) {
                        node.kind = NodeKind::Constructor;
                    }
                }
                // A qualifier this file declares only as a namespace makes the
                // definition a free function of that namespace (`void ns::f() {}`),
                // owned like a `void f() {}` written inside `namespace ns { }`.
                let namespace_names: std::collections::HashSet<String> = nodes
                    .iter()
                    .filter(|n| n.kind == NodeKind::Namespace)
                    .map(|n| n.name.clone())
                    .collect();
                for (idx, owner, scope) in std::mem::take(&mut f.qualified_owners) {
                    let node = &mut nodes[idx];
                    if class_names.contains(&owner) {
                        node.owner_class = Some(owner);
                        continue;
                    }
                    if node.kind != NodeKind::Method || node.owner_class.is_some() {
                        continue;
                    }
                    // `namespace a::b { }` names its node `a::b`, nested blocks name
                    // the inner one `b`: try the whole qualifier, then its last part.
                    let namespace = [&scope, &owner]
                        .into_iter()
                        .find(|q| namespace_names.contains(*q))
                        .cloned();
                    match namespace {
                        Some(namespace) => {
                            node.kind = NodeKind::Function;
                            node.owner_class = Some(namespace);
                        }
                        // A qualifier this file declares as neither is most often a
                        // class of a header (`void A::reset() {}` in `a.cpp`). It
                        // owns the definition ahead of any namespace around it, so
                        // `A::reset` and `B::reset` stay two declarations, never
                        // overloads of one.
                        None => node.owner_class = Some(owner),
                    }
                }
            }
            CppHook::Calls => {
                // Extract call sites with receiver-type binding: `this->method()` /
                // `this.method()` → `Class.method`, `Base::method()` → `Base.method`,
                // and typed-var `obj.method()` / `obj->method()` → `Type.method`.
                // Feeds the resolver's Tier 2.5 qualifier-scoped lookup.
                let mut bindings = collect_bindings(root, source);
                for (span, name) in std::mem::take(&mut f.recovered_classes) {
                    bindings.push_class_scope(span, name);
                }
                let (raw_path_literals, raw_sql_refs) = extract_cpp_calls_and_path_literals(
                    root,
                    source,
                    &mut cx.nodes,
                    &bindings,
                    &mut cx.call_sites,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
                f.bindings = bindings;
            }
            CppHook::Indirect => {
                // Merge bindings-derived types with declaration-level fn-pointer vars.
                let mut fn_ptr_vars = f.bindings.flat_bindings();
                let decl_vars = collect_c_cpp_fn_ptr_vars(root, source);
                fn_ptr_vars.extend(decl_vars);
                cx.call_metas = detect_c_cpp_indirect(
                    root,
                    source,
                    &cx.nodes,
                    &fn_ptr_vars,
                    true,
                    &cx.call_sites,
                );

                cx.framework_refs = detect_ast_framework_patterns(source, CPP_FRAMEWORKS);
            }
            CppHook::MacroFallback => emit_macro_fallback(source, &mut cx.nodes),
            CppHook::FriendOwners => {
                if !f.friend_spans.is_empty() {
                    restamp_friend_owners(&mut cx.nodes, &f.friend_spans);
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
        crate::function_meta::cpp::extract(root, source, nodes, category)
    }
}

/// A `friend` function belongs to the namespace around its class, so its
/// owner is the tightest enclosing `Namespace`, or none at file scope —
/// never the class the span stamping found. A node inside the friend's body
/// (a lambda) follows the friend, unless a type declared in that body holds it.
fn restamp_friend_owners(
    nodes: &mut [RawNode],
    friend_spans: &rustc_hash::FxHashSet<(u32, u32, u32, u32)>,
) {
    let contains = |outer: Span, inner: Span| {
        (outer.0, outer.1) <= (inner.0, inner.1) && (inner.2, inner.3) <= (outer.2, outer.3)
    };
    let namespaces: Vec<(String, (u32, u32, u32, u32))> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Namespace)
        .map(|n| (n.name.clone(), n.span))
        .collect();
    let types: Vec<Span> = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Struct | NodeKind::Enum))
        .map(|n| n.span)
        .collect();
    for node in nodes.iter_mut() {
        let Some(&friend) = friend_spans.iter().find(|&&f| contains(f, node.span)) else {
            continue;
        };
        let held_by_local_type = types
            .iter()
            .any(|&t| t != node.span && contains(friend, t) && contains(t, node.span));
        if held_by_local_type {
            continue;
        }
        node.owner_class = namespaces
            .iter()
            .filter(|(_, s)| contains(*s, node.span))
            .min_by_key(|(_, s)| crate::framework_helpers::span_area(*s))
            .map(|(name, _)| name.clone());
    }
}

/// Augment `nodes` with `#define NAME` Macros that tree-sitter ERROR-
/// recovery dropped. Mirror of the C parser's pass — same `preproc_fallback`
/// scanner; same NodeKind::Macro shape; same dedup-against-existing rule.
fn emit_macro_fallback(source: &[u8], nodes: &mut Vec<RawNode>) {
    let existing: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Macro)
        .map(|n| n.name.clone())
        .collect();
    for hit in crate::preproc_fallback::scan_define_macros(source) {
        if existing.contains(&hit.name) {
            continue;
        }
        nodes.push(RawNode {
            decorators: vec![],
            is_exported: true,
            heritage: vec![],
            type_annotation: None,
            name: hit.name,
            kind: NodeKind::Macro,
            span: (hit.line, hit.col_start, hit.line, hit.col_end),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        });
    }
}
