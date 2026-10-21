use ecp_analyzer::flow::{
    analyze, Budgets, Direction, FlowReport, FlowRequest, SourceFile, Subject,
};

fn forward(text: &str, marker: &str) -> FlowReport {
    let files = [SourceFile {
        path: "main.py".into(),
        source: text.into(),
    }];
    let offset = text.find(marker).expect("unique source marker");
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap().len() + 1;
    analyze(
        &files,
        &FlowRequest {
            file: "main.py".into(),
            line,
            column,
            subject: Subject::Value,
            direction: Direction::Forward,
            budgets: Budgets::default(),
        },
    )
    .unwrap()
}

fn binding(report: &FlowReport, label: &str) -> bool {
    report
        .nodes
        .iter()
        .any(|node| node.kind == "binding" && node.label == label)
}

fn argument(report: &FlowReport, label: &str) -> bool {
    report
        .nodes
        .iter()
        .any(|node| node.kind == "argument" && node.label == label)
}

const BOX: &str = "class Box:\n    def get(self, value):\n        return value\n\n";

#[test]
fn test_analyze_python_module_call_does_not_reach_method_name() {
    let report = forward(&format!("{BOX}consume(get(101))\n"), "101");
    assert!(
        report
            .nodes
            .iter()
            .all(|node| !(2..=3).contains(&node.line)),
        "a method is not a module-level name: {report:#?}"
    );
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.kind == "external_call" && b.line == 5),
        "{:?}",
        report.boundaries
    );
}

#[test]
fn test_analyze_python_method_does_not_see_class_scope() {
    let report = forward(
        &format!("{BOX}    def run(self):\n        consume(get(404))\n"),
        "404",
    );
    assert!(
        !binding(&report, "value"),
        "class-body names are invisible inside methods: {report:#?}"
    );
}

#[test]
fn test_analyze_python_self_call_reaches_method() {
    let report = forward(
        &format!("{BOX}    def run(self):\n        seed = 101\n        consume(self.get(seed))\n"),
        "101",
    );
    assert!(binding(&report, "value"), "{report:#?}");
    assert!(argument(&report, "self.get(seed)"), "{report:#?}");
}

#[test]
fn test_analyze_python_inherited_self_call_reaches_base_method() {
    let report = forward(
        &format!("{BOX}class Child(Box):\n    def run(self):\n        consume(self.get(202))\n"),
        "202",
    );
    assert!(binding(&report, "value"), "{report:#?}");
    assert!(argument(&report, "self.get(202)"), "{report:#?}");
}

#[test]
fn test_analyze_python_self_call_includes_subclass_override() {
    let report = forward(
        "class Base:\n    def run(self):\n        return self.hook(101)\n\n    def hook(self, in_base):\n        return in_base\n\nclass Child(Base):\n    def hook(self, in_child):\n        return in_child\n",
        "101",
    );
    assert!(binding(&report, "in_base"), "{report:#?}");
    assert!(
        binding(&report, "in_child"),
        "self dispatch may reach the override: {report:#?}"
    );
}

#[test]
fn test_analyze_python_class_attribute_call_reaches_method() {
    let report = forward(&format!("{BOX}consume(Box.get(None, 303))\n"), "303");
    assert!(binding(&report, "value"), "{report:#?}");
    assert!(argument(&report, "Box.get(None, 303)"), "{report:#?}");
}
