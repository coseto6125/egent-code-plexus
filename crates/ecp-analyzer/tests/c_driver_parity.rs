//! `CProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order. The expected values come from
//! a `{:#?}` dump of the old parser over these exact sources.

mod driver_parity_support;

use driver_parity_support::assert_empty_graph;
use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, LocalGraph};
use ecp_core::graph::NodeKind;
use std::path::Path;

type Span = (u32, u32, u32, u32);

fn parse_at(path: &str, src: &[u8]) -> LocalGraph {
    CProvider::new()
        .expect("CProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn parse(src: &[u8]) -> LocalGraph {
    parse_at("a.c", src)
}

/// `(name, kind, span, is_exported, type_annotation, owner_class)`
type NodeRow<'g> = (
    &'g str,
    NodeKind,
    Span,
    bool,
    Option<&'g str>,
    Option<&'g str>,
);

fn node_rows(g: &LocalGraph) -> Vec<NodeRow<'_>> {
    g.nodes
        .iter()
        .map(|n| {
            (
                n.name.as_str(),
                n.kind,
                n.span,
                n.is_exported,
                n.type_annotation.as_deref(),
                n.owner_class.as_deref(),
            )
        })
        .collect()
}

/// `(source, imported_name, alias, binding_kind)`
type ImportRow<'g> = (&'g str, &'g str, Option<&'g str>, Option<BindingKind>);

fn import_rows(g: &LocalGraph) -> Vec<ImportRow<'_>> {
    g.imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
                i.binding_kind,
            )
        })
        .collect()
}

fn calls_of<'g>(g: &'g LocalGraph, name: &str, span: Span) -> (Vec<&'g str>, Vec<&'g str>) {
    let n = g
        .nodes
        .iter()
        .find(|n| n.name == name && n.span == span)
        .unwrap_or_else(|| panic!("no node {name} at {span:?}"));
    (
        n.calls.iter().map(String::as_str).collect(),
        n.field_reads.iter().map(String::as_str).collect(),
    )
}

/// `(span, flags, params, return_type)`
type FnMetaRow<'g> = (Span, u16, Vec<&'g str>, &'g str);

/// `(caller_name, caller_span, call_index, flags, dispatch_type)`
type CallMetaRow<'g> = (&'g str, Span, u32, u8, &'g str);

fn binding(name: &'static str, source: &'static str, kind: BindingKind) -> ImportRow<'static> {
    (source, name, Some(name), Some(kind))
}

#[test]
fn test_c_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_empty_graph(&g, "a.c");
}

/// `#include` keeps source order and duplicates: C sorts no imports.
#[test]
fn test_c_driver_includes_only_file_keeps_order_and_duplicates() {
    let g = parse(b"#include <stdio.h>\n#include \"foo.h\"\n#include <stdio.h>\n");
    assert!(g.nodes.is_empty() && g.raw_function_metas.is_empty());
    assert_eq!(
        import_rows(&g),
        [
            ("<stdio.h>", "*", None, None),
            ("\"foo.h\"", "*", None, None),
            ("<stdio.h>", "*", None, None),
        ]
    );
}

const DECLS: &[u8] = br#"struct point { int x; char *name; };
union val { int i; float f; };
enum color { RED, GREEN = 2, BLUE };
typedef unsigned long counter_t;
typedef struct point point_t;
typedef int (*cmp_fn)(int, int);
static const int LIMIT = 5;
int *gptr;
int add(int a, int b);
static int sub(int a, int b) { return a - b; }
int add(int a, int b) { return sub(a, b) + LIMIT; }
enum color pick(void) { return GREEN; }
"#;

