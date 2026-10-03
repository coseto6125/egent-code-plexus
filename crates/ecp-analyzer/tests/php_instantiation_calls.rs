//! PHP instantiation calls: `new A()` must give `ecp impact` a caller for `A`.
//! The call queries do not capture `object_creation_expression`, so today
//! every `new` site is invisible to the graph.

mod instantiation_calls_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_no_instantiation_call, assert_only_call,
    graph_of,
};

const WIDGET: (&str, &str) = (
    "src/Widget.php",
    "<?php\n\nclass Widget\n{\n    public function __construct($x)\n    {\n    }\n}\n",
);
const GADGET: (&str, &str) = ("src/Gadget.php", "<?php\n\nclass Gadget\n{\n}\n");
const ITEM: (&str, &str) = (
    "src/Models/Item.php",
    r"<?php

namespace App\Models;

class Item
{
    public function __construct()
    {
    }
}
",
);
const MAKE_WIDGET: (&str, &str) = (
    "src/app.php",
    "<?php\n\nfunction makeWidget()\n{\n    return new Widget(1);\n}\n",
);

fn provider() -> PhpProvider {
    PhpProvider::new().expect("PhpProvider::new")
}

#[test]
fn test_php_cross_file_new_with_constructor_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "makeWidget", "Widget", "src/Widget.php");
}

#[test]
fn test_php_cross_file_new_without_constructor_calls_class() {
    let app = (
        "src/app.php",
        "<?php\n\nfunction makeGadget()\n{\n    return new Gadget();\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "makeGadget",
        "Gadget",
        NodeKind::Class,
        "src/Gadget.php",
    );
}

#[test]
fn test_php_same_file_new_calls_constructor() {
    let local = (
        "src/local.php",
        "<?php\n\nclass Local\n{\n    public function __construct()\n    {\n    }\n}\n\nfunction makeLocal()\n{\n    return new Local();\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "makeLocal", "Local", "src/local.php");
}

#[test]
fn test_php_imported_namespaced_new_calls_constructor() {
    let app = (
        "src/app.php",
        r"<?php

use App\Models\Item;

function makeItem()
{
    return new Item();
}
",
    );
    let graph = graph_of(&provider(), &[ITEM, app]);
    assert_calls_constructor(&graph, "makeItem", "Item", "src/Models/Item.php");
}

#[test]
fn test_php_fully_qualified_new_calls_constructor() {
    let app = (
        "src/app.php",
        r"<?php

function makeItemQualified()
{
    return new \App\Models\Item();
}
",
    );
    let graph = graph_of(&provider(), &[ITEM, app]);
    assert_calls_constructor(&graph, "makeItemQualified", "Item", "src/Models/Item.php");
}

/// PHP keeps functions and classes in separate symbol tables: a function
/// named like an unrelated class keeps its call edge.
#[test]
fn test_php_same_named_function_call_targets_function() {
    let util = (
        "src/util.php",
        "<?php\n\nfunction Thing()\n{\n    return 1;\n}\n",
    );
    let model = ("src/Thing.php", "<?php\n\nclass Thing\n{\n}\n");
    let app = (
        "src/app.php",
        "<?php\n\nfunction callThing()\n{\n    return Thing();\n}\n",
    );
    let graph = graph_of(&provider(), &[util, model, app]);
    assert_only_call(&graph, "callThing", "Thing", "src/util.php");
}

/// `$obj->Widget()` is a method call, never a construction of class `Widget`.
#[test]
fn test_php_member_call_named_like_class_no_constructor_call() {
    let app = (
        "src/app.php",
        "<?php\n\nfunction useFactory($obj)\n{\n    return $obj->Widget();\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_no_instantiation_call(&graph, "useFactory", "Widget");
}
