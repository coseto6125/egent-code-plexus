use ecp_analyzer::flow::{
    analyze, Budgets, Direction, FlowReport, FlowRequest, SourceFile, Subject,
};

fn source(path: &str, text: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        source: text.into(),
    }
}

fn inspect(files: &[SourceFile], file: &str, marker: &str, subject: Subject) -> FlowReport {
    let text = &files.iter().find(|item| item.path == file).unwrap().source;
    let offset = text.find(marker).expect("unique source marker");
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap().len() + 1;
    analyze(
        files,
        &FlowRequest {
            file: file.into(),
            line,
            column,
            subject,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap()
}

fn argument(report: &FlowReport, label: &str) -> bool {
    report
        .nodes
        .iter()
        .any(|node| node.kind == "argument" && node.label == label)
}

#[test]
fn test_analyze_hoisted_var_shadows_outer_value() {
    let files = [source(
        "main.js",
        "let value = 101;\nfunction read() { consume(value); var value = 202; }\nread();",
    )];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        !argument(&report, "value"),
        "hoisted local is undefined, not outer value: {report:#?}"
    );
}

#[test]
fn test_analyze_both_return_branches_exclude_unreachable_consumer() {
    let files = [source("main.js", "let value = 101;\nfunction read(flag) { if (flag) { return 1; } else { return 2; } consume(value); }\nread(flag);")];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        !argument(&report, "value"),
        "unreachable consume must not execute: {report:#?}"
    );
}

#[test]
fn test_analyze_outer_return_subject_excludes_nested_returns() {
    let files = [source(
        "main.js",
        "function outer() { function inner() { return 101; } return 202; }\nconsume(outer());",
    )];
    let report = inspect(&files, "main.js", "outer()", Subject::Return);
    let anchors: Vec<_> = report
        .nodes
        .iter()
        .filter(|node| report.anchor.contains(&node.id))
        .collect();
    assert!(!anchors.is_empty());
    assert!(
        anchors.iter().all(|node| !node.label.contains("101")),
        "nested returns do not belong to outer: {anchors:#?}"
    );
}

#[test]
fn test_analyze_branch_alias_writes_preserve_both_possible_values() {
    let files = [source("main.js", "let value = 101;\nlet object = { x: 0 };\nlet alias = object;\nif (flag) { alias.x = value; } else { object.x = 202; }\nconsume(object.x);")];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        argument(&report, "object.x"),
        "true branch write reaches read: {report:#?}"
    );
}

#[test]
fn test_analyze_imported_arrow_is_independent_of_snapshot_order() {
    let main = source(
        "main.js",
        "import { identity } from './lib.js';\nconsume(identity(101));",
    );
    let library = source("lib.js", "export const identity = (value) => value;");
    for files in [[main.clone(), library.clone()], [library, main]] {
        let report = inspect(&files, "main.js", "101", Subject::Value);
        assert!(
            argument(&report, "identity(101)"),
            "snapshot order must not hide exported arrow: {report:#?}"
        );
    }
}

#[test]
fn test_analyze_reexport_alias_reaches_original_return() {
    let files = [
        source(
            "main.js",
            "import { identity } from './barrel.js';\nconsume(identity(101));",
        ),
        source(
            "barrel.js",
            "export { original as identity } from './lib.js';",
        ),
        source(
            "lib.js",
            "export function original(value) { return value; }",
        ),
    ];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        argument(&report, "identity(101)"),
        "reexport must preserve binding: {report:#?}"
    );
}

#[test]
fn test_analyze_js_closure_reads_latest_captured_binding() {
    let files = [source(
        "main.js",
        "let value = 101;\nlet read = () => value;\nvalue = 202;\nconsume(read());",
    )];
    let old = inspect(&files, "main.js", "101", Subject::Value);
    let latest = inspect(&files, "main.js", "202", Subject::Value);
    assert!(
        !argument(&old, "read()"),
        "reassigned value is dead: {old:#?}"
    );
    assert!(
        argument(&latest, "read()"),
        "closure reads latest binding: {latest:#?}"
    );
}