/// Query order: each declaration, then its fields or enumerators; the
/// owner stamp runs last. A prototype has no `@type` and a `static` function
/// is not exported.
#[test]
fn test_c_driver_declarations_keep_query_order_and_fields() {
    let g = parse(DECLS);
    let s = Some;
    assert_eq!(
        node_rows(&g),
        [
            ("point", NodeKind::Struct, (0, 0, 0, 35), true, None, None),
            (
                "x",
                NodeKind::Property,
                (0, 15, 0, 21),
                true,
                s("int"),
                s("point")
            ),
            (
                "name",
                NodeKind::Property,
                (0, 22, 0, 33),
                true,
                s("char *"),
                s("point")
            ),
            ("val", NodeKind::Struct, (1, 0, 1, 29), true, None, None),
            (
                "i",
                NodeKind::Property,
                (1, 12, 1, 18),
                true,
                s("int"),
                s("val")
            ),
            (
                "f",
                NodeKind::Property,
                (1, 19, 1, 27),
                true,
                s("float"),
                s("val")
            ),
            ("color", NodeKind::Enum, (2, 0, 2, 35), true, None, None),
            (
                "RED",
                NodeKind::EnumVariant,
                (2, 13, 2, 16),
                true,
                None,
                s("color")
            ),
            (
                "GREEN",
                NodeKind::EnumVariant,
                (2, 18, 2, 27),
                true,
                None,
                s("color")
            ),
            (
                "BLUE",
                NodeKind::EnumVariant,
                (2, 29, 2, 33),
                true,
                None,
                s("color")
            ),
            (
                "counter_t",
                NodeKind::Typedef,
                (3, 0, 3, 32),
                true,
                None,
                None
            ),
            (
                "point_t",
                NodeKind::Typedef,
                (4, 0, 4, 29),
                true,
                None,
                None
            ),
            // A function-pointer typedef is a Typedef node like the plain ones.
            ("cmp_fn", NodeKind::Typedef, (5, 0, 5, 32), true, None, None),
            (
                "LIMIT",
                NodeKind::Variable,
                (6, 0, 6, 27),
                true,
                s("static const int"),
                None
            ),
            (
                "gptr",
                NodeKind::Variable,
                (7, 0, 7, 10),
                true,
                s("int *"),
                None
            ),
            ("add", NodeKind::Function, (8, 0, 8, 22), true, None, None),
            (
                "sub",
                NodeKind::Function,
                (9, 0, 9, 46),
                false,
                s("int"),
                None
            ),
            (
                "add",
                NodeKind::Function,
                (10, 0, 10, 51),
                true,
                s("int"),
                None
            ),
            (
                "pick",
                NodeKind::Function,
                (11, 0, 11, 39),
                true,
                s("enum color"),
                None
            ),
        ]
    );
    assert_eq!(g.nodes[0].content_hash, 11787528772899515626);
    assert_eq!(
        calls_of(&g, "add", (10, 0, 10, 51)),
        (vec!["sub"], vec!["\u{1}name LIMIT", "\u{1}variant LIMIT"])
    );
    assert_eq!(
        calls_of(&g, "pick", (11, 0, 11, 39)),
        (vec![], vec!["\u{1}name GREEN", "\u{1}variant GREEN"])
    );
    // Named-binding imports follow source order.
    assert_eq!(
        import_rows(&g),
        [
            binding("counter_t", "unsigned long", BindingKind::Alias),
            binding("point_t", "struct point", BindingKind::Alias),
            binding("cmp_fn", "int", BindingKind::Alias),
        ]
    );
    let metas: Vec<FnMetaRow<'_>> = g
        .raw_function_metas
        .iter()
        .map(|m| {
            (
                m.span,
                m.flags,
                m.params.iter().map(String::as_str).collect(),
                m.return_type.as_str(),
            )
        })
        .collect();
    let ab = || vec!["a", "int", "b", "int"];
    assert_eq!(
        metas,
        [
            ((8, 0, 8, 22), 32, ab(), "int"),
            ((9, 0, 9, 46), 4, ab(), "int"),
            ((10, 0, 10, 51), 0, ab(), "int"),
            ((11, 0, 11, 39), 0, vec![], "enum color"),
        ]
    );
    assert!(g.blind_spots.is_empty() && g.call_metas.is_empty());
}

const OPS: &[u8] = br#"struct file_ops { int (*open)(const char *p); int (*close)(int fd); };
static int my_open(const char *p) { return 0; }
static int my_close(int fd) { return fd; }
static struct file_ops fops = { .open = my_open, .close = my_close };
int run(struct file_ops *ops) { return ops->open("x") + fops.close(1); }
"#;

