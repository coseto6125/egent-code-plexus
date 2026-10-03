//! Receiver typing, e2e: the binder types a parameter annotation (`d:
//! Derived`). The callee's method is inherited from a base type in another
//! file, and an unrelated decoy type declares a method of the same name. The
//! ladder must resolve the typed callee through the type's declared heritage to
//! the base.

mod receiver_typing_support;

use ecp_analyzer::python::parser::PythonProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = PythonProvider::new().expect("PythonProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "pkg/base.py",
                r#"class Base:
    def greet(self):
        return 1
"#,
            ),
            (
                "pkg/derived.py",
                r#"from .base import Base


class Derived(Base):
    pass
"#,
            ),
            (
                "pkg/decoy.py",
                r#"class Decoy:
    def greet(self):
        return 2
"#,
            ),
            (
                "pkg/app.py",
                r#"from .derived import Derived


def run(d: Derived):
    return d.greet()
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "pkg/base.py");
}
