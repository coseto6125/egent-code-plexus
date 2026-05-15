//! AST-pattern framework detection for Ruby (Rails / Sinatra).
//!
//! Ported from upstream `_source_code/gitnexus/src/core/ingestion/languages/ruby.ts:156-178`.

use graph_nexus_analyzer::ruby::parser::RubyProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawFrameworkRef;

fn parse(src: &str) -> Vec<RawFrameworkRef> {
    let provider = RubyProvider::new().unwrap();
    let local = provider
        .parse_file("test.rb".as_ref(), src.as_bytes())
        .unwrap();
    local.framework_refs
}

fn has_framework(refs: &[RawFrameworkRef], framework: &str) -> bool {
    refs.iter().any(|r| r.target_name == framework)
}

#[test]
fn rails_application_controller_emits_ref() {
    let src = r#"
        class UsersController < ApplicationController
          before_action :authenticate
          def index; end
        end
    "#;
    assert!(has_framework(&parse(src), "rails"));
}

#[test]
fn rails_active_record_associations_emit_ref() {
    let src = r#"
        class User < ApplicationRecord
          has_many :posts
          belongs_to :tenant
          validates :email, presence: true
        end
    "#;
    let refs = parse(src);
    assert!(has_framework(&refs, "rails"));
    // Many patterns match but dedupe → only ONE rails ref.
    assert_eq!(refs.iter().filter(|r| r.target_name == "rails").count(), 1);
}

#[test]
fn sinatra_base_class_emits_ref() {
    let src = r#"
        class App < Sinatra::Base
          get '/' do
            "hello"
          end
        end
    "#;
    assert!(has_framework(&parse(src), "sinatra"));
}

#[test]
fn no_framework_patterns_no_refs() {
    let src = r#"
        class Plain
          def hello; end
        end
    "#;
    assert!(parse(src).is_empty());
}

#[test]
fn rails_and_sinatra_can_coexist() {
    // A file mixing both names emits both refs (substring scan, not
    // mutually-exclusive).
    let src = r#"
        class A < ApplicationController; end
        class B < Sinatra::Base; end
    "#;
    let refs = parse(src);
    assert!(has_framework(&refs, "rails"));
    assert!(has_framework(&refs, "sinatra"));
}
