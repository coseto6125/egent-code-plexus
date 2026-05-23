//! Ruby-side `path_literals` extractor regression tests.

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = RubyProvider::new().expect("RubyProvider::new");
    let graph = provider
        .parse_file(Path::new("test.rb"), src.as_bytes())
        .expect("parse_file");
    graph
        .path_literals
        .map(|b| b.into_vec())
        .unwrap_or_default()
}

fn find_by_value<'a>(lits: &'a [RawPathLiteral], value: &str) -> &'a RawPathLiteral {
    lits.iter()
        .find(|l| l.value == value)
        .unwrap_or_else(|| panic!("expected literal {value:?}, got: {lits:?}"))
}

#[test]
fn method_with_read_sink() {
    let src = r#"
class Loader
  def load
    File.read("session_meta.json")
  end
end
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
    assert!(
        lit.sink_reason.starts_with("sink:read"),
        "got: {}",
        lit.sink_reason
    );
}

#[test]
fn pr357_minirepro_both_literals_surface() {
    let src = r#"
class Repo
  def read_it
    File.read("meta.json")
  end
  def write_it(d)
    File.write("session_meta.json", d)
  end
end
"#;
    let lits = parse_path_literals(src);
    assert!(lits.iter().any(|l| l.value == "meta.json"));
    assert!(lits.iter().any(|l| l.value == "session_meta.json"));
}
