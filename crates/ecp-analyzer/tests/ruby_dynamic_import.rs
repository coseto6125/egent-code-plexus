//! FU-2026-11-01-c07a40e90e36, dynamic module load: Ruby `require_relative`
//! inside a method.
//!
//! Load form: `require_relative 'a'` inside a method, then a call.
//! `src/ruby/queries.scm:40` captures `require` and `require_relative` calls as
//! imports, so the file gets an `Imports` edge (0.9). A bare call through the
//! import (`run`) stays unbound (a known gap); with a decoy `run` in
//! `b.rb` the bare name is ambiguous. `A.run` is a qualified call: the resolver
//! looks `A` up as a type by name, independent of the require. Expected:
//! `Imports` edge to `a.rb`; bare `run` binds neither file; `A.run` binds `a.rb`
//! (qualifier lookup) and not the decoy `B.run`.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
use crate::instantiation_calls_support;

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&RubyProvider::new().expect("RubyProvider::new"), files)
}

/// `owner.name` of every `Calls` edge from `caller` into `file`.
fn calls_into(graph: &ZeroCopyGraph, caller: &str, file: &str) -> Vec<String> {
    calls_from(graph, caller)
        .into_iter()
        .filter(|hit| hit.file == file)
        .map(|hit| format!("{}.{}", hit.owner, hit.name))
        .collect()
}

/// Target files of every `Imports` edge of the File node named `file`.
#[allow(dead_code)]
fn imports_of(graph: &ZeroCopyGraph, file: &str) -> Vec<String> {
    edges_from(graph, file, RelType::Imports)
        .into_iter()
        .map(|hit| hit.file)
        .collect()
}

const TARGET: (&str, &str) = (
    "a.rb",
    "class A\n  def self.run; end\nend\n\ndef run; end\n",
);
const DECOY: (&str, &str) = (
    "b.rb",
    "class B\n  def self.run; end\nend\n\ndef run; end\n\ndef plain; end\n",
);

const BARE: (&str, &str) = ("app.rb", "def load\n  require_relative 'a'\n  run\nend\n");
const QUALIFIED: (&str, &str) = ("app.rb", "def load\n  require_relative 'a'\n  A.run\nend\n");

#[test]
fn test_require_relative_in_method_emits_imports_edge() {
    let g = graph(&[TARGET, DECOY, BARE]);
    assert_eq!(imports_of(&g, "app.rb"), vec!["a.rb".to_string()]);
}

#[test]
fn test_require_relative_bare_call_binds_neither_run() {
    let g = graph(&[TARGET, DECOY, BARE]);
    let calls = calls_from(&g, "load");
    assert!(
        calls.iter().all(|hit| hit.name != "run"),
        "known-gap pin: load calls: {calls:?}"
    );
}

#[test]
fn test_require_relative_qualified_call_binds_a_not_decoy() {
    let g = graph(&[TARGET, DECOY, QUALIFIED]);
    assert_eq!(calls_into(&g, "load", DECOY.0), Vec::<String>::new());
    assert_eq!(calls_into(&g, "load", TARGET.0), vec!["A.run".to_string()]);
}

/// Control: a plain cross-file call to a unique name binds.
#[test]
fn test_require_relative_control_plain_cross_file_call_binds() {
    let g = graph(&[DECOY, ("ctl.rb", "def control\n  plain()\nend\n")]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec![".plain".to_string()]
    );
}
