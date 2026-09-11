use super::*;

fn query(source: &str, line: usize, column: usize) -> FlowReport {
    analyze(
        &[SourceFile {
            path: "test.js".into(),
            source: source.into(),
        }],
        &FlowRequest {
            file: "test.js".into(),
            line,
            column,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap()
}
fn has(report: &FlowReport, label: &str) -> bool {
    report.nodes.iter().any(|n| n.label == label)
}
#[test]
fn test_analyze_closure_chain_reaches_consumers() {
    let report = query(
        "var step = function() { return 10; };\nvar perView = function() { return 100 / step(); };\nvar pages = function() { return 50 / perView(); };\nvar buildDots = function() { consume(pages()); };\nbuildDots();",
        1,
        32,
    );
    assert!(has(&report, "pages()"), "{report:#?}");
    assert!(has(&report, "perView()"));
    assert!(report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "pages()"));
}
#[test]
fn test_analyze_discarded_return_does_not_taint_caller_return() {
    let report = query(
        "function step() { return 10; }\nfunction perView() { step(); return 3; }\nconsume(perView());",
        1,
        26,
    );
    assert!(!report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "perView()"));
}
#[test]
fn test_analyze_reassignment_stops_old_value() {
    let report = query("let x = 10;\nx = 20;\nconsume(x);", 1, 9);
    assert!(!has(&report, "x") || !report.nodes.iter().any(|n| n.kind == "argument"));
}
#[test]
fn test_analyze_distinct_calls_keep_arguments_separate() {
    let report = query(
        "function identity(x) { return x; }\nlet a = identity(10);\nlet b = identity(20);\nconsume(a);\nconsume(b);",
        2,
        18,
    );
    assert!(report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "a"));
    assert!(!report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "b"));
}
#[test]
fn test_analyze_named_import_propagates_across_files() {
    let files = vec![
        SourceFile {
            path: "main.js".into(),
            source: "import { identity as id } from './lib.js';\nconsume(id(10));".into(),
        },
        SourceFile {
            path: "lib.js".into(),
            source: "export function identity(x) { return x; }".into(),
        },
    ];
    let report = analyze(
        &files,
        &FlowRequest {
            file: "main.js".into(),
            line: 2,
            column: 12,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap();
    assert!(
        report
            .nodes
            .iter()
            .any(|n| n.file == "lib.js" && n.kind == "return"),
        "{report:#?}"
    );
    assert!(report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "id(10)"));
}
#[test]
fn test_analyze_unknown_call_reports_boundary() {
    let report = query("let x = 10;\nconsume(dynamic(x));", 1, 9);
    assert!(report.boundaries.iter().any(|b| b.kind == "external_call"));
    assert!(!report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "dynamic(x)"));
}
#[test]
fn test_analyze_python_and_php_returns_propagate() {
    for (path, source, line, column) in [
        (
            "test.py",
            "def identity(x):\n    return x\na = identity(10)\nconsume(a)\n",
            3,
            14,
        ),
        (
            "test.php",
            "<?php\nfunction identity($x) { return $x; }\n$a = identity(10);\nconsume($a);",
            3,
            15,
        ),
    ] {
        let report = analyze(
            &[SourceFile {
                path: path.into(),
                source: source.into(),
            }],
            &FlowRequest {
                file: path.into(),
                line,
                column,
                subject: Subject::Value,
                direction: Direction::Forward,
                budgets: Budgets::default(),
            },
        )
        .unwrap();
        assert!(
            report.nodes.iter().any(|n| n.kind == "return"),
            "{path}: {report:#?}"
        );
        assert!(
            report
                .nodes
                .iter()
                .any(|n| n.kind == "argument" && (n.label == "a" || n.label == "$a")),
            "{path}: {report:#?}"
        );
    }
}
#[test]
fn test_analyze_alias_field_write_reaches_read() {
    let report = query(
        "let obj = {x: 1};\nlet alias = obj;\nalias.x = 10;\nconsume(obj.x);",
        3,
        11,
    );
    assert!(
        report
            .nodes
            .iter()
            .any(|n| n.kind == "argument" && n.label == "obj.x"),
        "{report:#?}"
    );
}
#[test]
fn test_analyze_branch_has_control_edges() {
    let report = query("let flag = true;\nif (flag) { consume(10); }", 1, 12);
    assert!(report.edges.iter().any(|e| e.kind == "control"));
}

#[test]
fn test_analyze_block_shadowing_keeps_outer_binding() {
    let report = query(
        "let x = 10;\n{ let x = 20; consume(x); }\nconsume(x);",
        1,
        9,
    );
    assert!(!report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.line == 2));
    assert!(report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.line == 3));
}

#[test]
fn test_analyze_recursive_return_reaches_fixed_point() {
    let report = query(
        "function count(n) { if (n) { return count(n-1); } return n; }\nconsume(count(10));",
        2,
        15,
    );
    assert!(report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "count(10)"));
    assert!(!report.truncated);
}

#[test]
fn test_analyze_loop_reaches_value_fixed_point() {
    let report = query(
        "let x = 10;\nwhile (flag) { x = x + 1; }\nconsume(x);",
        1,
        9,
    );
    assert!(report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.line == 3));
    assert!(!report.truncated, "{report:#?}");
}

