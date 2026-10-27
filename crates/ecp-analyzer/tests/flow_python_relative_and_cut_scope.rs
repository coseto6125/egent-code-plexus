//! Flow: a Python relative import keeps its level, and an AST-budget cut only
//! marks the answers whose slice reaches the cut file.

use ecp_analyzer::flow::{
    analyze, Budgets, Direction, FlowReport, FlowRequest, SourceFile, Subject,
};

fn file(path: &str, source: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        source: source.into(),
    }
}

fn forward(files: &[SourceFile], path: &str, line: usize, column: usize) -> FlowReport {
    analyze(
        files,
        &FlowRequest {
            file: path.into(),
            line,
            column,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .expect("flow analysis")
}

fn reaches(report: &FlowReport, path: &str) -> bool {
    report.nodes.iter().any(|n| n.file == path)
}

const IDENTITY: &str = "def f(x):\n    return x\n";
const OTHER: &str = "def f(x):\n    return 'DECOY'\n";

/// `importer` calls `f(101)` on line 2 after `from <module> import f`.
fn call_through(module: &str, real: &str, decoys: &[&str]) -> FlowReport {
    let importer = "pkg/a/sub/b.py";
    let mut files = vec![
        file(
            importer,
            &format!("from {module} import f\nresult = f(101)\n"),
        ),
        file(real, IDENTITY),
    ];
    files.extend(decoys.iter().map(|d| file(d, OTHER)));
    forward(&files, importer, 2, 12)
}

#[test]
fn test_python_two_dot_import_resolves_to_parent_package_not_sibling() {
    let report = call_through("..util", "pkg/a/util.py", &["pkg/a/sub/util.py"]);
    assert!(reaches(&report, "pkg/a/util.py"), "{report:#?}");
    assert!(!reaches(&report, "pkg/a/sub/util.py"), "{report:#?}");
}

#[test]
fn test_python_three_dot_import_climbs_two_directories() {
    let report = call_through(
        "...util",
        "pkg/util.py",
        &["pkg/a/util.py", "pkg/a/sub/util.py"],
    );
    assert!(reaches(&report, "pkg/util.py"), "{report:#?}");
    assert!(!reaches(&report, "pkg/a/util.py"), "{report:#?}");
    assert!(!reaches(&report, "pkg/a/sub/util.py"), "{report:#?}");
}

#[test]
fn test_python_one_dot_import_still_resolves_to_sibling() {
    let report = call_through(".util", "pkg/a/sub/util.py", &["pkg/a/util.py"]);
    assert!(reaches(&report, "pkg/a/sub/util.py"), "{report:#?}");
    assert!(!reaches(&report, "pkg/a/util.py"), "{report:#?}");
}

#[test]
fn test_python_dotted_relative_import_keeps_both_level_and_path() {
    let report = call_through(
        "..lib.util",
        "pkg/a/lib/util.py",
        &["pkg/a/sub/lib/util.py"],
    );
    assert!(reaches(&report, "pkg/a/lib/util.py"), "{report:#?}");
    assert!(!reaches(&report, "pkg/a/sub/lib/util.py"), "{report:#?}");
}

/// `importer` calls `f(101)` after `from <module> import f`; `files` are the
/// other files of the repo.
fn call_from(importer: &str, module: &str, others: Vec<SourceFile>) -> FlowReport {
    let mut files = vec![file(
        importer,
        &format!("from {module} import f\nresult = f(101)\n"),
    )];
    files.extend(others);
    forward(&files, importer, 2, 12)
}

#[test]
fn test_flow_python_double_dot_import_binds_parent_package() {
    let report = call_from(
        "pkg/a/sub/b.py",
        "..",
        vec![
            file("pkg/a/__init__.py", IDENTITY),
            file("pkg/a/sub/__init__.py", OTHER),
        ],
    );
    assert!(reaches(&report, "pkg/a/__init__.py"), "{report:#?}");
    assert!(!reaches(&report, "pkg/a/sub/__init__.py"), "{report:#?}");
}

#[test]
fn test_flow_python_single_dot_bare_import_binds_own_package() {
    let report = call_from(
        "pkg/a/sub/b.py",
        ".",
        vec![
            file("pkg/a/sub/__init__.py", IDENTITY),
            file("pkg/a/__init__.py", OTHER),
        ],
    );
    assert!(reaches(&report, "pkg/a/sub/__init__.py"), "{report:#?}");
    assert!(!reaches(&report, "pkg/a/__init__.py"), "{report:#?}");
}

#[test]
fn test_flow_python_level_above_repo_root_binds_nothing() {
    // `pkg/b.py` has one directory: `...` climbs past the repo root, so the
    // top-level `util.py` is not its target.
    let report = call_from("pkg/b.py", "...util", vec![file("util.py", OTHER)]);
    assert!(!reaches(&report, "util.py"), "{report:#?}");
    let report = call_from("b.py", "..util", vec![file("util.py", OTHER)]);
    assert!(!reaches(&report, "util.py"), "{report:#?}");
}

#[test]
fn test_flow_python_top_level_file_single_dot_binds_sibling() {
    let report = call_from("b.py", ".util", vec![file("util.py", IDENTITY)]);
    assert!(reaches(&report, "util.py"), "{report:#?}");
}

#[test]
fn test_flow_python_top_level_file_bare_dot_binds_root_package() {
    let report = call_from("b.py", ".", vec![file("__init__.py", IDENTITY)]);
    assert!(reaches(&report, "__init__.py"), "{report:#?}");
}

fn deep_cut_source() -> String {
    format!("let deep = {}0{};\n", "(".repeat(400), ")".repeat(400))
}

const SMALL: &str = "let value = 101;\nconsume(value);\n";

#[test]
fn test_cut_file_outside_slice_does_not_mark_answer_truncated() {
    let files = [
        file("main.js", SMALL),
        file("unrelated.js", &deep_cut_source()),
    ];
    let report = forward(&files, "main.js", 1, 13);
    assert!(!report.truncated, "{report:#?}");
}

#[test]
fn test_cut_file_outside_slice_keeps_its_boundary_in_output() {
    let files = [
        file("main.js", SMALL),
        file("unrelated.js", &deep_cut_source()),
    ];
    let report = forward(&files, "main.js", 1, 13);
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.kind == "ast_budget" && b.file == "unrelated.js"),
        "{report:#?}"
    );
    assert_eq!(report.boundaries_omitted, 0, "{report:#?}");
}

#[test]
fn test_cut_file_holding_the_slice_marks_answer_truncated() {
    let source = format!("{SMALL}{}", deep_cut_source());
    let report = forward(&[file("main.js", &source)], "main.js", 1, 13);
    assert!(report.truncated, "{report:#?}");
    assert!(report.boundaries.iter().any(|b| b.kind == "ast_budget"));
}

#[test]
fn test_cut_file_imported_by_slice_file_marks_answer_truncated() {
    let files = [
        file(
            "main.js",
            "import { deep } from './cut.js';\nlet value = 101;\nconsume(value);\n",
        ),
        file("cut.js", &deep_cut_source()),
    ];
    let report = forward(&files, "main.js", 2, 13);
    assert!(report.truncated, "{report:#?}");
}

#[test]
fn test_exhausted_node_budget_still_marks_answer_truncated() {
    let files = [file(
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
    assert!(report.truncated, "{report:#?}");
}