/// The ops-struct initialiser types `fops`; both member calls through a
/// function-pointer field become indirect call metas on `run`.
#[test]
fn test_c_driver_fn_ptr_ops_struct_emits_indirect_call_metas() {
    let g = parse(OPS);
    let s = Some;
    assert_eq!(
        node_rows(&g),
        [
            (
                "file_ops",
                NodeKind::Struct,
                (0, 0, 0, 69),
                true,
                None,
                None
            ),
            (
                "my_open",
                NodeKind::Function,
                (1, 0, 1, 47),
                false,
                s("int"),
                None
            ),
            (
                "my_close",
                NodeKind::Function,
                (2, 0, 2, 42),
                false,
                s("int"),
                None
            ),
            (
                "fops",
                NodeKind::Variable,
                (3, 0, 3, 69),
                true,
                s("static struct file_ops"),
                None
            ),
            (
                "run",
                NodeKind::Function,
                (4, 0, 4, 72),
                true,
                s("int"),
                None
            ),
        ]
    );
    assert_eq!(
        calls_of(&g, "run", (4, 0, 4, 72)),
        (vec!["close", "open"], vec!["\u{1}name fops"])
    );
    let metas: Vec<CallMetaRow<'_>> = g
        .call_metas
        .iter()
        .map(|m| {
            (
                m.caller_name.as_str(),
                m.caller_span,
                m.call_index,
                m.flags,
                m.dispatch_type.as_str(),
            )
        })
        .collect();
    assert_eq!(
        metas,
        [
            ("run", (4, 0, 4, 72), 1, 6, "struct file_ops"),
            ("run", (4, 0, 4, 72), 0, 6, "struct file_ops"),
        ]
    );
    assert!(g.imports.is_empty());
}

const FALLBACK: &[u8] = br#"#define OUTER(x) \
  #define NESTED x
const char *s = "a\
#define INSTR 1\
";
#define OUTER2 1
"#;

/// `NESTED` sits inside `OUTER`'s continued body, so only the raw-source
/// fallback sees it: it is pushed after every query node, with hash 0.
#[test]
fn test_c_driver_macro_fallback_node_pushed_last_with_zero_hash() {
    let g = parse(FALLBACK);
    let s = Some;
    assert_eq!(
        node_rows(&g),
        [
            ("OUTER", NodeKind::Macro, (0, 0, 2, 0), true, None, None),
            (
                "s",
                NodeKind::Variable,
                (2, 0, 4, 2),
                true,
                s("const char *"),
                None
            ),
            ("OUTER2", NodeKind::Macro, (5, 0, 6, 0), true, None, None),
            ("NESTED", NodeKind::Macro, (1, 10, 1, 16), true, None, None),
        ]
    );
    let hashes: Vec<u64> = g.nodes.iter().map(|n| n.content_hash).collect();
    assert_eq!(
        hashes,
        [
            11853692101427193027,
            16283371203306133002,
            790268426025208342,
            0
        ]
    );
    // Named-binding imports follow source order.
    assert_eq!(
        import_rows(&g),
        [
            binding("OUTER", "(x) #define NESTED x", BindingKind::Macro),
            binding("OUTER2", "1", BindingKind::Constant),
        ]
    );
}

const BINDINGS: &[u8] = br#"#ifndef BINDINGS_H
#define BINDINGS_H
#define MAX 100
#define HEX 0x1FUL
#define NAME "abc"
#define ALIAS_OF MAX
#define FLAG
#define EXPR (MAX + 1)
#define ADD(a, b) ((a) + (b))
#define NOBODY(x)
extern int g_counter;
extern void ext_fn(int);
typedef char **str_arr;
#ifdef FEATURE
typedef int feature_t;
#endif
#endif
"#;

