//! Integration tests for Ruby named bindings.
//!
//! Covers the three statically resolvable forms:
//!   * `alias new old` keyword
//!   * `alias_method :new, :old` metaprogramming
//!   * `Foo = Some::Module::Bar` constant alias
//!
//! Each form emits a `RawImport` with `alias = Some(new_name)`. The constant
//! alias query is constrained to `left: (constant)` so lowercase local-variable
//! assignments are skipped.

use graph_nexus_analyzer::ruby::parser::RubyProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport};
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = RubyProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.rb"), source.as_bytes())
        .expect("parse")
}

fn find_binding<'a>(
    graph: &'a LocalGraph,
    new_name: &str,
    source: &str,
) -> Option<&'a RawImport> {
    graph.imports.iter().find(|i| {
        i.alias.as_deref() == Some(new_name) && i.imported_name == new_name && i.source == source
    })
}

#[test]
fn test_ruby_alias_keyword_emits_binding() {
    let graph = parse("class C\n  alias new_method old_method\nend\n");
    let binding = find_binding(&graph, "new_method", "old_method");
    assert!(
        binding.is_some(),
        "expected alias binding new_method -> old_method, got imports = {:?}",
        graph.imports
    );
}

#[test]
fn test_ruby_alias_method_metaprogramming_emits_binding() {
    let graph = parse("class C\n  alias_method :new_m, :old_m\nend\n");
    let binding = find_binding(&graph, "new_m", "old_m");
    assert!(
        binding.is_some(),
        "expected alias_method binding new_m -> old_m, got imports = {:?}",
        graph.imports
    );
}

#[test]
fn test_ruby_constant_alias_scope_resolution_emits_binding() {
    let graph = parse("MyConst = OtherModule::Constant\n");
    let binding = find_binding(&graph, "MyConst", "OtherModule::Constant");
    assert!(
        binding.is_some(),
        "expected constant alias MyConst -> OtherModule::Constant, got imports = {:?}",
        graph.imports
    );
}

#[test]
fn test_ruby_constant_alias_bare_constant_emits_binding() {
    let graph = parse("Foo = Bar\n");
    let binding = find_binding(&graph, "Foo", "Bar");
    assert!(
        binding.is_some(),
        "expected constant alias Foo -> Bar, got imports = {:?}",
        graph.imports
    );
}

#[test]
fn test_ruby_local_variable_assignment_is_not_a_binding() {
    // lowercase identifier on the lhs is `identifier`, not `constant`, so the
    // constant-alias query MUST NOT fire.
    let graph = parse("local_var = some_method()\n");
    assert!(
        graph
            .imports
            .iter()
            .all(|i| i.imported_name != "local_var"),
        "local variable assignment leaked as named binding: {:?}",
        graph.imports
    );
}

#[test]
fn test_ruby_alias_keyword_outside_class_emits_binding() {
    // `alias` at the top level is legal Ruby (rare but valid). The binding is
    // still useful for downstream rename, so we emit it.
    let graph = parse("alias top_new top_old\n");
    let binding = find_binding(&graph, "top_new", "top_old");
    assert!(
        binding.is_some(),
        "expected top-level alias binding, got imports = {:?}",
        graph.imports
    );
}
