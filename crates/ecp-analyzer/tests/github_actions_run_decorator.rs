//! `run:` steps decorate their Function node with the first word of the
//! command. A block scalar's `|` / `>` header must not become that word, or
//! every block-scalar step shares one `run:|` decorator.

use ecp_analyzer::github_actions::parser::GitHubActionsProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn run_decorators(source: &str) -> Vec<String> {
    let provider = GitHubActionsProvider::new().expect("gha provider construction");
    let graph = provider
        .parse_file(Path::new(".github/workflows/test.yml"), source.as_bytes())
        .expect("parse workflow yaml");
    graph
        .nodes
        .into_iter()
        .flat_map(|n| n.decorators)
        .filter(|d| d.starts_with("run:"))
        .collect()
}

fn steps(step_yaml: &str) -> String {
    format!("jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n{step_yaml}")
}

#[test]
fn test_run_block_scalar_literal_decorates_with_first_command_word() {
    let yaml = steps("      - run: |\n          cargo build\n          cargo test\n");
    assert_eq!(run_decorators(&yaml), vec!["run:cargo"]);
}

#[test]
fn test_run_block_scalar_folded_and_chomped_headers_are_skipped() {
    for header in [">", "|-", ">-", "|+", "|2"] {
        let yaml = steps(&format!("      - run: {header}\n          npm ci\n"));
        assert_eq!(run_decorators(&yaml), vec!["run:npm"], "header {header}");
    }
}

#[test]
fn test_run_block_scalars_with_different_commands_get_distinct_decorators() {
    let yaml = steps(
        "      - name: a\n        run: |\n          make a\n      - name: b\n        run: |\n          go test\n",
    );
    assert_eq!(run_decorators(&yaml), vec!["run:make", "run:go"]);
}

#[test]
fn test_run_plain_and_quoted_scalars_decorate_with_unquoted_first_word() {
    let yaml = steps("      - run: echo hi\n      - run: \"pwd -P\"\n      - run: 'ls -l'\n");
    assert_eq!(run_decorators(&yaml), vec!["run:echo", "run:pwd", "run:ls"]);
}

#[test]
fn test_run_block_scalar_with_empty_body_emits_no_decorator() {
    let yaml = steps("      - run: |\n      - run: echo ok\n");
    assert_eq!(run_decorators(&yaml), vec!["run:echo"]);
}

#[test]
fn test_run_whitespace_only_quoted_scalar_emits_no_decorator() {
    let yaml = steps("      - run: \"   \"\n");
    assert!(run_decorators(&yaml).is_empty());
}

#[test]
fn test_run_mapping_value_emits_no_decorator() {
    let yaml = steps("      - run:\n          cmd: build\n");
    assert!(run_decorators(&yaml).is_empty());
}
