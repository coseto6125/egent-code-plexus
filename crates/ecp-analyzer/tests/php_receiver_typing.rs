//! Receiver typing, e2e: PHP's binder types only `$this` (and `self` / `static`
//! / `parent`); here `$this` is the enclosing class. The callee's method is
//! inherited from a base type in another file, and an unrelated decoy type
//! declares a method of the same name. The ladder must resolve the typed callee
//! through the type's declared heritage to the base.

use crate::instantiation_calls_support;
use crate::name_ref_support;
use crate::receiver_typing_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::ZeroCopyGraph;
use instantiation_calls_support::{calls_from, graph_of};
use name_ref_support::function_refs;
use receiver_typing_support::{
    assert_single_call_into_base, call_rows, call_rows_to, parse_all, UNTYPED_GUESS,
};
use std::path::Path;

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

/// `$this` and `self::` in a trait or an enum name it for the members it
/// declares, which bind as a class's do; untyped, the caller's own members
/// were unreachable by name. A member the trait does not declare belongs to
/// the class that uses it and keeps the by-name route.
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

/// Laravel's `HasUniqueStringIds::getIncrementing` calls `$this->getKeyName()`,
/// which the model class declares in another file: the trait does not
/// declare it, so the call keeps binding by name.
#[test]
fn test_this_receiver_in_trait_host_member_in_other_file_binds_by_name() {
    let graph = graph_of(
        &PhpProvider::new().unwrap(),
        &[
            (
                "src/Model.php",
                "<?php\nnamespace App;\nclass Model {\n    use HasIds;\n    public function getKeyName() { return 'id'; }\n}\n",
            ),
            (
                "src/HasIds.php",
                "<?php\nnamespace App;\ntrait HasIds {\n    public function getIncrementing() {\n        return $this->getKeyName();\n    }\n}\n",
            ),
        ],
    );
    let hits: Vec<_> = calls_from(&graph, "getIncrementing")
        .into_iter()
        .map(|hit| (hit.owner, hit.name))
        .collect();
    assert_eq!(hits, vec![("Model".to_string(), "getKeyName".to_string())]);
}

const A_PHP: (&str, &str) = ("A.php", "<?php\nclass A { function unique_cb() {} }\n");
const B_PHP: (&str, &str) = ("B.php", "<?php\nclass B { function other() {} }\n");

/// `A` declares `unique_cb`, `B` does not; one caller per receiver shape.
fn untyped_project() -> ZeroCopyGraph {
    graph_of(
        &PhpProvider::new().unwrap(),
        &[
            A_PHP,
            B_PHP,
            (
                "hold_untyped.php",
                "<?php\nfunction hold($o) { return $o->unique_cb(); }\n",
            ),
            (
                "hold_typed_a.php",
                "<?php\nfunction holdA(A $o) { return $o->unique_cb(); }\n",
            ),
            (
                "hold_typed_b.php",
                "<?php\nfunction holdB(B $o) { return $o->unique_cb(); }\n",
            ),
            (
                "hold_branches.php",
                "<?php\nfunction holdJ($c) {\n    if ($c) { $o = new A(); } else { $o = new B(); }\n    return $o->unique_cb();\n}\n",
            ),
            (
                "hold_fcc.php",
                "<?php\nfunction holdF($o) { return $o->unique_cb(...); }\n",
            ),
        ],
    )
}

fn guess_row() -> (String, String, String, f32) {
    (
        "A.php".to_string(),
        "unique_cb".to_string(),
        UNTYPED_GUESS.1.to_string(),
        UNTYPED_GUESS.0,
    )
}

/// Stands for: PHP is a guess language (decision A09). A receiver with no
/// evidence binds the method by its name alone as the tagged guess
/// `(0.55, "call:untyped-member")`, and a typed receiver keeps its full `call`.
#[test]
fn test_untyped_member_call_php_binds_by_name_as_tagged_guess() {
    receiver_typing_support::assert_untyped_member_is_guess("php");
    let g = untyped_project();
    assert_eq!(call_rows_to(&g, "hold", "unique_cb"), vec![guess_row()]);
    let typed = call_rows_to(&g, "holdA", "unique_cb");
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert_eq!(
        (typed[0].0.as_str(), typed[0].2.as_str()),
        ("A.php", "call")
    );
    assert!(typed[0].3 >= 0.7, "{typed:?}");
}

