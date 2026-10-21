//! Receiver typing, e2e: PHP's binder types only `$this` (and `self` / `static`
//! / `parent`); here `$this` is the enclosing class. The callee's method is
//! inherited from a base type in another file, and an unrelated decoy type
//! declares a method of the same name. The ladder must resolve the typed callee
//! through the type's declared heritage to the base.

mod instantiation_calls_support;
mod receiver_typing_support;

use ecp_analyzer::php::parser::PhpProvider;
use instantiation_calls_support::{calls_from, graph_of};
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

/// `$this` and `self::` in a trait or an enum name it as in a class: the
/// trait's own members, a member of the class that uses the trait, and an
/// enum's own method all bind. Untyped, the caller's own members were
/// unreachable by name.
#[test]
fn test_this_receiver_in_trait_and_enum_binds_member() {
    let source = r#"<?php
namespace App;
trait T {
    public function a($p) {
        $this->b(1, $p);
        $this->host();
        self::st();
    }
    protected function b($n, $p) {}
    public static function st() {}
}
enum E: string {
    case A = "a";
    public function label() { return $this->upper(); }
    private function upper() {}
}
class H {
    use T;
    public function host() {}
}
"#;
    let graph = graph_of(&PhpProvider::new().unwrap(), &[("src/T.php", source)]);
    let bound = |caller: &str| -> Vec<(String, String)> {
        let mut hits: Vec<_> = calls_from(&graph, caller)
            .into_iter()
            .map(|hit| (hit.owner, hit.name))
            .collect();
        hits.sort();
        hits
    };
    let pair = |owner: &str, name: &str| (owner.to_string(), name.to_string());
    assert_eq!(
        bound("a"),
        vec![pair("H", "host"), pair("T", "b"), pair("T", "st")]
    );
    assert_eq!(bound("label"), vec![pair("E", "upper")]);
}