#[test]
fn test_analyze_php_value_capture_keeps_creation_value() {
    let files = [source("main.php", "<?php\n$value = 101;\n$read = function() use ($value) { return $value; };\n$value = 202;\nconsume($read());")];
    let captured = inspect(&files, "main.php", "101", Subject::Value);
    let latest = inspect(&files, "main.php", "202", Subject::Value);
    assert!(
        argument(&captured, "$read()"),
        "PHP use captures by value: {captured:#?}"
    );
    assert!(
        !argument(&latest, "$read()"),
        "later assignment must not replace value capture: {latest:#?}"
    );
}

#[test]
fn test_analyze_python_default_uses_definition_time_value() {
    let files = [source("main.py", "value = 101\ndef read(parameter=value):\n    return parameter\nvalue = 202\nconsume(read())\n")];
    let captured = inspect(&files, "main.py", "101", Subject::Value);
    let latest = inspect(&files, "main.py", "202", Subject::Value);
    assert!(
        argument(&captured, "read()"),
        "Python default binds at definition: {captured:#?}"
    );
    assert!(
        !argument(&latest, "read()"),
        "later value does not alter default: {latest:#?}"
    );
}

#[test]
fn test_analyze_python_keyword_arguments_preserve_parameter_identity() {
    let files = [source(
        "main.py",
        "def first(left, right):\n    return left\nconsume(first(right=101, left=202))\n",
    )];
    let right = inspect(&files, "main.py", "101", Subject::Value);
    let left = inspect(&files, "main.py", "202", Subject::Value);
    assert!(
        !argument(&right, "first(right=101, left=202)"),
        "keyword right is not first positional parameter: {right:#?}"
    );
    assert!(
        argument(&left, "first(right=101, left=202)"),
        "keyword left reaches return: {left:#?}"
    );
}

#[test]
fn test_analyze_loop_carried_dependency_reaches_later_iteration() {
    let files = [source("main.js", "let value = 101;\nlet first = 0;\nlet second = 0;\nwhile (flag) { second = first; first = value; }\nconsume(second);")];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        argument(&report, "second"),
        "dependency needs at least two loop iterations: {report:#?}"
    );
}

#[test]
fn test_analyze_recursive_argument_return_cycle_reaches_consumer() {
    let files = [source("main.js", "function recur(value, flag) { if (flag) { return value; } return recur(value, true); }\nconsume(recur(101, flag));")];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        argument(&report, "recur(101, flag)"),
        "recursive return retains dependency: {report:#?}"
    );
}

#[test]
fn test_analyze_control_consumer_uses_control_edge() {
    let files = [source(
        "main.js",
        "let flag = 101;\nif (flag) { consume(202); }",
    )];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        argument(&report, "202"),
        "condition affects consumer execution: {report:#?}"
    );
    assert!(report.edges.iter().any(|edge| edge.kind == "control"));
}

#[test]
fn test_analyze_short_circuit_condition_controls_rhs_consumer() {
    let files = [source("main.js", "let flag = 101;\nflag && consume(202);")];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        argument(&report, "202"),
        "short-circuit condition controls execution: {report:#?}"
    );
    assert!(report.edges.iter().any(|edge| edge.kind == "control"));
}

#[test]
fn test_analyze_php_reference_capture_reads_latest_value() {
    let files = [source("main.php", "<?php\n$value = 101;\n$read = function() use (&$value) { return $value; };\n$value = 202;\nconsume($read());")];
    let old = inspect(&files, "main.php", "101", Subject::Value);
    let latest = inspect(&files, "main.php", "202", Subject::Value);
    assert!(
        !argument(&old, "$read()"),
        "reference capture sees reassignment: {old:#?}"
    );
    assert!(
        argument(&latest, "$read()"),
        "reference capture sees latest value: {latest:#?}"
    );
}

