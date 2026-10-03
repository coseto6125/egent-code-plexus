//! Receiver typing, e2e: the binder types a method parameter (`Derived d`). The
//! callee's method is inherited from a base type in another file, and an
//! unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/Base.cs",
                r#"public class Base
{
    public int Greet()
    {
        return 1;
    }
}
"#,
            ),
            (
                "src/Derived.cs",
                r#"public class Derived : Base
{
}
"#,
            ),
            (
                "src/Decoy.cs",
                r#"public class Decoy
{
    public int Greet()
    {
        return 2;
    }
}
"#,
            ),
            (
                "src/App.cs",
                r#"public class App
{
    public int Run(Derived d)
    {
        return d.Greet();
    }
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "Run", "Derived.Greet", "Greet", "src/Base.cs");
}
