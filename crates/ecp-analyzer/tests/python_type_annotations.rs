//! Python: a method returning `Self` (`typing.Self`) returns its own class, so `ecp impact` on the class lists it.
use crate::name_ref_support;

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
/// class both before and after the change. `-> list[Tok]` binds `Tok` as its
/// element type.
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
    assert_eq!(annotated(&g, "all"), vec![pair("src/app.py", "Tok")]);
}

/// Contract: a class-level annotation (`st_field: St`, with or without a
/// value), a dataclass field and a quoted forward reference reach the
/// project class, like a parameter does. A builtin, a `Generic` type
/// variable and a bare assignment bind nothing.
#[test]
fn test_class_field_annotation_binds_project_type() {
    let g = graph(
        &PythonProvider::new().unwrap(),
        &[(
            "src/app.py",
            "class St:\n    pass\n\n\
             class C(Generic[T]):\n    st_field: St\n    st_init: St = None\n    quoted: \"St\"\n    plain: int = 1\n    tparam: T\n    bare = 1\n\n\
             @dataclass\nclass D:\n    st_dc: St\n    n: int = 0\n",
        )],
    );
    let st = vec![pair("src/app.py", "St")];
    assert_eq!(annotated(&g, "st_field"), st, "annotation only");
    assert_eq!(annotated(&g, "st_init"), st, "annotation with value");
    assert_eq!(annotated(&g, "quoted"), st, "forward reference");
    assert_eq!(annotated(&g, "st_dc"), st, "dataclass field");
    let none = Vec::<(String, String)>::new();
    assert_eq!(annotated(&g, "plain"), none, "builtin");
    assert_eq!(annotated(&g, "tparam"), none, "type variable");
    assert_eq!(annotated(&g, "bare"), none, "no annotation");
    assert_eq!(annotated(&g, "n"), none, "builtin dataclass field");
}

/// Contract: an instance attribute annotated in a method (`self.st: St = ...`)
/// binds the same way as a class-level one.
#[test]
fn test_self_attribute_annotation_binds_project_type() {
    let g = graph(
        &PythonProvider::new().unwrap(),
        &[(
            "src/app.py",
            "class St:\n    pass\n\nclass C:\n    def __init__(self):\n        self.st_attr: St = St()\n        self.n: int = 0\n",
        )],
    );
    assert_eq!(annotated(&g, "st_attr"), [pair("src/app.py", "St")]);
    assert_eq!(annotated(&g, "n"), Vec::<(String, String)>::new());
}
