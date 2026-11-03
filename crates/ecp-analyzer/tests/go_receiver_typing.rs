//! Receiver typing, e2e: the binder types a pointer parameter (`d *Derived`).
//! Go has no class inheritance; struct embedding is recorded as heritage
//! (`go/queries.scm`), and the embedded type's methods are promoted. The
//! callee's method is inherited from a base type in another file, and an
//! unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

use crate::receiver_typing_support;

use ecp_analyzer::go::parser::GoProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_typed_param_promoted_method_resolves_to_embedded_struct() {
    let provider = GoProvider::new().expect("GoProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "pkg/base.go",
                r#"package pkg

type Base struct{}

func (b *Base) Greet() int {
	return 1
}
"#,
            ),
            (
                "pkg/derived.go",
                r#"package pkg

type Derived struct {
	Base
}
"#,
            ),
            (
                "pkg/decoy.go",
                r#"package pkg

type Decoy struct{}

func (d Decoy) Greet() int {
	return 2
}
"#,
            ),
            (
                "pkg/app.go",
                r#"package pkg

func Run(d *Derived) int {
	return d.Greet()
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "Run", "Derived.Greet", "Greet", "pkg/base.go");
}

/// Rule U: in Go, a member call on an untyped receiver that binds a method by
/// its name alone is a guess: 0.55, `call:untyped-member`, from any tier.
/// The caller's own type and an ambiguous name bind nothing; the nearest
/// copy of a duplicated declaration keeps 0.6.
#[test]
fn test_untyped_member_call_binds_by_name_as_tagged_guess() {
    receiver_typing_support::assert_untyped_member_is_guess("go");
}