/// Stands for: a receiver proven to be another class on every path binds no
/// method of `A`; the untyped sibling `hold` in the same graph does bind.
#[test]
fn test_calls_php_receiver_typed_as_other_class_binds_nothing() {
    let g = untyped_project();
    assert_eq!(call_rows_to(&g, "holdB", "unique_cb"), vec![]);
    assert_eq!(call_rows_to(&g, "hold", "unique_cb"), vec![guess_row()]);
}

/// Stands for: branches that disagree on the class leave the receiver
/// without one proven type, so the call binds only as the tagged guess
/// (decision A09 overrides the case text "binds nothing"), never as a
/// full-confidence `call`.
#[test]
fn test_calls_php_receiver_with_conflicting_branch_types_binds_tagged_guess() {
    let g = untyped_project();
    assert_eq!(call_rows_to(&g, "holdJ", "unique_cb"), vec![guess_row()]);
}

/// Stands for: a first-class callable `$o->m(...)` on an untyped receiver
/// records no `References`, and its `Calls` edge exists only as the tagged
/// guess.
#[test]
fn test_calls_php_first_class_callable_untyped_receiver_binds_only_tagged_guess() {
    let g = untyped_project();
    assert_eq!(function_refs(&g, "holdF"), vec![]);
    let rows = call_rows_to(&g, "holdF", "unique_cb");
    assert!(rows.iter().all(|row| *row == guess_row()), "{rows:?}");
    assert_eq!(call_rows_to(&g, "hold", "unique_cb"), vec![guess_row()]);
}

/// Stands for: a receiver typed `A` keeps the full-confidence `call` edge.
#[test]
fn test_calls_php_typed_receiver_keeps_tier_confidence() {
    let g = untyped_project();
    let rows = call_rows_to(&g, "holdA", "unique_cb");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].0.as_str(), rows[0].2.as_str()), ("A.php", "call"));
    assert!(rows[0].3 >= 0.7, "{rows:?}");
}

/// An empty caller body is the degenerate input: no `Calls` edge at all.
#[test]
fn test_calls_php_empty_function_body_binds_nothing() {
    let g = graph_of(
        &PhpProvider::new().unwrap(),
        &[A_PHP, ("e.php", "<?php\nfunction empty_fn($o) {}\n")],
    );
    assert_eq!(call_rows(&g, "empty_fn"), vec![]);
}

/// Stands for (M01, PHP): a same-named class added in another language
/// leaves the PHP typed call on its own file, and no edge reaches the added
/// file.
#[test]
fn test_calls_php_typed_receiver_with_python_namesake_binds_php_method() {
    let php = PhpProvider::new().unwrap();
    let python = PythonProvider::new().unwrap();
    let cls = ("Cls.php", "<?php\nclass Cls { function m() {} }\n");
    let usage = ("use.php", "<?php\nfunction run(Cls $c) { $c->m(); }\n");
    let build = |with_python: bool| {
        let mut builder = GraphBuilder::new();
        for (path, src) in [cls, usage] {
            builder.add_graph(php.parse_file(Path::new(path), src.as_bytes()).unwrap());
        }
        if with_python {
            builder.add_graph(
                python
                    .parse_file(
                        Path::new("cls.py"),
                        b"class Cls:\n    def m(self):\n        pass\n",
                    )
                    .unwrap(),
            );
        }
        builder.build()
    };
    let base = call_rows_to(&build(false), "run", "m");
    assert_eq!(base.len(), 1, "{base:?}");
    assert_eq!(
        (base[0].0.as_str(), base[0].2.as_str()),
        ("Cls.php", "call")
    );
    let variant = call_rows_to(&build(true), "run", "m");
    assert_eq!(
        variant, base,
        "the added cls.py must not change the verdict"
    );
}
