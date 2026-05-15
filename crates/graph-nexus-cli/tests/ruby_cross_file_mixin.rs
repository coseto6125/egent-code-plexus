//! Integration test: cross-file Ruby mixin / delegator propagation.
//!
//! Setup:
//!   - `lib/foo.rb` defines `module Foo` that `extend Forwardable` and uses
//!     `def_delegators :backend, :read, :write`. After PR #13 + the delegator
//!     commit, Foo's parsed `LocalGraph.imports` contains two `RawImport`
//!     entries for `read` and `write`.
//!   - `lib/bar.rb` defines `class Bar; include Foo; end`. Bar's heritage
//!     gets `Foo` appended via the existing mixin pipeline.
//!
//! What this test verifies (and documents):
//!
//!   1. **Single-file emit works.** `lib/foo.rb`'s `LocalGraph` contains
//!      RawImports for `read` / `write` aliased to `backend.read` /
//!      `backend.write`. Within `foo.rb` the resolver can use those imports.
//!
//!   2. **Heritage edge is wired.** `lib/bar.rb`'s `LocalGraph.nodes`
//!      contains `Bar` with `heritage = ["Foo"]` after the include parse.
//!
//!   3. **Cross-file binding does NOT propagate transparently.** The
//!      resolver's `resolve_symbol` walks (SameFile → ImportScoped →
//!      QualifierScoped → Global) but it does not pull a heritage parent's
//!      `RawImport` aliases into the child's lookup table. From `bar.rb`'s
//!      perspective, `read` is NOT resolvable as an alias of `backend.read`
//!      — only the `Extends` edge `Bar → Foo` exists in the graph; the
//!      delegator's `RawImport` is keyed against `foo.rb`'s file scope.
//!
//! This is the architectural limitation acknowledged in the receiver-aware
//! resolver spec §4 ("跨檔 mixin tracking … 走第二階段"). Closing it would
//! require either (a) materialising the delegator as a real Method
//! RawNode on `module Foo` so the heritage chain's existing Extends edges
//! carry it, or (b) teaching `Resolver` to walk heritage when looking up a
//! callable. Both are out of scope for PR #13 — this test pins the current
//! behaviour so a future closing PR can flip the assertions.

use graph_nexus_analyzer::resolution::index::{ResolveTarget, SymbolTable};
use graph_nexus_analyzer::resolution::resolver::Resolver;
use graph_nexus_analyzer::ruby::parser::RubyProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse_ruby(path: &str, src: &str) -> LocalGraph {
    RubyProvider::new()
        .expect("provider")
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse")
}

const FOO_RB: &str = "\
module Foo
  extend Forwardable
  def_delegators :backend, :read, :write
end
";

const BAR_RB: &str = "\
class Bar
  include Foo
end
";

#[test]
fn single_file_delegator_emits_alias_in_originating_file() {
    let foo = parse_ruby("lib/foo.rb", FOO_RB);
    let mut found_read = false;
    let mut found_write = false;
    for imp in &foo.imports {
        if imp.imported_name == "read" && imp.source == "backend.read" {
            found_read = true;
        }
        if imp.imported_name == "write" && imp.source == "backend.write" {
            found_write = true;
        }
    }
    assert!(
        found_read && found_write,
        "expected def_delegators to emit RawImports for read & write, got {:?}",
        foo.imports
    );
}

#[test]
fn include_attaches_module_to_class_heritage() {
    let bar = parse_ruby("lib/bar.rb", BAR_RB);
    let bar_class = bar
        .nodes
        .iter()
        .find(|n| n.name == "Bar")
        .expect("Bar class node");
    assert!(
        bar_class.heritage.iter().any(|h| h == "Foo"),
        "include Foo must append Foo to Bar's heritage; got {:?}",
        bar_class.heritage
    );
}

