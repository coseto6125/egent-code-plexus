//! C++ member calls and member ownership, in the shapes nlohmann/json uses.
//!
//! - A. A member call on a receiver the binder cannot type (`std::string f;
//!   f.size()`) is an untyped member call: it never binds a free `size()` or
//!   the caller's own `size()`. `this->size()` still types as the class.
//! - B. Every method declared in a class body carries its class as owner:
//!   templated, nested, out-of-class (`R basic_json<T>::size()`,
//!   `void ns::Foo::bar()`), after a macro line (`TPL_DECL class X`), and in
//!   a class tree-sitter cannot parse (`#if` in a constructor initializer).
//! - D. Templated and qualified bases are heritage (`Proto<CharType>` →
//!   `Proto`, `detail::foo`), so an inherited implicit-`this` call binds
//!   the base's member.
//! - F. A receiver that is a field of the enclosing class (`sax->m()`,
//!   `this->sax->m()`, a field declared after the method) types as the
//!   field's declared type; a local of the same name hides the field.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{CallSite, LocalGraph, RawNode};
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn parse(path: &str, source: &str) -> LocalGraph {
    CppProvider::new()
        .expect("CppProvider::new")
        .parse_file(Path::new(path), source.as_bytes())
        .expect("parse_file")
}

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = CppProvider::new().expect("CppProvider::new");
    let mut builder = GraphBuilder::new();
    for &(path, source) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .expect("parse_file"),
        );
    }
    builder.build()
}

/// `(callee name, callee owner_class)` of every `Calls` edge out of the one
/// node named `caller`, sorted. A free function's owner is `""`.
fn calls_from(graph: &ZeroCopyGraph, caller: &str) -> Vec<(String, String)> {
    let pool = &graph.string_pool;
    let name = |i: u32| graph.nodes[i as usize].name.resolve(pool).to_string();
    let callers: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| name(i) == caller)
        .collect();
    assert_eq!(callers.len(), 1, "one `{caller}` node");
    let mut out: Vec<(String, String)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == callers[0])
        .map(|e| {
            (
                name(e.target),
                graph.nodes[e.target as usize]
                    .owner_class
                    .resolve(pool)
                    .to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

fn named<'g>(g: &'g LocalGraph, name: &str) -> Vec<&'g RawNode> {
    let mut nodes: Vec<&RawNode> = g.nodes.iter().filter(|n| n.name == name).collect();
    nodes.sort_by_key(|n| n.span);
    nodes
}

/// The one node named `name` whose span starts on 0-based `row`.
fn at<'g>(g: &'g LocalGraph, name: &str, row: u32) -> &'g RawNode {
    let hits: Vec<&RawNode> = named(g, name)
        .into_iter()
        .filter(|n| n.span.0 == row)
        .collect();
    assert_eq!(hits.len(), 1, "one `{name}` on row {row}: {:?}", g.nodes);
    hits[0]
}

fn owners<'g>(g: &'g LocalGraph, name: &str) -> Vec<Option<&'g str>> {
    named(g, name)
        .into_iter()
        .map(|n| n.owner_class.as_deref())
        .collect()
}

/// The `calls` of the one node named `name`, sorted: the extractor records
/// a function's calls in tree-walk order, not source order.
fn calls_of(g: &LocalGraph, name: &str) -> Vec<String> {
    let nodes = named(g, name);
    assert_eq!(nodes.len(), 1, "one `{name}` node: {:?}", g.nodes);
    let mut calls = nodes[0].calls.clone();
    calls.sort();
    calls
}

// ---- A: untyped receivers ----

const LEXER_STRING_SIZE: &str = "\
#include <string>
unsigned size(const char* s) { return 0; }
class lexer {
  public:
    unsigned size() const { return 1; }
    void scan() {
        std::string f;
        f.size();
    }
};
";

const LEXER_THIS_SIZE: &str = "\
unsigned size(const char* s) { return 0; }
class lexer {
  public:
    unsigned size() const { return 1; }
    void scan() {
        this->size();
    }
};
";

