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
    let report = analyze_changes(
        &files,
        &BTreeMap::from([("test.js".into(), vec![1])]),
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
fn query_in(path: &str, source: &str, line: usize, column: usize) -> FlowReport {
    analyze(
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
    .unwrap()
}
fn consumes(report: &FlowReport, label: &str) -> bool {
    report
        .consumers
        .iter()
        .any(|id| report.nodes.iter().any(|n| n.id == *id && n.label == label))
}
// Contract for the five tests below: a value that is consumed on at least one
// execution path stays a consumer in the path-insensitive report. An empty
// consumer list must not be produced by a path the analysis silently dropped.
#[test]
fn test_analyze_short_circuit_keeps_skipped_assignment_path() {
    let report = query(
        "let value = 101;\nlet flag = false;\nflag && (value = 202);\nconsume(value);\n",
        1,
        13,
    );
    assert!(consumes(&report, "value"), "{report:#?}");
}
#[test]
fn test_analyze_python_conditional_expression_reaches_consumer() {
    let report = query_in(
        "test.py",
        "value = 101\nresult = value if flag else 0\nconsume(result)\n",
        1,
        9,
    );
    assert!(consumes(&report, "result"), "{report:#?}");
}
#[test]
fn test_analyze_python_elif_branch_reaches_consumer() {
    let report = query_in(
        "test.py",
        "value = 101\nresult = 0\nif a:\n    result = 0\nelif b:\n    result = value\nelse:\n    result = 0\nconsume(result)\n",
        1,
        9,
    );
    assert!(consumes(&report, "result"), "{report:#?}");
}
#[test]
fn test_analyze_multiple_callees_keep_each_side_effect() {
    let report = query(
        "let result = 0;\nfunction a() { result = 101; }\nfunction b() { result = 202; }\nlet selected = flag ? a : b;\nselected();\nconsume(result);\n",
        2,
        25,
    );
    assert!(consumes(&report, "result"), "{report:#?}");
}
#[test]
fn test_analyze_uncalled_function_does_not_kill_sibling_read() {
    let report = query(
        "let value = 101;\nfunction reset() { value = 202; }\nfunction read() { consume(value); }\n",
        1,
        13,
    );
    assert!(consumes(&report, "value"), "{report:#?}");
}
#[test]
fn test_analyze_loop_with_object_literal_converges() {
    let report = query(
        "const items = [1];\nconst out = [];\nfor (const item of items) { out.push({ id: item.id }); }\nlet f = 0;\nwhile (f < 2) { const g = () => f; f = f + 1; }\nconsume(out);\n",
        2,
        13,
    );
    assert!(!report.truncated, "{report:#?}");
    assert!(
        !report.boundaries.iter().any(|b| b.kind == "loop_budget"),
        "{report:#?}"
    );
}
#[test]
fn test_analyze_boundaries_scoped_to_reached_files() {
    let files = [
        SourceFile {
            path: "a.js".into(),
            source: "let x = 1;\nconsume(x);\n".into(),
        },
        SourceFile {
            path: "b.js".into(),
            source: "other();\n".into(),
        },
    ];
    let report = analyze(
        &files,
        &FlowRequest {
            file: "a.js".into(),
            line: 1,
            column: 9,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap();
    assert!(
        report.boundaries.iter().all(|b| b.file == "a.js"),
        "{report:#?}"
    );
    assert_eq!(report.boundaries_omitted, 1, "{report:#?}");
}
// Second round (codex cross-family review of the fix series).
#[test]
fn test_analyze_condition_side_effect_survives_into_fallthrough() {
    let report = query(
        "let value = 0;\nfunction check() { value = 101; return flag; }\nfunction run() {\n  if (check()) return;\n  consume(value);\n}\nrun();\n",
        2,
        29,
    );
    assert!(consumes(&report, "value"), "{report:#?}");
}
#[test]
fn test_analyze_loop_closure_identity_stays_per_call() {
    let report = query(
        "function make(v) {\n  let f;\n  while (flag) { f = () => v; }\n  return f;\n}\nconst first = make(101);\nconst second = make(202);\nconsume(second());\n",
        7,
        21,
    );
    assert!(consumes(&report, "second()"), "{report:#?}");
}
#[test]
fn test_analyze_loop_allocation_reuse_keeps_previous_iteration_field() {
    let report = query(
        "let prev;\nlet flag = true;\nwhile (flag) {\n  let next = { x: 0 };\n  if (prev) consume(prev.x);\n  next.x = 101;\n  prev = next;\n}\n",
        6,
        12,
    );
    assert!(consumes(&report, "prev.x"), "{report:#?}");
}
#[test]
fn test_analyze_uncalled_nested_sibling_keeps_enclosing_value() {
    let report = query(
        "function outer() {\n  let value = 101;\n  function reset() { value = 202; }\n  function read() { consume(value); }\n}\n",
        2,
        15,
    );
    assert!(consumes(&report, "value"), "{report:#?}");
}
#[test]
fn test_analyze_python_elif_return_controls_following_statement() {
    // The value reaching `b` decides whether consume(7) runs: its condition
    // node must carry a control edge to the argument after the branch.
    let report = query_in(
        "test.py",
        "b = 101\ndef f(a):\n    if a:\n        x = 0\n    elif b:\n        return 0\n    consume(7)\n",
        1,
        5,
    );
    assert!(consumes(&report, "7"), "{report:#?}");
}
#[test]
fn test_analyze_boundaries_keep_direct_module_neighbours() {
    let files = [
        SourceFile {
            path: "a.js".into(),
            source: "export const x = { secret: 101 };\n".into(),
        },
        SourceFile {
            path: "b.js".into(),
            source: "import { x } from './a.js';\nconst y = { ...x };\nconsume(y);\n".into(),
        },
        SourceFile {
            path: "c.js".into(),
            source: "other();\n".into(),
        },
    ];
    let report = analyze(
        &files,
        &FlowRequest {
            file: "a.js".into(),
            line: 1,
            column: 28,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap();
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.file == "b.js" && b.kind == "object_member"),
        "{report:#?}"
    );
    assert!(
        report.boundaries.iter().all(|b| b.file != "c.js"),
        "{report:#?}"
    );
}
#[test]
fn test_analyze_loop_calling_helper_that_allocates_converges() {
    let report = query(
        "const xs = [1];\nlet acc = 0;\nfunction helper(i) { return { v: i }; }\nfor (const i of xs) { acc = helper(i); }\nconsume(acc);\n",
        1,
        13,
    );
    assert!(!report.truncated, "{report:#?}");
    assert!(
        !report.boundaries.iter().any(|b| b.kind == "loop_budget"),
        "{report:#?}"
    );
    assert!(consumes(&report, "acc"), "{report:#?}");
}
#[test]
fn test_analyze_two_call_sites_in_loop_stay_distinct() {
    let report = query(
        "const xs = [1];\nfunction id(v) { return v; }\nlet a = 0;\nlet b = 0;\nfor (const i of xs) { a = id(101); b = id(202); }\nconsume(a);\n",
        5,
        30,
    );
    assert!(consumes(&report, "a"), "{report:#?}");
    assert!(!consumes(&report, "b"), "{report:#?}");
}
