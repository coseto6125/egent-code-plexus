//! Repro for nlohmann/json.hpp `template<...> class basic_json { reference at(...) {...} }`:
//! cgn emits 18 Method nodes for json.hpp but completely misses `at`, `accept`,
//! `count`, `contains`, `emplace`, `erase`, `compute_boundaries`, ... — all template
//! class members whose body contains `JSON_TRY/JSON_CATCH/JSON_THROW` macros plus
//! K&R-style brace-on-newline.
//!
//! Two hypotheses to disambiguate:
//! 1. brace-on-newline confuses queries.scm patterns
//! 2. tree-sitter-cpp produces ERROR nodes inside method bodies when macros
//!    aren't recognized, suppressing the outer @method match

use cgn_analyzer::cpp::parser::CppProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CppProvider::new().expect("CppProvider init");
    let graph = provider
        .parse_file(Path::new("t.cpp"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in nodes={nodes:#?}"))
}

#[test]
fn brace_on_newline_still_emits_method() {
    // Hypothesis 1: brace style alone is fine
    let src = "template <typename T> class C {\n\
               int at(int idx)\n\
               {\n\
                   return idx;\n\
               }\n\
               };\n";
    let nodes = parse(src);
    find(&nodes, "at", NodeKind::Method);
}

#[test]
fn macro_in_body_still_emits_method() {
    // Hypothesis 2: unknown macro inside body — simulates JSON_TRY/JSON_THROW
    let src = "template <typename T> class C {\n\
               int at(int idx)\n\
               {\n\
                   JSON_TRY { return idx; }\n\
                   JSON_CATCH (std::out_of_range&) { JSON_THROW(error{}); }\n\
               }\n\
               };\n";
    let nodes = parse(src);
    find(&nodes, "at", NodeKind::Method);
}

#[test]
fn likely_macro_in_condition_still_emits_method() {
    // tree-sitter-cpp likely choking on `JSON_HEDLEY_LIKELY(expr)` as a macro
    // that wraps a condition expression
    let src = "template <typename T> class C {\n\
               int at(int idx)\n\
               {\n\
                   if (JSON_HEDLEY_LIKELY(idx > 0)) { return idx; }\n\
                   return 0;\n\
               }\n\
               };\n";
    let nodes = parse(src);
    find(&nodes, "at", NodeKind::Method);
}

#[test]
fn typedef_return_type_still_emits_method() {
    // `reference at(size_type idx)` — both types are typedefs
    let src = "template <typename T> class C {\n\
               public:\n\
               using reference = T&;\n\
               using size_type = unsigned long;\n\
               reference at(size_type idx) { return *new T{}; }\n\
               };\n";
    let nodes = parse(src);
    find(&nodes, "at", NodeKind::Method);
}

#[test]
fn full_repro_combined() {
    // All the suspicious features together — this is the closest to json.hpp pattern
    let src = "template <typename T> class C {\n\
               public:\n\
               using reference = T&;\n\
               using size_type = unsigned long;\n\
               reference at(size_type idx)\n\
               {\n\
                   if (JSON_HEDLEY_LIKELY(is_array()))\n\
                   {\n\
                       JSON_TRY\n\
                       {\n\
                           return set_parent(m_data.m_value.array->at(idx));\n\
                       }\n\
                       JSON_CATCH (std::out_of_range&)\n\
                       {\n\
                           JSON_THROW(out_of_range::create(401, msg(), this));\n\
                       }\n\
                   }\n\
                   else\n\
                   {\n\
                       JSON_THROW(type_error::create(304, msg(), this));\n\
                   }\n\
               }\n\
               };\n";
    let nodes = parse(src);
    find(&nodes, "at", NodeKind::Method);
}