#[test]
fn test_cpp_callee_name_untyped_receivers_records_untyped_member() {
    let g = parse(
        "a.hpp",
        "\
#include <string>
std::string make();
class lexer {
  public:
    unsigned size() const { return 1; }
    void scan() {
        std::string f;
        f.size();
        make().size();
        this->size();
    }
};
",
    );
    let calls = calls_of(&g, "scan");
    let untyped = CallSite::untyped_member("size");
    assert_eq!(
        calls.iter().filter(|c| **c == untyped).count(),
        2,
        "`f.size()` and `make().size()` are untyped member calls: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c == "lexer.size"),
        "`this->size()` types as the class: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c == "size"),
        "no member call is recorded as a bare `size()`: {calls:?}"
    );
}

/// Depends on the resolver too: an untyped member call must bind no free
/// function, and no member of the caller's class by that name alone.
#[test]
fn test_resolve_call_untyped_string_receiver_binds_no_size() {
    let graph = build(&[("a.hpp", LEXER_STRING_SIZE)]);
    assert_eq!(
        calls_from(&graph, "scan"),
        Vec::<(String, String)>::new(),
        "`f.size()` on a `std::string` binds neither the free nor the own `size`"
    );
}

/// Depends on the resolver too: the qualified callee `lexer.size` must bind
/// the class's own member, not the same-file free `size`.
#[test]
fn test_resolve_call_this_receiver_binds_own_size() {
    let graph = build(&[("a.hpp", LEXER_THIS_SIZE)]);
    assert_eq!(
        calls_from(&graph, "scan"),
        vec![("size".to_string(), "lexer".to_string())]
    );
}

// ---- B: owners of class members ----

const TEMPLATE_LIB: &str = "\
namespace ns {
template<typename T>
class basic_json {
  public:
    using size_type = unsigned long;
    size_type size() const noexcept { return 0; }
    void begin() {}
    void decl_only();
    class iter {
      public:
        int deref() { return 1; }
    };
    template<typename U>
    struct inner_tpl {
        int get() { return 2; }
    };
};

template<typename T>
void basic_json<T>::decl_only() {}

struct Plain { void run(); };
void Plain::run() {}
}
";

#[test]
fn test_parse_file_class_template_members_carry_owner() {
    let g = parse("lib.hpp", TEMPLATE_LIB);
    assert_eq!(owners(&g, "size"), [Some("basic_json")]);
    assert_eq!(owners(&g, "begin"), [Some("basic_json")]);
    assert_eq!(owners(&g, "deref"), [Some("iter")]);
    assert_eq!(owners(&g, "get"), [Some("inner_tpl")]);
    // In-class declaration and the out-of-class template definition.
    assert_eq!(
        owners(&g, "decl_only"),
        [Some("basic_json"), Some("basic_json")]
    );
    // The out-of-class definition belongs to its qualifier, not to `ns`.
    assert_eq!(owners(&g, "run"), [Some("Plain"), Some("Plain")]);
}

#[test]
fn test_parse_file_nested_qualified_definitions_carry_innermost_owner() {
    let g = parse(
        "a.cpp",
        "\
namespace ns {
struct Foo {
    void bar();
    bool operator==(const Foo& o) const;
    Foo& operator=(const Foo& o);
};
}
void ns::Foo::bar() {}
bool ns::Foo::operator==(const Foo& o) const { return true; }
void A::B<int>::c() {}
",
    );
    let bars = named(&g, "bar");
    assert_eq!(bars.len(), 2, "declaration and definition: {:?}", g.nodes);
    assert!(bars
        .iter()
        .all(|n| n.kind == NodeKind::Method && n.owner_class.as_deref() == Some("Foo")));
    assert_eq!(owners(&g, "operator=="), [Some("Foo"), Some("Foo")]);
    assert_eq!(owners(&g, "operator="), [Some("Foo")]);
    assert_eq!(owners(&g, "c"), [Some("B")]);
    assert!(
        g.nodes.iter().all(|n| !n.name.contains("::")),
        "a qualified definition is named by its last segment: {:?}",
        g.nodes
    );
}

#[test]
fn test_parse_file_macro_line_before_class_keeps_class_and_owner() {
    let g = parse(
        "m.hpp",
        "\
#define TPL_DECL template<typename T>
TPL_DECL
class macro_json {
  public:
    int count() const { return 3; }
};
",
    );
    assert_eq!(named(&g, "macro_json").len(), 1, "{:?}", g.nodes);
    assert_eq!(named(&g, "macro_json")[0].kind, NodeKind::Class);
    assert_eq!(owners(&g, "count"), [Some("macro_json")]);
    assert_eq!(named(&g, "TPL_DECL").len(), 1, "the macro stays a node");
}