#[test]
fn test_analyze_source_position_after_line_end_rejects_nearest_value() {
    let files = [source("main.js", "let value = 101; consume(value);")];
    let result = analyze(
        &files,
        &FlowRequest {
            file: "main.js".into(),
            line: 1,
            column: 500,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    );
    assert!(
        result.is_err(),
        "an invalid position must not silently choose a nearby value"
    );
}

#[test]
fn test_analyze_small_node_budget_reports_truncation() {
    let files = [source(
        "main.js",
        "let value = 101;\nconsume(value);\nconsume(value + 1);\nconsume(value + 2);",
    )];
    let report = analyze(
        &files,
        &FlowRequest {
            file: "main.js".into(),
            line: 1,
            column: 13,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets {
                max_nodes: 5,
                ..Budgets::default()
            },
        },
    )
    .unwrap();
    assert!(
        report.truncated,
        "exhausted graph budget must be explicit: {report:#?}"
    );
    assert!(report.nodes.len() <= 5);
}

#[test]
fn test_analyze_budget_lost_anchor_explains_truncation() {
    let files = [source(
        "main.js",
        "let first = 101;\nlet second = 202;\nconsume(second);",
    )];
    let error = analyze(
        &files,
        &FlowRequest {
            file: "main.js".into(),
            line: 2,
            column: 14,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets {
                max_nodes: 1,
                ..Budgets::default()
            },
        },
    )
    .unwrap_err()
    .to_lowercase();
    assert!(
        error.contains("budget") || error.contains("truncat"),
        "budget loss differs from absent value: {error}"
    );
}

#[test]
fn test_analyze_malformed_suffix_preserves_parse_boundary() {
    let files = [source(
        "main.js",
        "let value = 101;\nconsume(value);\nfunction broken(",
    )];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        report
            .boundaries
            .iter()
            .any(|boundary| boundary.kind == "parse_error"),
        "malformed source must carry a boundary: {report:#?}"
    );
}

#[test]
fn test_analyze_deep_ast_is_bounded_without_stack_overflow() {
    let text = format!(
        "let value = 101;\nconsume(value);\n{}0{};",
        "(".repeat(400),
        ")".repeat(400)
    );
    let files = [source("main.js", &text)];
    let report = inspect(&files, "main.js", "101", Subject::Value);
    assert!(
        report.truncated,
        "depth budget must stop lowering: {report:#?}"
    );
    assert!(report
        .boundaries
        .iter()
        .any(|boundary| boundary.kind == "ast_budget"));
}

#[test]
fn test_analyze_alias_overwrite_kills_previous_field_value() {
    let files = [source(
        "main.js",
        "let object = { x: 101 };\nlet alias = object;\nalias.x = 202;\nconsume(object.x);",
    )];
    let old = inspect(&files, "main.js", "101", Subject::Value);
    let latest = inspect(&files, "main.js", "202", Subject::Value);
    assert!(
        !argument(&old, "object.x"),
        "overwritten field value must not reach read: {old:#?}"
    );
    assert!(
        argument(&latest, "object.x"),
        "replacement field value must reach read: {latest:#?}"
    );
}

#[test]
fn test_analyze_backward_second_call_excludes_first_argument() {
    let files = [source("main.js", "function identity(value) { return value; }\nconst first = identity(101);\nconst second = identity(202);\nconsume(second);")];
    let report = analyze(
        &files,
        &FlowRequest {
            file: "main.js".into(),
            line: 3,
            column: 7,
            subject: Subject::Binding,
            direction: Direction::Backward,
            budgets: Budgets::default(),
        },
    )
    .unwrap();
    assert!(
        report.nodes.iter().any(|node| node.label == "202"),
        "second argument supplies second result: {report:#?}"
    );
    assert!(
        !report.nodes.iter().any(|node| node.label == "101"),
        "call contexts must remain separate: {report:#?}"
    );
}
