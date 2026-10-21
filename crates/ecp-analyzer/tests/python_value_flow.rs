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

fn boundary_at(report: &FlowReport, kind: &str, line: usize) -> bool {
    report
        .boundaries
        .iter()
        .any(|b| b.kind == kind && b.line == line)
}

#[test]
fn test_analyze_python_staticmethod_first_parameter_is_not_self() {
    let report = forward(
        "class A:\n    @staticmethod\n    def make(cfg):\n        return cfg.build()\n\n    def build(self):\n        secret = 101\n        return secret\n",
        "101",
    );
    assert!(
        !report
            .nodes
            .iter()
            .any(|node| node.kind == "return" && node.label == "return cfg.build()"),
        "a static method's parameter is not the instance: {report:#?}"
    );
    assert!(
        boundary_at(&report, "external_call", 4),
        "{:?}",
        report.boundaries
    );
}

#[test]
fn test_analyze_python_self_call_to_staticmethod_passes_no_receiver() {
    let report = forward(
        "class A:\n    @staticmethod\n    def stat(y, z):\n        return z\n\n    def run(self):\n        return self.stat(101, 1)\n",
        "101",
    );
    assert!(binding(&report, "y"), "{report:#?}");
    assert!(
        !binding(&report, "z"),
        "the first argument binds the first parameter: {report:#?}"
    );
}

#[test]
fn test_analyze_python_classmethod_call_binds_class_to_cls() {
    let report = forward(
        "class Box:\n    @classmethod\n    def create(cls, size):\n        return size\n\nconsume(Box.create(101))\n",
        "101",
    );
    assert!(binding(&report, "size"), "{report:#?}");
    assert!(!binding(&report, "cls"), "{report:#?}");
    assert!(argument(&report, "Box.create(101)"), "{report:#?}");
}

/// `self.cm()` passes `type(self)`: `A.run(B)` runs `cm` with `cls` bound to
/// B, so `cls.value` reads B's value and never A's.
#[test]
fn test_analyze_python_self_call_to_classmethod_binds_receiver_class_to_cls() {
    let source = "class A:\n    value = 404\n\n    @classmethod\n    def cm(cls):\n        return cls.value\n\n    def run(self):\n        return self.cm()\n\nclass B(A):\n    value = 101\n\nconsume(A.run(B))\n";
    let report = forward(source, "101");
    assert!(argument(&report, "A.run(B)"), "{report:#?}");
    let report = forward(source, "404");
    assert!(
        !argument(&report, "A.run(B)"),
        "cls is B, whose value shadows A's: {report:#?}"
    );
}

#[test]
fn test_analyze_python_self_call_to_classmethod_unknown_receiver_keeps_external_call() {
    let report = forward(
        "class A:\n    @classmethod\n    def cm(cls, size):\n        return size\n\n    def run(self):\n        return self.cm(101)\n",
        "101",
    );
    assert!(
        !binding(&report, "size"),
        "the class of an unknown `self` is unknown: {report:#?}"
    );
    assert!(
        boundary_at(&report, "external_call", 7),
        "{:?}",
        report.boundaries
    );
}

#[test]
fn test_analyze_python_unknown_decorator_call_keeps_external_call() {
    let report = forward(
        "class Box:\n    @property\n    def get(self, value):\n        return value\n\nconsume(Box.get(None, 101))\n",
        "101",
    );
    assert!(
        !binding(&report, "value"),
        "a property is not called like its getter: {report:#?}"
    );
    assert!(
        boundary_at(&report, "external_call", 6),
        "{:?}",
        report.boundaries
    );
}

#[test]
fn test_analyze_python_inherited_class_attribute_read_reaches_consumer() {
    let report = forward(
        "class Base:\n    LIMIT = 101\n\nclass Sub(Base):\n    pass\n\nconsume(Sub.LIMIT)\n",
        "101",
    );
    assert!(argument(&report, "Sub.LIMIT"), "{report:#?}");
}

#[test]
fn test_analyze_python_multiple_inheritance_follows_mro_first_base() {
    let report = forward(
        "class A:\n    def m(self, in_a):\n        return in_a\n\nclass B:\n    def m(self, in_b):\n        return in_b\n\nclass C(A, B):\n    def run(self):\n        return self.m(101)\n",
        "101",
    );
    assert!(binding(&report, "in_a"), "{report:#?}");
    assert!(
        !binding(&report, "in_b"),
        "the MRO picks A.m, not both: {report:#?}"
    );
}

#[test]
fn test_analyze_python_diamond_inheritance_follows_c3_order() {
    let report = forward(
        "class Base:\n    def m(self, in_base):\n        return in_base\n\nclass Left(Base):\n    pass\n\nclass Right(Base):\n    def m(self, in_right):\n        return in_right\n\nclass D(Left, Right):\n    def run(self):\n        return self.m(101)\n",
        "101",
    );
    assert!(
        binding(&report, "in_right"),
        "C3 puts Right before Base: {report:#?}"
    );
    assert!(!binding(&report, "in_base"), "{report:#?}");
}

/// 25 stacked diamonds reach `O` along 2^25 paths; the resolution order
/// must still come out in time. The test's runtime is the guard.
#[test]
fn test_analyze_python_stacked_diamonds_resolve_root_member() {
    let mut source = String::from("class O:\n    def m(self, in_o):\n        return in_o\n\n");
    let mut below = String::from("O");
    for level in 0..25 {
        source.push_str(&format!(
            "class L{level}({below}):\n    pass\n\nclass R{level}({below}):\n    pass\n\nclass J{level}(L{level}, R{level}):\n    pass\n\n"
        ));
        below = format!("J{level}");
    }
    source.push_str(&format!(
        "class Top({below}):\n    def run(self):\n        return self.m(101)\n"
    ));
    let report = forward(&source, "101");
    assert!(binding(&report, "in_o"), "{report:#?}");
}

#[test]
fn test_analyze_python_pass_statement_raises_no_boundary() {
    let report = forward("def f():\n    pass\n\nx = 101\nconsume(x)\n", "101");
    assert!(
        !report
            .boundaries
            .iter()
            .any(|b| b.message.starts_with("pass_statement")),
        "{:?}",
        report.boundaries
    );
}
