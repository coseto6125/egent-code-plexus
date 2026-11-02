//! Python: a method returning `Self` (`typing.Self`) returns its own class, so `ecp impact` on the class lists it.
mod name_ref_support;

use ecp_analyzer::python::parser::PythonProvider;
use name_ref_support::{annotated, graph};

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// Contract: `def clone(self) -> Self` in `class Resolver` reaches `Resolver`,
/// bare and quoted (`-> "Self"`), like `-> "Resolver"`. Before the I3 gate the
/// edge looked up a type named `Self`: no such type, no edge.
#[test]
fn test_self_return_annotation_binds_declaring_class() {
    let g = graph(
        &PythonProvider::new().unwrap(),
        &[(
            "src/table.py",
            "class Resolver:\n    def clone(self) -> Self:\n        return self\n\n    def quoted(self) -> \"Self\":\n        return self\n\n    def plain(self) -> \"Resolver\":\n        return self\n",
        )],
    );
    let resolver = vec![pair("src/table.py", "Resolver")];
    assert_eq!(annotated(&g, "clone"), resolver, "-> Self");
    assert_eq!(annotated(&g, "quoted"), resolver, "-> \"Self\"");
    assert_eq!(annotated(&g, "plain"), resolver, "-> \"Resolver\"");
}

/// Guard: a free function has no declaring class, so `-> Self` binds nothing.
#[test]
fn test_self_return_annotation_outside_class_no_edge() {
    let g = graph(
        &PythonProvider::new().unwrap(),
        &[(
            "src/free.py",
            "class Resolver:\n    pass\n\ndef free() -> Self:\n    return Resolver()\n",
        )],
    );
    assert_eq!(annotated(&g, "free"), Vec::<(String, String)>::new());
}

/// Guard: Python already reduces the key, so `-> Tok | None` binds the project
/// class both before and after the change. `-> list[Tok]` is a generic and
/// binds nothing.
#[test]
fn test_optional_union_return_annotation_binds_project_type() {
    let g = graph(
        &PythonProvider::new().unwrap(),
        &[(
            "src/app.py",
            "class Tok:\n    pass\n\ndef find() -> Tok | None:\n    return None\n\ndef all() -> list[Tok]:\n    return []\n",
        )],
    );
    assert_eq!(annotated(&g, "find"), vec![pair("src/app.py", "Tok")]);
    assert_eq!(annotated(&g, "all"), Vec::<(String, String)>::new());
}
