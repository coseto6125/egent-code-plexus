//! Receiver typing, e2e: PHP's binder types only `$this` (and `self` / `static`
//! / `parent`); here `$this` is the enclosing class. The callee's method is
//! inherited from a base type in another file, and an unrelated decoy type
//! declares a method of the same name. The ladder must resolve the typed callee
//! through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::php::parser::PhpProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_this_receiver_inherited_method_resolves_to_base_class() {
    let provider = PhpProvider::new().expect("PhpProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/Base.php",
                r#"<?php

class Base
{
    public function greet()
    {
        return 1;
    }
}
"#,
            ),
            (
                "src/Derived.php",
                r#"<?php

class Derived extends Base
{
    public function run()
    {
        return $this->greet();
    }
}
"#,
            ),
            (
                "src/Decoy.php",
                r#"<?php

class Decoy
{
    public function greet()
    {
        return 2;
    }
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "src/Base.php");
}