/// Each `#define` body class, `extern`, and typedefs nested in preproc
/// blocks become named-binding imports in source order. The include guard
/// is neither a Macro node nor an import; the guarded declarations are
/// nodes as at file scope.
#[test]
fn test_c_driver_named_bindings_classify_and_skip_include_guard() {
    let g = parse(BINDINGS);
    let macro_at = |name: &'static str, row: u32| -> NodeRow<'static> {
        (
            name,
            NodeKind::Macro,
            (row, 0, row + 1, 0),
            true,
            None,
            None,
        )
    };
    assert_eq!(
        node_rows(&g),
        // No guard Macro; the guarded extern, prototype and typedef are nodes.
        [
            macro_at("MAX", 2),
            macro_at("HEX", 3),
            macro_at("NAME", 4),
            macro_at("ALIAS_OF", 5),
            macro_at("FLAG", 6),
            macro_at("EXPR", 7),
            macro_at("ADD", 8),
            macro_at("NOBODY", 9),
            (
                "g_counter",
                NodeKind::Variable,
                (10, 0, 10, 21),
                true,
                Some("extern int"),
                None
            ),
            (
                "ext_fn",
                NodeKind::Function,
                (11, 0, 11, 24),
                true,
                None,
                None
            ),
            (
                "str_arr",
                NodeKind::Typedef,
                (12, 0, 12, 23),
                true,
                None,
                None
            ),
            (
                "feature_t",
                NodeKind::Typedef,
                (14, 0, 14, 22),
                true,
                None,
                None
            ),
        ]
    );
    // Source order; a pointer typedef keeps its stars in the source.
    assert_eq!(
        import_rows(&g),
        [
            binding("MAX", "100", BindingKind::Constant),
            binding("HEX", "0x1FUL", BindingKind::Constant),
            binding("NAME", "\"abc\"", BindingKind::Constant),
            binding("ALIAS_OF", "MAX", BindingKind::Alias),
            binding("FLAG", "", BindingKind::Flag),
            binding("EXPR", "(MAX + 1)", BindingKind::Macro),
            binding("ADD", "(a, b) ((a) + (b))", BindingKind::Macro),
            binding("NOBODY", "(x)", BindingKind::Macro),
            binding("g_counter", "external", BindingKind::Alias),
            binding("ext_fn", "external", BindingKind::Alias),
            binding("str_arr", "char **", BindingKind::Alias),
            binding("feature_t", "int", BindingKind::Alias),
        ]
    );
}

const DLSYM: &[u8] = br#"#include <dlfcn.h>
void load(void) {
    void *h = dlopen("lib.so", 0);
    void (*fn)(void) = dlsym(h, "entry");
    fn();
}
"#;

/// `dlsym(...)` is the one C blind spot; `is_test` follows the path.
#[test]
fn test_c_driver_dlsym_call_emits_blind_spot_with_test_flag() {
    for (path, is_test) in [("dlsym.c", false), ("tests/test_load.c", true)] {
        let g = parse_at(path, DLSYM);
        let spots: Vec<(&str, &Path, Span, bool)> = g
            .blind_spots
            .iter()
            .map(|b| (b.kind.as_str(), b.file_path.as_path(), b.span, b.is_test))
            .collect();
        assert_eq!(
            spots,
            [("c-dlsym", Path::new(path), (3, 23, 3, 40), is_test)],
            "{path}"
        );
        assert!(g.blind_spots[0]
            .hint
            .starts_with("dlsym(<handle>, <name>) — runtime symbol resolution"));
        assert_eq!(
            node_rows(&g),
            [(
                "load",
                NodeKind::Function,
                (1, 0, 5, 1),
                true,
                Some("void"),
                None
            )]
        );
        assert_eq!(
            calls_of(&g, "load", (1, 0, 5, 1)).0,
            ["fn", "dlsym", "dlopen"]
        );
        assert_eq!(import_rows(&g), [("<dlfcn.h>", "*", None, None)]);
    }
}

const RECEIVER: &[u8] = br#"struct widget { int w; };
int widget_width(struct widget *self) { return self->w; }
int widget_area(struct widget *self, int h) { return widget_width(self) * h; }
"#;

/// A `struct T *self` first parameter makes a function a method on `T`:
/// its owner is `T`, and a call to it is qualified as `T.fn`.
#[test]
fn test_c_driver_self_receiver_sets_owner_and_qualified_call() {
    let g = parse(RECEIVER);
    let s = Some;
    assert_eq!(
        node_rows(&g),
        [
            ("widget", NodeKind::Struct, (0, 0, 0, 24), true, None, None),
            (
                "w",
                NodeKind::Property,
                (0, 16, 0, 22),
                true,
                s("int"),
                s("widget")
            ),
            (
                "widget_width",
                NodeKind::Function,
                (1, 0, 1, 57),
                true,
                s("int"),
                s("widget")
            ),
            (
                "widget_area",
                NodeKind::Function,
                (2, 0, 2, 78),
                true,
                s("int"),
                s("widget")
            ),
        ]
    );
    assert_eq!(
        calls_of(&g, "widget_width", (1, 0, 1, 57)),
        (vec![], vec!["w"])
    );
    assert_eq!(
        calls_of(&g, "widget_area", (2, 0, 2, 78)),
        (vec!["widget.widget_width"], vec![])
    );
}
