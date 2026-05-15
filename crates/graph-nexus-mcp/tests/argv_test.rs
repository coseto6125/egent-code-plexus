use graph_nexus_mcp::argv::json_to_argv;
use serde_json::json;

#[test]
fn flat_string_args_become_double_dashed_flags() {
    let argv = json_to_argv(&json!({"name": "validateUser", "format": "json"})).unwrap();
    // Order isn't guaranteed by serde_json::Map iteration, so check membership.
    assert!(argv.windows(2).any(|w| w == ["--name", "validateUser"]));
    assert!(argv.windows(2).any(|w| w == ["--format", "json"]));
    assert_eq!(argv.len(), 4);
}

#[test]
fn bool_true_becomes_flag_only() {
    let argv = json_to_argv(&json!({"includeTests": true})).unwrap();
    assert_eq!(argv, vec!["--include-tests"]);
}

#[test]
fn bool_false_emits_nothing() {
    let argv = json_to_argv(&json!({"includeTests": false})).unwrap();
    assert!(argv.is_empty());
}

#[test]
fn null_values_are_skipped() {
    let argv = json_to_argv(&json!({"name": "foo", "uid": null})).unwrap();
    assert_eq!(argv, vec!["--name", "foo"]);
}

#[test]
fn numbers_serialize_as_strings() {
    let argv = json_to_argv(&json!({"limit": 42, "ratio": 3.14})).unwrap();
    assert!(argv.windows(2).any(|w| w == ["--limit", "42"]));
    assert!(argv.windows(2).any(|w| w == ["--ratio", "3.14"]));
}

#[test]
fn camel_case_keys_get_kebab_case_flags() {
    let argv = json_to_argv(&json!({"baseRef": "main"})).unwrap();
    assert_eq!(argv, vec!["--base-ref", "main"]);
}

#[test]
fn non_object_root_errors() {
    let res = json_to_argv(&json!([1, 2, 3]));
    assert!(res.is_err());
}
