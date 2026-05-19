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

fn find_binding<'a>(graph: &'a LocalGraph, new_name: &str, source: &str) -> Option<&'a RawImport> {
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
        graph.imports.iter().all(|i| i.imported_name != "local_var"),
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

// --- def_delegator / def_delegators / delegate metaprogramming ---
//
// Each delegated method is emitted as a `RawImport`:
//   { alias: Some(method), imported_name: method, source: "<target>.<method>" }
// The presence of `extend Forwardable` inside the same enclosing class
// upgrades confidence (currently advisory only — no field on RawImport yet);
// without Forwardable we still emit (Option-A fallback per the receiver-aware
// resolver spec) and accept the known false positive: a user-defined
// `def def_delegator(a, b); …; end` whose CALL site is indistinguishable from
// the real Forwardable metaprogramming.

#[test]
fn test_ruby_def_delegator_with_forwardable_emits_binding() {
    let graph = parse("class Album\n  extend Forwardable\n  def_delegator :@songs, :each\nend\n");
    let binding = find_binding(&graph, "each", "songs.each");
    assert!(
        binding.is_some(),
        "expected def_delegator binding each -> songs.each, got imports = {:?}",
        graph.imports
    );
}

#[test]
fn test_ruby_def_delegators_emits_one_binding_per_method() {
    let graph = parse(
        "class Album\n  extend Forwardable\n  def_delegators :@songs, :foo, :bar, :baz\nend\n",
    );
    for method in ["foo", "bar", "baz"] {
        let source = format!("songs.{method}");
        let binding = find_binding(&graph, method, &source);
        assert!(
            binding.is_some(),
            "expected def_delegators binding {method} -> {source}, got imports = {:?}",
            graph.imports
        );
    }
}

#[test]
fn test_ruby_delegate_rails_style_emits_bindings() {
    let graph = parse("class Order\n  delegate :address, :phone, to: :customer\nend\n");
    for method in ["address", "phone"] {
        let source = format!("customer.{method}");
        let binding = find_binding(&graph, method, &source);
        assert!(
            binding.is_some(),
            "expected delegate binding {method} -> {source}, got imports = {:?}",
            graph.imports
        );
    }
}

#[test]
fn test_ruby_def_delegator_without_forwardable_still_emits() {
    // Option-A low-confidence fallback: emit even when `extend Forwardable`
    // is absent. This is the deliberate trade-off for not having a
    // BindingKind discriminant on RawImport yet; the false-positive case
    // (a user-defined call named `def_delegator(a, b)`) is documented as a
    // known limitation in the receiver-aware-resolver spec.
    let graph = parse("class Plain\n  def_delegator :@inner, :run\nend\n");
    let binding = find_binding(&graph, "run", "inner.run");
    assert!(
        binding.is_some(),
        "expected fallback binding even without extend Forwardable, got imports = {:?}",
        graph.imports
    );
}

#[test]
fn test_ruby_user_defined_def_delegator_method_is_not_a_binding() {
    // Defining a method named `def_delegator` does NOT trigger emission —
    // `def def_delegator(...)` parses as a `method` node, distinct from the
    // `call` node the query matches. Only an actual *call* to a same-named
    // identifier would be a false positive, and that case is the known
    // limitation we document but do not try to disambiguate.
    let graph = parse("class C\n  def def_delegator(a, b)\n    nil\n  end\nend\n");
    assert!(
        graph
            .imports
            .iter()
            .all(|i| i.imported_name != "def_delegator" && !i.source.contains("def_delegator")),
        "user-defined def_delegator method leaked as a delegator binding: {:?}",
        graph.imports
    );
}

#[test]
fn test_ruby_def_delegator_top_level_is_ignored_gracefully() {
    // Top-level `def_delegator` outside any class. Current behaviour: the
    // call is still parsed and the binding is emitted with the same shape;
    // we do not try to enforce class scope on the metaprogramming call
    // itself (matches the existing `alias` top-level behaviour above).
    let graph = parse("def_delegator :@target, :read\n");
    let binding = find_binding(&graph, "read", "target.read");
    assert!(
        binding.is_some(),
        "expected top-level def_delegator binding (graceful fallback), got imports = {:?}",
        graph.imports
    );
}