/// tree-sitter-cpp parses this whole class as an ERROR: the `#if` sits in
/// the constructor's initializer list, and `JSON_PRIVATE_UNLESS_TESTED:` is
/// a macro used as an access specifier.
const UNPARSABLE_CLASS: &str = "\
template<typename T>
class basic_json
{
  public:
    basic_json(const basic_json& other)
#if JSON_DIAGNOSTIC_POSITIONS
        : start_position(other.start_position)
#endif
    {
        other.check();
    }

  JSON_PRIVATE_UNLESS_TESTED:
    void check() const {}

  public:
    unsigned size() const noexcept { return 0; }
};
";

#[test]
fn test_parse_file_unparsable_class_body_recovers_owner_lexically() {
    let source = format!("{UNPARSABLE_CLASS}unsigned size(const char* s) {{ return 1; }}\n");
    let g = parse("json.hpp", &source);
    let class = named(&g, "basic_json")
        .into_iter()
        .filter(|n| n.kind == NodeKind::Class)
        .collect::<Vec<_>>();
    assert_eq!(class.len(), 1, "the class is recovered once: {:?}", g.nodes);
    assert_eq!((class[0].span.0, class[0].span.2), (1, 17));

    let member = at(&g, "size", 16);
    assert_eq!(member.kind, NodeKind::Method);
    assert_eq!(member.owner_class.as_deref(), Some("basic_json"));
    assert_eq!(
        at(&g, "check", 13).owner_class.as_deref(),
        Some("basic_json")
    );

    let free = at(&g, "size", 18);
    assert_eq!(free.kind, NodeKind::Function);
    assert_eq!(free.owner_class, None);
}

#[test]
fn test_resolve_call_typed_template_receiver_binds_member() {
    let graph = build(&[
        ("lib.hpp", TEMPLATE_LIB),
        (
            "main.cpp",
            "#include \"lib.hpp\"\nvoid use_it() {\n    basic_json j;\n    j.size();\n}\n",
        ),
    ]);
    assert_eq!(
        calls_from(&graph, "use_it"),
        vec![("size".to_string(), "basic_json".to_string())]
    );
}

#[test]
fn test_resolve_call_typed_receiver_of_recovered_class_binds_member() {
    let graph = build(&[
        ("json.hpp", UNPARSABLE_CLASS),
        (
            "main.cpp",
            "#include \"json.hpp\"\nvoid use_it() {\n    basic_json j;\n    j.size();\n}\n",
        ),
    ]);
    assert_eq!(
        calls_from(&graph, "use_it"),
        vec![("size".to_string(), "basic_json".to_string())]
    );
}

#[test]
fn test_parse_file_friend_function_in_class_body_is_namespace_member() {
    let g = parse(
        "f.hpp",
        "\
namespace ns {
struct Foo {
    friend void swap(Foo& a, Foo& b) {}
    friend int peek(const Foo& a);
    void own() {}
};
}
struct Bar {
    friend void bar_swap(Bar& a) {}
};
",
    );
    let swaps = named(&g, "swap");
    let swap = swaps[0];
    assert_eq!(swap.kind, NodeKind::Function);
    assert_eq!(swap.owner_class.as_deref(), Some("ns"));
    assert_eq!(owners(&g, "peek"), [Some("ns")]);
    assert_eq!(owners(&g, "bar_swap"), [None::<&str>]);
    assert_eq!(owners(&g, "own"), [Some("Foo")]);
}

#[test]
fn test_parse_file_union_static_and_anonymous_members_carry_owner() {
    let g = parse(
        "u.hpp",
        "\
union U {
    int get() { return 0; }
    static int make() { return 1; }
};
class Outer {
    struct { int inner() { return 2; } } anon;
    static Outer* instance();
};
",
    );
    assert_eq!(owners(&g, "get"), [Some("U")]);
    assert_eq!(owners(&g, "make"), [Some("U")]);
    assert_eq!(owners(&g, "instance"), [Some("Outer")]);
    // An unnamed type owns nothing by name: the nearest named class does.
    assert_eq!(owners(&g, "inner"), [Some("Outer")]);
}

