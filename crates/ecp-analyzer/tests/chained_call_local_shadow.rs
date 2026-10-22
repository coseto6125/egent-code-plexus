//! A chained call through a local callable must not use a same-named
//! declaration's return type. The local callable can still be called; ECP
//! simply does not infer the type of its result yet.

mod instantiation_calls_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::graph::ZeroCopyGraph;
use instantiation_calls_support::{calls_from, graph_of};

fn owners(graph: &ZeroCopyGraph, caller: &str, member: &str) -> Vec<String> {
    let mut owners: Vec<_> = calls_from(graph, caller)
        .into_iter()
        .filter(|hit| hit.name == member)
        .map(|hit| hit.owner)
        .collect();
    owners.sort();
    owners
}

fn assert_no_alpha_bar(graph: &ZeroCopyGraph, caller: &str) {
    assert!(
        !owners(graph, caller, "Bar").contains(&"Alpha".to_string()),
        "{caller}: local callable must not infer Alpha.Bar; calls: {:?}",
        calls_from(graph, caller)
    );
}

fn assert_alpha_bar(graph: &ZeroCopyGraph, caller: &str) {
    assert_eq!(
        owners(graph, caller, "Bar"),
        ["Alpha"],
        "{caller}: module callable must still infer Alpha.Bar; calls: {:?}",
        calls_from(graph, caller)
    );
}

#[test]
fn test_cpp_local_callable_chain_does_not_use_free_function_return_type() {
    let provider = CppProvider::new().expect("CppProvider::new");
    let graph = graph_of(
        &provider,
        &[(
            "src/shadow.cpp",
            "class Alpha { public: void Bar() {} };\n\
             class Beta { public: void Bar() {} };\n\
             Alpha make() { return Alpha(); }\n\
             void local() { auto make = []() { return Beta(); }; make().Bar(); }\n\
             void parameter(std::function<Beta()> make) { make().Bar(); }\n\
             void module() { make().Bar(); }\n",
        )],
    );

    assert_no_alpha_bar(&graph, "local");
    assert_no_alpha_bar(&graph, "parameter");
    assert_alpha_bar(&graph, "module");
}

#[test]
fn test_cpp_callable_field_chain_does_not_use_free_function_return_type() {
    let provider = CppProvider::new().expect("CppProvider::new");
    let graph = graph_of(
        &provider,
        &[(
            "src/field.cpp",
            "class Alpha { public: void Bar() {} };\n\
             class Beta { public: void Bar() {} };\n\
             Alpha* make() { return nullptr; }\n\
             class Maker { public: Beta* operator()() { return nullptr; } };\n\
             class Use {\n\
                 Maker make;\n\
             public:\n\
                 void field() { make()->Bar(); }\n\
             };\n",
        )],
    );

    assert_no_alpha_bar(&graph, "field");
}

#[test]
fn test_cpp_function_pointer_parameter_chain_does_not_use_free_function_return_type() {
    let provider = CppProvider::new().expect("CppProvider::new");
    let graph = graph_of(
        &provider,
        &[(
            "src/function_pointer.cpp",
            "class Alpha { public: void Bar() {} };\n\
             class Beta { public: void Bar() {} };\n\
             Alpha* make() { return nullptr; }\n\
             void parameter(Beta* (*make)()) { make()->Bar(); }\n",
        )],
    );

    assert_no_alpha_bar(&graph, "parameter");
}

#[test]
fn test_csharp_local_callable_chain_does_not_use_member_function_return_type() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graph = graph_of(
        &provider,
        &[(
            "src/Shadow.cs",
            "using System;\n\
             class Alpha { public void Bar() {} }\n\
             class Beta { public void Bar() {} }\n\
             class Use {\n\
                 public Alpha Make() { return new Alpha(); }\n\
                 public void Local() { Func<Beta> Make = () => new Beta(); Make().Bar(); }\n\
                 public void Parameter(Func<Beta> Make) { Make().Bar(); }\n\
                 public void Module() { Make().Bar(); }\n\
             }\n",
        )],
    );

    assert_no_alpha_bar(&graph, "Local");
    assert_no_alpha_bar(&graph, "Parameter");
    assert_alpha_bar(&graph, "Module");
}

#[test]
fn test_csharp_inherited_callable_field_chain_does_not_use_base_method_return_type() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graph = graph_of(
        &provider,
        &[(
            "src/Field.cs",
            "using System;\n\
             class Alpha { public void Bar() {} }\n\
             class Beta { public void Bar() {} }\n\
             class Base { protected Alpha Make() { return new Alpha(); } }\n\
             class Derived : Base {\n\
                 private Func<Beta> Make = () => new Beta();\n\
                 public void Field() { Make().Bar(); }\n\
             }\n",
        )],
    );

    assert_no_alpha_bar(&graph, "Field");
}
