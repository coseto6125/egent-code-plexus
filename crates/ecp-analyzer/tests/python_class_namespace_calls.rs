//! A bare call in a Python class body calls `T.name` when the class binds
//! `name` above the call, and the module's `name` otherwise. Python runs a
//! class body top to bottom in the class namespace, then the module globals.
//! A member the call should miss is defined after the call in these sources, so
//! a wrong rewrite to `T.name` reaches a node and shows as a changed owner.

use crate::name_ref_support::graph;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::graph::RelType;

/// The sorted, deduplicated owner classes of the targets of every Calls edge
/// out of any node named in `callers`; `""` is a module-level function.
fn owners(src: &str, callers: &[&str]) -> Vec<String> {
    let g = graph(&PythonProvider::new().unwrap(), &[("t.py", src)]);
    let pool = g.string_pool.as_slice();
    let mut out: Vec<String> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| callers.contains(&g.nodes[e.source as usize].name.resolve(pool)))
        .map(|e| {
            g.nodes[e.target as usize]
                .owner_class
                .resolve(pool)
                .to_owned()
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[test]
fn test_bind_class_body_call_annotation_only_field_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    target: int\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_annotated_assignment_with_value_binds_member() {
    let src = "def target(): pass\nclass T:\n    target: int = 1\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_if_block_binds_member() {
    let src = "def target(): pass\nclass T:\n    if True:\n        def target(self): pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_else_block_binds_member() {
    let src = "def target(): pass\nclass T:\n    if False:\n        pass\n    else:\n        def target(self): pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_try_block_binds_member() {
    let src = "def target(): pass\nclass T:\n    try:\n        def target(self): pass\n    except Exception:\n        pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_except_block_binds_member() {
    let src = "def target(): pass\nclass T:\n    try:\n        pass\n    except Exception:\n        def target(self): pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_finally_block_binds_member() {
    let src = "def target(): pass\nclass T:\n    try:\n        pass\n    finally:\n        def target(self): pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_with_block_binds_member() {
    let src = "def target(): pass\nclass T:\n    with open('f'):\n        def target(self): pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_for_block_binds_member() {
    let src = "def target(): pass\nclass T:\n    for _i in range(1):\n        def target(self): pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_while_block_binds_member() {
    let src = "def target(): pass\nclass T:\n    while False:\n        def target(self): pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_definition_in_nested_compound_binds_member() {
    let src = "def target(): pass\nclass T:\n    if True:\n        try:\n            def target(self): pass\n        except Exception:\n            pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_import_binds_member() {
    let src = "def target(): pass\nclass T:\n    import target\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_from_import_binds_member() {
    let src = "def target(): pass\nclass T:\n    from m import target\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_aliased_from_import_binds_member() {
    let src = "def target(): pass\nclass T:\n    from m import other as target\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_aliased_import_binds_member() {
    let src = "def target(): pass\nclass T:\n    import other as target\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_dotted_import_binds_first_segment_binds_member() {
    let src = "def target(): pass\nclass T:\n    import target.sub\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_tuple_assignment_binds_member() {
    let src = "def target(): pass\nclass T:\n    a, target = 1, 2\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_chained_assignment_binds_member() {
    let src = "def target(): pass\nclass T:\n    a = target = 1\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_for_target_binds_member() {
    let src = "def target(): pass\nclass T:\n    for target in range(1): pass\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_with_alias_binds_member() {
    let src = "def target(): pass\nclass T:\n    with open('f') as target: pass\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_compound_definition_after_call_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    x = target()\n    if True:\n        def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_import_after_call_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    x = target()\n    from m import target\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_self_assignment_reads_module_callee() {
    let src = "def target(): pass\nclass T:\n    target = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["target"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_self_assignment_in_if_reads_module_callee() {
    let src = "def target(): pass\nclass T:\n    if True:\n        target = target()\n    def target(self): pass\n";
    // An assignment inside a class-body `if` declares no Variable, so `T` holds the call.
    assert_eq!(owners(src, &["T", "target"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_name_defined_twice_binds_at_first_definition() {
    let src = "def target(): pass\nclass T:\n    def target(self): pass\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_name_defined_twice_after_call_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    x = target()\n    def target(self): pass\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_function_body_definition_does_not_bind() {
    let src = "def target(): pass\nclass T:\n    def helper(self):\n        def target(): pass\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_nested_class_member_does_not_bind_outer() {
    let src = "def target(): pass\nclass T:\n    class Inner:\n        def target(self): pass\n    x = target()\n    def target(self): pass\n";
    assert_eq!(owners(src, &["x"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_method_parameter_default_binds_member() {
    let src =
        "def target(): pass\nclass T:\n    def target(): pass\n    def m(self, a=target()): pass\n";
    assert_eq!(owners(src, &["m"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_method_parameter_default_before_definition_keeps_module_callee() {
    let src =
        "def target(): pass\nclass T:\n    def m(self, a=target()): pass\n    def target(): pass\n";
    assert_eq!(owners(src, &["m"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_method_keyword_default_binds_member() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    def m(self, *, a=target()): pass\n";
    assert_eq!(owners(src, &["m"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_function_parameter_default_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\ndef m(a=target()): pass\n";
    assert_eq!(owners(src, &["m"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_method_decorator_binds_member() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    @target()\n    def m(self): pass\n";
    assert_eq!(owners(src, &["m", "T"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_lambda_parameter_default_binds_member() {
    let src =
        "def target(): pass\nclass T:\n    def target(): pass\n    f = lambda a=target(): a\n";
    assert_eq!(owners(src, &["f"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_lambda_body_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    f = lambda: target()\n";
    assert_eq!(owners(src, &["f"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_staticmethod_definition_binds_member() {
    let src = "def target(): pass\nclass T:\n    @staticmethod\n    def target(): pass\n    x = target()\n";
    assert_eq!(owners(src, &["x"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_nested_class_base_list_binds_outer_member() {
    let src =
        "def mk(): pass\nclass Outer:\n    def mk(): pass\n    class Inner(mk()):\n        pass\n";
    assert_eq!(owners(src, &["Inner", "Outer"]), vec!["Outer".to_string()]);
}

#[test]
fn test_bind_class_body_call_nested_class_keyword_base_binds_outer_member() {
    let src = "def mk(): pass\nclass Outer:\n    def mk(): pass\n    class Inner(metaclass=mk()):\n        pass\n";
    assert_eq!(owners(src, &["Inner", "Outer"]), vec!["Outer".to_string()]);
}

#[test]
fn test_bind_class_body_call_nested_class_decorator_binds_outer_member() {
    let src = "def mk(): pass\nclass Outer:\n    def mk(): pass\n    @mk()\n    class Inner:\n        pass\n";
    assert_eq!(owners(src, &["Inner", "Outer"]), vec!["Outer".to_string()]);
}

#[test]
fn test_bind_class_body_call_nested_class_base_list_ignores_inner_member() {
    let src = "def mk(): pass\nclass Outer:\n    class Inner(mk()):\n        def mk(self): pass\n";
    assert_eq!(owners(src, &["Inner", "Outer"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_module_class_base_list_keeps_module_callee() {
    let src = "def mk(): pass\nclass Inner(mk()):\n    def mk(self): pass\n";
    assert_eq!(owners(src, &["Inner"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_comprehension_outermost_iterable_binds_member() {
    let src =
        "def target(): pass\nclass T:\n    def target(): pass\n    xs = [i for i in target()]\n";
    assert_eq!(owners(src, &["xs"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_generator_outermost_iterable_binds_member() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    xs = list(i for i in target())\n";
    assert_eq!(owners(src, &["xs"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_dict_comprehension_outermost_iterable_binds_member() {
    let src =
        "def target(): pass\nclass T:\n    def target(): pass\n    xs = {i: i for i in target()}\n";
    assert_eq!(owners(src, &["xs"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_comprehension_element_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    xs = [target() for i in range(1)]\n";
    assert_eq!(owners(src, &["xs"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_comprehension_second_iterable_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    xs = [j for i in range(1) for j in target()]\n";
    assert_eq!(owners(src, &["xs"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_comprehension_condition_keeps_module_callee() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    xs = [i for i in range(1) if target()]\n";
    assert_eq!(owners(src, &["xs"]), vec!["".to_string()]);
}

#[test]
fn test_bind_class_body_call_nested_comprehension_outermost_iterable_binds_member() {
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    xs = [i for i in [j for j in target()]]\n";
    assert_eq!(owners(src, &["xs"]), vec!["T".to_string()]);
}

#[test]
fn test_bind_class_body_call_many_members_binds_each_to_class() {
    let members = 3000;
    let mut src = String::from("def auto(): pass\nclass T:\n    def auto(): pass\n");
    for i in 0..members {
        src.push_str(&format!("    m{i} = auto()\n"));
    }
    let last = format!("m{}", members - 1);
    assert_eq!(owners(&src, &["m0", &last]), vec!["T".to_string()]);
}
