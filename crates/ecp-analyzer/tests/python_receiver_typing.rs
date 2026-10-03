//! Receiver typing, e2e: the binder types a parameter annotation (`d:
//! Derived`). The callee's method is inherited from a base type in another
//! file, and an unrelated decoy type declares a method of the same name. The
//! ladder must resolve the typed callee through the type's declared heritage to
//! the base.

mod receiver_typing_support;

use ecp_analyzer::python::parser::PythonProvider;
use receiver_typing_support::{
    assert_binder_emits, assert_single_call_into_base, build, callee_files, parse_all,
};

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

const BASE_B: &str = "class B:\n    def greet(self):\n        return 1\n";
const A_ON_EXTERNAL: &str = "from external_lib import External\n\n\nclass A(External):\n    pass\n";
const DECOY: &str = "class Decoy:\n    def greet(self):\n        return 2\n";
const APP: &str = "from .derived import Derived\n\n\ndef run(d: Derived):\n    return d.greet()\n";

fn greet_targets(derived: &str) -> Vec<String> {
    let provider = PythonProvider::new().expect("PythonProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            ("pkg/b.py", BASE_B),
            ("pkg/a.py", A_ON_EXTERNAL),
            ("pkg/derived.py", derived),
            ("pkg/decoy.py", DECOY),
            ("pkg/app.py", APP),
        ],
    );
    assert_binder_emits(&graphs, "run", "Derived.greet");
    callee_files(&build(graphs), "run", "greet")
}

/// MRO of `Derived(A, B)` is Derived, A, External, B: the unindexed
/// `External` comes before `B` and may define `greet`, though the
/// breadth-first walk meets `B.greet` first.
#[test]
fn test_unresolved_base_on_earlier_branch_blocks_later_owner() {
    let derived = "from .a import A\nfrom .b import B\n\n\nclass Derived(A, B):\n    pass\n";
    assert!(greet_targets(derived).is_empty());
}

/// `Derived(B, A)`: B's branch precedes A's, so `External` (above A) cannot
/// shadow `B.greet`.
#[test]
fn test_unresolved_base_on_later_branch_keeps_earlier_owner() {
    let derived = "from .a import A\nfrom .b import B\n\n\nclass Derived(B, A):\n    pass\n";
    assert_eq!(greet_targets(derived), vec!["pkg/b.py".to_string()]);
}

/// `greet = replacement` in `Derived` shadows the inherited method; the call
/// runs `replacement`, not `B.greet`.
#[test]
fn test_attribute_on_receiver_type_shadows_inherited_method() {
    let derived = "from .b import B\n\n\ndef replacement(self):\n    return 3\n\n\nclass Derived(B):\n    greet = replacement\n";
    assert!(!greet_targets(derived).contains(&"pkg/b.py".to_string()));
}

/// Diamond: `Derived(A, C)`, `A(M)`, `M(B)`, `C(External, N)`, `N(B)`. The
/// MRO is Derived, A, M, C, External, N, B: `External` comes before the
/// shared base `B`, though the first path the walk finds runs through A.
#[test]
fn test_unresolved_base_before_shared_ancestor_in_diamond_blocks_owner() {
    let provider = PythonProvider::new().expect("PythonProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            ("pkg/b.py", BASE_B),
            ("pkg/m.py", "from .b import B\n\n\nclass M(B):\n    pass\n"),
            ("pkg/a.py", "from .m import M\n\n\nclass A(M):\n    pass\n"),
            ("pkg/n.py", "from .b import B\n\n\nclass N(B):\n    pass\n"),
            (
                "pkg/c.py",
                "from external_lib import External\nfrom .n import N\n\n\nclass C(External, N):\n    pass\n",
            ),
            (
                "pkg/derived.py",
                "from .a import A\nfrom .c import C\n\n\nclass Derived(A, C):\n    pass\n",
            ),
            ("pkg/decoy.py", DECOY),
            ("pkg/app.py", APP),
        ],
    );
    assert_binder_emits(&graphs, "run", "Derived.greet");
    assert!(callee_files(&build(graphs), "run", "greet").is_empty());
}