// ---- D: templated and qualified bases ----

const ADAPTERS: &str = "\
namespace detail { struct foo { void ping() {} }; }
template<typename CharType> struct output_adapter_protocol {
    virtual void write_character(CharType c) = 0;
};
struct Other { void write_character(char c) {} };
struct B { void b() {} };
struct C { void c() {} };
class Multi : public B, public C {};
template<typename CharType>
class output_vector_adapter : public output_adapter_protocol<CharType>, public detail::foo {
  public:
    void run() { write_character('a'); ping(); }
};
template<typename T> struct Dependent : T::template rebind<int> {};
class Global : public ::x::Y<int>::Z {};
";

#[test]
fn test_parse_file_templated_and_qualified_bases_recorded_as_heritage() {
    let g = parse("a.hpp", ADAPTERS);
    let heritage = |name: &str| {
        let nodes = named(&g, name);
        assert_eq!(nodes.len(), 1, "one `{name}` node: {:?}", g.nodes);
        nodes[0].heritage.clone()
    };
    assert_eq!(
        heritage("output_vector_adapter"),
        ["output_adapter_protocol", "detail::foo"]
    );
    // Two bases once emitted the class twice, the second copy nameless.
    assert_eq!(heritage("Multi"), ["B", "C"]);
    assert!(g.nodes.iter().all(|n| !n.name.is_empty()), "{:?}", g.nodes);
    assert_eq!(heritage("Dependent"), Vec::<String>::new());
    assert_eq!(heritage("Global"), ["x::Y::Z"]);
}

#[test]
fn test_resolve_call_inherited_template_base_member_binds_base() {
    let graph = build(&[("a.hpp", ADAPTERS)]);
    assert_eq!(
        calls_from(&graph, "run"),
        vec![
            ("ping".to_string(), "foo".to_string()),
            (
                "write_character".to_string(),
                "output_adapter_protocol".to_string()
            ),
        ]
    );
}

// ---- F: field receivers ----

const PARSER_FIELDS: &str = "\
struct sax_t { void string(int v) {} };
struct other_t { void string(int v) {} };
class parser {
  public:
    void run() {
        sax->string(1);
        this->sax->string(2);
        late.string(3);
    }
    void shadowed() {
        other_t* sax = nullptr;
        sax->string(4);
    }
    void shadowed_auto() {
        auto sax = make();
        sax->string(5);
    }
    void out_of_class();
    other_t* make();
    sax_t* sax;
    sax_t late;
};
void parser::out_of_class() {
    sax->string(6);
    this->sax->string(7);
}
template<typename json_sax_t>
class tpl_parser {
    json_sax_t* sax = nullptr;
    void run_tpl() { sax->string(8); }
};
";

#[test]
fn test_cpp_callee_name_field_receivers_type_as_field() {
    let g = parse("p.hpp", PARSER_FIELDS);
    assert_eq!(
        calls_of(&g, "run"),
        ["sax_t.string", "sax_t.string", "sax_t.string"],
        "`sax->`, `this->sax->` and the later-declared `late.`"
    );
    assert_eq!(calls_of(&g, "shadowed"), ["other_t.string"]);
    assert_eq!(
        calls_of(&g, "shadowed_auto"),
        [CallSite::untyped_member("string"), "make".to_string()],
        "an `auto` local hides the field"
    );
    let out_of_class = named(&g, "out_of_class")
        .into_iter()
        .find(|n| !n.calls.is_empty())
        .expect("the out-of-class definition holds the calls");
    assert_eq!(out_of_class.calls, ["sax_t.string", "sax_t.string"]);
    assert_eq!(calls_of(&g, "run_tpl"), ["json_sax_t.string"]);
}

#[test]
fn test_resolve_call_field_receiver_binds_field_type_member() {
    let graph = build(&[("p.hpp", PARSER_FIELDS)]);
    let calls = calls_from(&graph, "run");
    assert!(!calls.is_empty(), "`run` calls `string`");
    assert!(
        calls
            .iter()
            .all(|(name, owner)| name == "string" && owner == "sax_t"),
        "every field-receiver call binds `sax_t::string`: {calls:?}"
    );
    assert_eq!(
        calls_from(&graph, "shadowed"),
        vec![("string".to_string(), "other_t".to_string())]
    );
}
