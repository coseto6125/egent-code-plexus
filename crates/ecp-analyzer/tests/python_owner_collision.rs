use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = PythonProvider::new().expect("PythonProvider::new");
    provider
        .parse_file(Path::new("test.py"), source.as_bytes())
        .expect("parse_file")
}

fn owners_of(g: &LocalGraph, name: &str) -> Vec<Option<String>> {
    g.nodes
        .iter()
        .filter(|n| n.name == name)
        .map(|n| n.owner_class.clone())
        .collect()
}

/// Two decorator outer functions each contain `def wrapper()`.
/// Before this fix both emitted `owner_class=None` → uid collision.
/// After: each wrapper's `owner_class` is its enclosing function name.
#[test]
fn nested_wrapper_in_two_decorators_has_distinct_owners() {
    let src = "\
def login_required(f):
    def wrapper(*args, **kwargs):
        return f(*args, **kwargs)
    return wrapper

def cache(timeout):
    def wrapper(*args, **kwargs):
        return timeout
    return wrapper
";
    let g = parse(src);
    let wrappers: Vec<_> = g.nodes.iter().filter(|n| n.name == "wrapper").collect();
    assert!(
        wrappers.len() >= 2,
        "both wrapper() definitions must be emitted: {wrappers:?}"
    );
    let owners: Vec<_> = wrappers
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"login_required"),
        "one wrapper must have owner login_required; owners: {owners:?}"
    );
    assert!(
        owners.contains(&"cache"),
        "one wrapper must have owner cache; owners: {owners:?}"
    );
}

/// A `def wrapper()` nested inside a decorator function gets `owner_class`
/// set to the outer function name, NOT None.
#[test]
fn nested_function_owner_is_outer_function_name() {
    let src = "\
def my_decorator(f):
    def wrapper(*args, **kwargs):
        pass
    return wrapper
";
    let g = parse(src);
    let wrappers = owners_of(&g, "wrapper");
    assert!(!wrappers.is_empty(), "wrapper must be emitted");
    assert!(
        wrappers
            .iter()
            .any(|o| o.as_deref() == Some("my_decorator")),
        "wrapper must have owner_class=my_decorator; got {wrappers:?}"
    );
}

/// Module-level functions must NOT be assigned an owner from the fn-nesting pass.
#[test]
fn module_level_function_still_has_no_owner() {
    let src = "def top_level(): pass\n";
    let g = parse(src);
    let owners = owners_of(&g, "top_level");
    assert!(
        owners.iter().all(|o| o.is_none()),
        "top_level must have owner_class=None; got {owners:?}"
    );
}

/// Class-bound methods retain their class owner and are NOT re-stamped
/// by the function-nesting pass.
#[test]
fn class_method_owner_is_class_not_outer_function() {
    let src = "\
class MyClass:
    def my_method(self):
        pass
";
    let g = parse(src);
    let methods: Vec<_> = g.nodes.iter().filter(|n| n.name == "my_method").collect();
    assert!(!methods.is_empty(), "my_method must be emitted");
    for m in &methods {
        assert_eq!(
            m.owner_class.as_deref(),
            Some("MyClass"),
            "my_method must have owner MyClass; got {:?}",
            m.owner_class
        );
    }
}

/// Two nested classes with the same name inside different functions get
/// distinct owner_class values, preventing uid collisions.
#[test]
fn nested_class_inside_functions_has_distinct_owners() {
    let src = "\
def func_a():
    class Inner:
        pass

def func_b():
    class Inner:
        pass
";
    let g = parse(src);
    let inners: Vec<_> = g.nodes.iter().filter(|n| n.name == "Inner").collect();
    assert!(
        inners.len() >= 2,
        "both Inner class definitions must be emitted: {inners:?}"
    );
    let owners: Vec<_> = inners
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"func_a"),
        "one Inner must have owner func_a; owners: {owners:?}"
    );
    assert!(
        owners.contains(&"func_b"),
        "one Inner must have owner func_b; owners: {owners:?}"
    );
}