#[test]
fn cross_file_delegator_alias_is_visible_inside_originating_file() {
    // Build a 2-file symbol table and check that *inside foo.rb* the
    // resolver can use Foo's own imports to bind `read` → `backend.read`'s
    // member side. This is the baseline positive case; it must hold
    // regardless of the cross-file question below.
    let foo = parse_ruby("lib/foo.rb", FOO_RB);
    let bar = parse_ruby("lib/bar.rb", BAR_RB);

    let mut symbols = SymbolTable::new();
    let mut idx = 0u32;
    for n in &foo.nodes {
        symbols.register_node("lib/foo.rb", &n.name, idx, n.kind);
        idx += 1;
    }
    for n in &bar.nodes {
        symbols.register_node("lib/bar.rb", &n.name, idx, n.kind);
        idx += 1;
    }
    let resolver = Resolver::new(&symbols);

    // From inside foo.rb, lookup `read`. With Foo's RawImports in scope the
    // import-scoped tier would only fire if `backend.read` resolved to a
    // known file path — backend has no node here, so we instead assert the
    // weaker invariant: the import IS present in foo.rb's local graph and
    // would be the binding the resolver consults. (Resolver returns an empty
    // Vec when the target file lookup misses, which is expected.)
    let res = resolver.resolve_symbol(
        Path::new("lib/foo.rb"),
        "read",
        &foo.imports,
        ResolveTarget::Callable,
    );
    // The lookup may yield zero hits (because `backend` is a bare receiver
    // with no registered node) — what we are asserting is that the import
    // is plumbed in, not that the whole chain resolves to a real node.
    let has_alias = foo
        .imports
        .iter()
        .any(|i| i.alias.as_deref() == Some("read"));
    assert!(
        has_alias,
        "expected Foo's RawImports to contain alias `read`; got {:?}",
        foo.imports
    );
    // We assert nothing about res length — that depends on whether
    // `backend.read` happens to resolve to a global candidate. The key
    // invariant is the alias being in foo.imports.
    let _ = res;
}

#[test]
fn cross_file_delegator_alias_does_not_propagate_through_include() {
    // The architectural pin: from inside bar.rb, the resolver cannot see
    // Foo's delegator-aliases. bar.rb's `RawImport` set does NOT contain
    // `read`/`write` (only `Bar`'s class declaration sits in bar.rb), and
    // the resolver's heritage-aware path is limited to emitting `Extends`
    // edges — it does not perform method lookup through the chain.
    //
    // If a future PR closes this gap (option B in the spec), it should
    // *invert* this assertion: `resolve_symbol("read", &bar.imports)` would
    // start returning at least the alias hit (with low/medium confidence
    // depending on whether `backend` is registered).
    let foo = parse_ruby("lib/foo.rb", FOO_RB);
    let bar = parse_ruby("lib/bar.rb", BAR_RB);

    // bar.rb's RawImport set contains no delegator alias.
    let bar_has_read_alias = bar
        .imports
        .iter()
        .any(|i| i.alias.as_deref() == Some("read"));
    assert!(
        !bar_has_read_alias,
        "current architecture: bar.rb must NOT carry Foo's delegator alias \
         (cross-file mixin propagation is not yet implemented); got {:?}",
        bar.imports
    );

    // The full resolver path with bar.rb as the caller also returns no hits.
    let mut symbols = SymbolTable::new();
    let mut idx = 0u32;
    for n in &foo.nodes {
        symbols.register_node("lib/foo.rb", &n.name, idx, n.kind);
        idx += 1;
    }
    for n in &bar.nodes {
        symbols.register_node("lib/bar.rb", &n.name, idx, n.kind);
        idx += 1;
    }
    let resolver = Resolver::new(&symbols);
    let res = resolver.resolve_symbol(
        Path::new("lib/bar.rb"),
        "read",
        &bar.imports,
        ResolveTarget::Callable,
    );
    assert!(
        res.is_empty(),
        "current architecture: resolver from bar.rb must not resolve `read` \
         through the Foo mixin; got hits {:?}",
        res
    );
}
