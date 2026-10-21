//! Declarations and their syntax across every language the flow engine parses
//! (JavaScript, TypeScript, Python, PHP; other extensions are rejected).

use ecp_analyzer::flow::{
    analyze, Budgets, Direction, FlowReport, FlowRequest, SourceFile, Subject,
};

fn source(path: &str, text: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        source: text.into(),
    }
}

fn forward(files: &[SourceFile], file: &str, marker: &str, budgets: Budgets) -> FlowReport {
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
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets,
        },
    )
    .unwrap_or_else(|error| panic!("{file}: {error}"))
}

fn unsupported(report: &FlowReport) -> Vec<&str> {
    report
        .boundaries
        .iter()
        .filter(|b| b.kind == "unsupported_semantics")
        .filter_map(|b| b.message.split(' ').next())
        .collect()
}

#[test]
fn test_analyze_method_value_slice_excludes_declaration_nodes() {
    for (path, text) in [
        (
            "main.js",
            "class Box {\n  read(flag) {\n    const v = 101;\n    return v;\n  }\n}\n",
        ),
        (
            "main.ts",
            "class Box {\n  public read(flag: boolean): number {\n    const v = 101;\n    return v;\n  }\n}\n",
        ),
        (
            "main.py",
            "class Box:\n    def read(self, flag):\n        v = 101\n        return v\n",
        ),
        (
            "main.php",
            "<?php\nclass Box\n{\n    public function read(bool $flag): int\n    {\n        $v = 101;\n        return $v;\n    }\n}\n",
        ),
    ] {
        let files = [source(path, text)];
        let report = forward(&files, path, "101", Budgets::default());
        assert!(
            report.nodes.iter().any(|n| n.kind == "return"),
            "{path}: the return consumer must be reached: {report:#?}"
        );
        let declarations: Vec<_> = report
            .nodes
            .iter()
            .filter(|n| {
                n.kind == "value" && (n.label.starts_with("class ") || n.label.contains("read("))
            })
            .collect();
        assert!(
            declarations.is_empty(),
            "{path}: a declaration is not a value: {declarations:#?}"
        );
    }
}

#[test]
fn test_analyze_structural_nodes_raise_no_boundary_but_real_gaps_do() {
    for (path, text, real, structural) in [
        (
            "main.js",
            "class Box extends Base {\n  read(a, b = 1) {\n    const v = new Thing(101);\n    return v;\n  }\n}\n",
            "new_expression",
            &[
                "class_declaration",
                "class_heritage",
                "class_body",
                "formal_parameters",
                "property_identifier",
                "assignment_pattern",
            ][..],
        ),
        (
            "main.ts",
            "class Box extends Base implements Shape {\n  public read(a: number, b: string = 'x'): number {\n    const v = new Thing(101);\n    return v;\n  }\n}\n",
            "new_expression",
            &[
                "class_declaration",
                "class_heritage",
                "extends_clause",
                "implements_clause",
                "class_body",
                "type_identifier",
                "accessibility_modifier",
                "formal_parameters",
                "required_parameter",
                "optional_parameter",
                "type_annotation",
                "predefined_type",
                "property_identifier",
            ][..],
        ),
        (
            "main.py",
            "class Box(Base):\n    def read(self, a: int = 1):\n        v = [x for x in range(101)]\n        return v\n",
            "list_comprehension",
            &["class_definition", "argument_list", "parameters", "typed_default_parameter"][..],
        ),
        (
            "main.php",
            "<?php\nnamespace App;\nuse Vendor\\Base;\nclass Box extends Base implements Shape\n{\n    public function read(int $a, ?string $b = null): int\n    {\n        $v = new Thing(101);\n        return $v;\n    }\n}\n",
            "object_creation_expression",
            &[
                "namespace_definition",
                "namespace_name",
                "class_declaration",
                "base_clause",
                "class_interface_clause",
                "declaration_list",
                "method_declaration",
                "visibility_modifier",
                "formal_parameters",
                "simple_parameter",
                "named_type",
                "optional_type",
                "primitive_type",
                "arguments",
            ][..],
        ),
    ] {
        let files = [source(path, text)];
        let report = forward(&files, path, "101", Budgets::default());
        let kinds = unsupported(&report);
        assert!(
            kinds.iter().all(|kind| !structural.contains(kind)),
            "{path}: structural nodes must not raise boundaries: {kinds:?}"
        );
        assert!(
            kinds.contains(&real),
            "{path}: {real} keeps its boundary: {kinds:?}"
        );
    }
}

/// Contract: a JS method body still runs in place, unbound and uncalled, so
/// it keeps one boundary saying so rather than looking fully analysed.
#[test]
fn test_analyze_js_method_body_keeps_its_boundary() {
    let files = [source(
        "main.js",
        "class Box {\n  read() {\n    const v = 101;\n    return v;\n  }\n}\n",
    )];
    let report = forward(&files, "main.js", "101", Budgets::default());
    assert!(
        unsupported(&report).contains(&"method_definition"),
        "{:?}",
        report.boundaries
    );
}

#[test]
fn test_analyze_queried_file_entry_points_run_before_budget_spent_elsewhere() {
    for (heavy_path, heavy, main_path, main, consumer) in [
        (
            "heavy.js",
            format!("function heavy() {{\n{}}}\n", "  let a = 1;\n".repeat(100)),
            "main.js",
            "function main() {\n  let anchor = 101;\n  consume(anchor);\n}\n",
            "anchor",
        ),
        (
            "heavy.ts",
            format!("function heavy() {{\n{}}}\n", "  let a = 1;\n".repeat(100)),
            "main.ts",
            "function main(): void {\n  let anchor = 101;\n  consume(anchor);\n}\n",
            "anchor",
        ),
        (
            "heavy.py",
            format!("def heavy():\n{}", "    a = 1\n".repeat(100)),
            "main.py",
            "def main():\n    anchor = 101\n    consume(anchor)\n",
            "anchor",
        ),
        (
            "heavy.php",
            format!("<?php\nfunction heavy() {{\n{}}}\n", "  $a = 1;\n".repeat(100)),
            "main.php",
            "<?php\nclass Main\n{\n    public function run()\n    {\n        $anchor = 101;\n        consume($anchor);\n    }\n}\n",
            "$anchor",
        ),
    ] {
        // The heavy file comes first, so file order alone would spend the
        // node budget on it before the queried function ever runs.
        let files = [source(heavy_path, &heavy), source(main_path, main)];
        let report = forward(
            &files,
            main_path,
            "101",
            Budgets {
                max_nodes: 60,
                ..Budgets::default()
            },
        );
        assert!(
            report
                .nodes
                .iter()
                .any(|n| n.kind == "argument" && n.label == consumer),
            "{main_path}: {report:#?}"
        );
        assert!(report.truncated, "{heavy_path} must still be cut");
    }
}

#[test]
fn test_analyze_pyi_stub_uses_python_semantics() {
    let files = [source("main.pyi", "x = abs(101)\nconsume(x)\n")];
    let report = forward(&files, "main.pyi", "101", Budgets::default());
    assert!(
        report.boundaries.iter().any(|b| b.kind == "builtin_model"),
        "a .pyi file is Python: {:?}",
        report.boundaries
    );
    assert!(
        report
            .nodes
            .iter()
            .any(|n| n.kind == "argument" && n.label == "x"),
        "{report:#?}"
    );
}
