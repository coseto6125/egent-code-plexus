//! Integration tests for GitHub Actions `uses:` directive detection.
//!
//! Validates that the GitHubActionsProvider emits `RawImport` entries for the
//! five canonical `uses:` forms (step-level public actions with tag/SHA refs,
//! step-level local composite actions, job-level local reusable workflows,
//! and job-level cross-repo reusable workflows), and emits no imports for
//! workflows that contain no `uses:` directives.

use ecp_analyzer::github_actions::parser::GitHubActionsProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawImport;
use std::path::Path;

fn parse(source: &str) -> Vec<RawImport> {
    let provider = GitHubActionsProvider::new().expect("gha provider construction");
    let graph = provider
        .parse_file(Path::new(".github/workflows/test.yml"), source.as_bytes())
        .expect("parse workflow yaml");
    graph.imports
}

#[test]
fn test_step_uses_public_action_with_tag_ref() {
    let yaml = "\
name: CI
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
";
    let imports = parse(yaml);
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    let imp = &imports[0];
    assert_eq!(imp.imported_name, "actions/checkout");
    assert_eq!(imp.source, "actions/checkout@v4");
    assert_eq!(imp.alias, None);
}

#[test]
fn test_step_uses_public_action_with_sha_ref() {
    let yaml = "\
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@a1b2c3d
";
    let imports = parse(yaml);
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    let imp = &imports[0];
    assert_eq!(imp.imported_name, "actions/checkout");
    assert_eq!(imp.source, "actions/checkout@a1b2c3d");
}

#[test]
fn test_step_uses_local_composite_action_no_ref() {
    let yaml = "\
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: ./.github/actions/local
";
    let imports = parse(yaml);
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    let imp = &imports[0];
    // Local composite has no `@ref` — imported_name and source must match.
    assert_eq!(imp.imported_name, "./.github/actions/local");
    assert_eq!(imp.source, "./.github/actions/local");
}

#[test]
fn test_job_uses_local_reusable_workflow() {
    let yaml = "\
jobs:
  call:
    uses: ./.github/workflows/build.yml
";
    let imports = parse(yaml);
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    let imp = &imports[0];
    assert_eq!(imp.imported_name, "./.github/workflows/build.yml");
    assert_eq!(imp.source, "./.github/workflows/build.yml");
}

#[test]
fn test_job_uses_cross_repo_reusable_workflow() {
    let yaml = "\
jobs:
  call:
    uses: org/repo/.github/workflows/build.yml@main
";
    let imports = parse(yaml);
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    let imp = &imports[0];
    assert_eq!(imp.imported_name, "org/repo/.github/workflows/build.yml");
    assert_eq!(imp.source, "org/repo/.github/workflows/build.yml@main");
}

#[test]
fn test_workflow_without_uses_emits_no_imports() {
    let yaml = "\
name: CI
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo hi
      - name: build
        run: cargo build
";
    let imports = parse(yaml);
    assert!(imports.is_empty(), "expected no imports, got {imports:?}");
}

#[test]
fn test_mixed_step_and_job_uses_emit_all_imports() {
    // Sanity check: a real workflow combining step-level public action,
    // step-level local composite, and a separate reusable-workflow job
    // should surface all three as distinct RawImports.
    let yaml = "\
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: ./.github/actions/setup
      - run: cargo test
  call:
    uses: org/repo/.github/workflows/release.yml@v1
";
    let imports = parse(yaml);
    assert_eq!(imports.len(), 3, "expected three imports, got {imports:?}");
    let sources: Vec<&str> = imports.iter().map(|i| i.source.as_str()).collect();
    assert!(sources.contains(&"actions/checkout@v4"));
    assert!(sources.contains(&"./.github/actions/setup"));
    assert!(sources.contains(&"org/repo/.github/workflows/release.yml@v1"));
}

#[test]
fn test_quoted_uses_value_strips_quotes() {
    // Edge case: YAML allows quoting scalar values — the scalar_text helper
    // already strips quotes, so the import data should match the unquoted form.
    let yaml = "\
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: \"actions/checkout@v4\"
";
    let imports = parse(yaml);
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    assert_eq!(imports[0].imported_name, "actions/checkout");
    assert_eq!(imports[0].source, "actions/checkout@v4");
}
