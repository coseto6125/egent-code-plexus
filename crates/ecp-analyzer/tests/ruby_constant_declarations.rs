//! Ruby class/module-body constants — `UPPERCASE = <value>` — must emit
//! as Const. Before Round 68, queries.scm only had a `const_alias` rule
//! (matching `MyConst = OtherConst`), so general declarations with
//! non-constant RHS (hashes, integers, regex, symbols, arrays) were
//! silently dropped.
//!
//! Repro from Rails sources: HostAuthorization::DOT, HostAuthorization::
//! PORT_REGEXP, RequestId::DEFAULT_OPTIONS, AuthenticityToken::TOKEN_LENGTH
//! etc. all returned 0 Const nodes before the fix.

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = RubyProvider::new().expect("RubyProvider init");
    p.parse_file(Path::new("t.rb"), src.as_bytes())
        .expect("parse_file")
}

fn consts(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn class_body_integer_const_emits() {
    let g = parse("class AuthenticityToken\n  TOKEN_LENGTH = 32\nend\n");
    assert!(consts(&g).contains(&"TOKEN_LENGTH"));
}

#[test]
fn class_body_string_const_emits() {
    let g = parse("class HostAuthorization\n  DOT = '.'\nend\n");
    assert!(consts(&g).contains(&"DOT"));
}

#[test]
fn class_body_regex_const_emits() {
    let g = parse(
        r#"
class HostAuthorization
  PORT_REGEXP = /:\d+\z/.freeze
  SUBDOMAINS = /[a-z0-9\-.]+/.freeze
end
"#,
    );
    let cs = consts(&g);
    assert!(cs.contains(&"PORT_REGEXP"), "{cs:?}");
    assert!(cs.contains(&"SUBDOMAINS"), "{cs:?}");
}

#[test]
fn class_body_hash_const_emits() {
    let g = parse(
        r#"
class Base
  DEFAULT_OPTIONS = {
    reaction: :default_reaction,
    logging: true,
  }
end
"#,
    );
    assert!(consts(&g).contains(&"DEFAULT_OPTIONS"));
}

#[test]
fn class_body_symbol_array_const_emits() {
    let g = parse("class CSP\n  DIRECTIVES = %i[base_uri child_src connect_src]\nend\n");
    assert!(consts(&g).contains(&"DIRECTIVES"));
}

#[test]
fn module_body_const_emits() {
    // Module-level constants are equally type-level.
    let g = parse("module Net\n  HTTP_PORT = 80\nend\n");
    assert!(consts(&g).contains(&"HTTP_PORT"));
}

#[test]
fn top_level_const_emits() {
    // Plain `CONST = value` at the file root.
    let g = parse("VERSION = '1.0.0'\n");
    assert!(consts(&g).contains(&"VERSION"));
}

#[test]
fn const_alias_still_emits_const_node() {
    // `MyConst = OtherConst` is BOTH a Const node (the new declaration)
    // AND an alias binding (FQN resolution). Verify the Const survives
    // the overlap with the legacy const_alias rule.
    let g = parse("class Foo\n  Aliased = OtherModule::Original\nend\n");
    assert!(consts(&g).contains(&"Aliased"));
}

#[test]
fn lowercase_assignment_does_not_emit_const() {
    // Lowercase identifiers parse as `identifier`, not `constant`. The
    // query's lhs constraint must keep these out.
    let g = parse("class Foo\n  local_var = 5\nend\n");
    let cs = consts(&g);
    assert!(
        !cs.contains(&"local_var"),
        "lowercase locals must not leak as Const: {cs:?}"
    );
}
