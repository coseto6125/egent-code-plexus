use ecp_cli::hint::{collision_warning, empty_result, error_with_cause, fuzzy_suggestions};

#[test]
fn empty_result_format() {
    let msg = empty_result("foo", "symbol", "ecp find foo --mode bm25");
    assert!(msg.contains("No"));
    assert!(msg.contains("foo"));
    assert!(msg.contains("ecp find"));
    assert!(msg.lines().count() <= 3);
}

#[test]
fn fuzzy_suggestions_format() {
    let msg = fuzzy_suggestions("validate", &["validateUser", "validate_input", "Validator"]);
    assert!(msg.contains("Did you mean"));
    assert!(msg.contains("validateUser"));
    assert!(msg.contains("Validator"));
}

#[test]
fn fuzzy_suggestions_no_candidates() {
    let msg = fuzzy_suggestions("xyz", &[]);
    assert!(msg.contains("No matches"));
    assert!(msg.contains("xyz"));
}

#[test]
fn error_with_cause_three_lines() {
    let msg = error_with_cause(
        "Index build failed",
        "framework not recognized",
        "ecp coverage --blind-spots",
    );
    let lines: Vec<&str> = msg.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("✗"));
    assert!(lines[1].contains("cause:"));
    assert!(lines[2].contains("next:"));
}

#[test]
fn collision_warning_lists_locations() {
    let msg = collision_warning("checkUser", &["src/utils/check.rs:50".to_string()]);
    assert!(msg.contains("COLLISION"));
    assert!(msg.contains("checkUser"));
    assert!(msg.contains("src/utils/check.rs:50"));
}