#[test]
fn test_analyze_return_subject_excludes_nested_function() {
    let files=[SourceFile{path:"test.js".into(),source:"const outer = function() { function inner() { return 9; } return 2; };\nconsume(outer());".into()}];
    let report = analyze(
        &files,
        &FlowRequest {
            file: "test.js".into(),
            line: 1,
            column: 8,
            subject: Subject::Return,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap();
    assert!(report
        .nodes
        .iter()
        .any(|n| n.kind == "return" && n.label == "return 2;"));
    assert!(!report
        .nodes
        .iter()
        .any(|n| n.kind == "return" && n.label == "return 9;"));
}

#[test]
fn test_analyze_invalid_column_returns_error() {
    let files = [SourceFile {
        path: "test.js".into(),
        source: "let x = 10;".into(),
    }];
    assert!(analyze(
        &files,
        &FlowRequest {
            file: "test.js".into(),
            line: 1,
            column: 100,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default()
        }
    )
    .is_err());
}

#[test]
fn test_analyze_ast_budget_reports_truncation() {
    let files = [SourceFile {
        path: "test.js".into(),
        source: "let x = 10; consume(x);".into(),
    }];
    let report = analyze_lines(
        &files,
        "test.js",
        &[1],
        &Budgets {
            max_steps: 3,
            ..Budgets::default()
        },
    )
    .unwrap();
    assert!(report.truncated);
    assert!(report.boundaries.iter().any(|b| b.kind == "ast_budget"));
}

#[test]
fn test_analyze_carousel_numeric_builtins_reach_pages_and_scroll() {
    let source = "list.forEach(function(el) {\nvar step = function() { return el.clientWidth + gap; };\nvar perView = function() { return Math.max(1, Math.floor(el.clientWidth / step())); };\nvar pages = function() { return Math.ceil(count / perView()); };\nvar buildDots = function() { consume(pages()); };\nbuildDots();\nel.scrollLeft = step() * 2;\n});";
    let files = [SourceFile {
        path: "test.js".into(),
        source: source.into(),
    }];
    let report = analyze(
        &files,
        &FlowRequest {
            file: "test.js".into(),
            line: 2,
            column: 5,
            subject: Subject::Return,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap();
    assert!(
        report
            .nodes
            .iter()
            .any(|n| n.kind == "argument" && n.label == "pages()"),
        "{report:#?}"
    );
    assert!(report
        .nodes
        .iter()
        .any(|n| n.kind == "field_write" && n.label == "el.scrollLeft"));
    assert!(report
        .boundaries
        .iter()
        .any(|b| b.kind == "callback_escape"));
}

#[test]
fn test_analyze_shadowed_math_does_not_apply_builtin_summary() {
    let report = query(
        "let Math = { floor: function(x) { return 0; } };\nconsume(Math.floor(101));",
        2,
        20,
    );
    assert!(!report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "Math.floor(101)"));
}

#[test]
fn test_analyze_partial_return_excludes_terminated_assignment() {
    let report = query(
        "let value = 101;\nfunction choose(flag) { let x = 0; if (flag) { x = value; return 1; } return x; }\nconsume(choose(flag));",
        1,
        13,
    );
    assert!(
        !report
            .nodes
            .iter()
            .any(|n| n.kind == "argument" && n.label == "choose(flag)"),
        "{report:#?}"
    );
}

#[test]
fn test_analyze_partial_return_preserves_captured_write() {
    let report = query(
        "let value = 101; let output = 0;\nfunction choose(flag) { if (flag) { output = value; return 1; } return 0; }\nchoose(flag); consume(output);",
        1,
        13,
    );
    assert!(
        report
            .nodes
            .iter()
            .any(|n| n.kind == "argument" && n.label == "output"),
        "{report:#?}"
    );
}

#[test]
fn test_analyze_default_parameters_use_argument_when_supplied() {
    let report = query(
        "function identity(x = 101) { return x; }\nconsume(identity());\nconsume(identity(202));",
        1,
        23,
    );
    assert!(
        report
            .nodes
            .iter()
            .any(|n| n.kind == "argument" && n.label == "identity()"),
        "{report:#?}"
    );
    assert!(!report
        .nodes
        .iter()
        .any(|n| n.kind == "argument" && n.label == "identity(202)"));
}

#[test]
fn test_analyze_long_identifiers_preserve_binding_identity() {
    let prefix = "a".repeat(256);
    let source = format!("const {prefix}x = 101;\nconst {prefix}y = 202;\nconsume({prefix}x);");
    let report = query(&source, 1, source.find("101").unwrap() + 1);
    assert!(report
        .nodes
        .iter()
        .any(|node| node.kind == "argument" && node.line == 3));
    assert!(report
        .nodes
        .iter()
        .all(|node| node.label.chars().count() <= 160));
}

#[test]
fn test_analyze_long_import_path_resolves_complete_source() {
    let path = format!("{}lib.js", "segment/".repeat(40));
    let sources = [
        SourceFile {
            path: "main.js".into(),
            source: format!("import {{ identity }} from './{path}';\nconsume(identity(101));"),
        },
        SourceFile {
            path,
            source: "export function identity(value) { return value; }".into(),
        },
    ];
    let report = analyze(
        &sources,
        &FlowRequest {
            file: "main.js".into(),
            line: 2,
            column: 18,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap();
    assert!(report
        .nodes
        .iter()
        .any(|node| node.kind == "argument" && node.label == "identity(101)"));
}

#[test]
fn test_analyze_long_property_keys_preserve_field_identity() {
    let prefix = "k".repeat(256);
    let source = format!(
        "const object = {{ \"{prefix}x\": 101, \"{prefix}y\": 202 }};\nconsume(object[\"{prefix}x\"]);"
    );
    let report = query(&source, 1, source.find("101").unwrap() + 1);
    assert!(report
        .nodes
        .iter()
        .any(|node| node.kind == "argument" && node.line == 2));
}
