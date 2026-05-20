//! Visibility checks for the GitHub Actions provider.
//!
//! Job-level `outputs:` keys declared under `jobs.<id>.outputs` are exported
//! (consumed downstream via `needs.<id>.outputs.<key>`).
//! Workflow-level `on.workflow_call.outputs` keys are also exported (consumed
//! by reusable-workflow callers).
//! Step `id:` values are job-internal and must NOT be exported.

use ecp_analyzer::github_actions::parser::GitHubActionsProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = GitHubActionsProvider::new().expect("GitHubActionsProvider init");
    let graph = provider
        .parse_file(Path::new(".github/workflows/test.yml"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn job_output_key_is_exported() {
    let yaml = "\
name: CI
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    outputs:
      artifact_id: ${{ steps.upload.outputs.id }}
    steps:
      - id: upload
        run: echo done
";
    let nodes = parse(yaml);
    let out = find(&nodes, "build/artifact_id", NodeKind::Property);
    assert!(
        out.is_exported,
        "`build/artifact_id` job output must be exported"
    );
}

#[test]
fn step_id_is_not_exported() {
    let yaml = "\
name: CI
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - id: install
        run: npm install
";
    let nodes = parse(yaml);
    // step `id` is tracked as a Function node (run: step)
    let step = find(&nodes, "build/step0", NodeKind::Function);
    assert!(
        !step.is_exported,
        "step `install` (id) must NOT be exported"
    );
}

#[test]
fn workflow_call_output_is_exported() {
    let yaml = "\
name: Reusable
on:
  workflow_call:
    outputs:
      artifact_url:
        description: 'URL of the built artifact'
        value: ${{ jobs.build.outputs.url }}
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo build
";
    let nodes = parse(yaml);
    let out = find(&nodes, "artifact_url", NodeKind::Property);
    assert!(
        out.is_exported,
        "`artifact_url` workflow_call output must be exported"
    );
}

#[test]
fn job_output_and_step_id_together() {
    let yaml = "\
name: Mixed
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    outputs:
      result: ${{ steps.compile.outputs.status }}
    steps:
      - id: compile
        run: cargo build
";
    let nodes = parse(yaml);

    let out = find(&nodes, "build/result", NodeKind::Property);
    assert!(
        out.is_exported,
        "`build/result` job output must be exported"
    );

    // The run step is emitted as a Function; step id is internal
    let step = find(&nodes, "build/step0", NodeKind::Function);
    assert!(!step.is_exported, "step id `compile` must NOT be exported");
}
