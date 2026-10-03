//! Python instantiation calls: `A()` must give `ecp impact` a caller for `A`.
//! The constructor is `__init__`, so the Callable lookup for `A` finds only
//! the class and today resolves nothing.

mod instantiation_calls_support;

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_no_instantiation_call, assert_only_call,
    graph_of,
};

const WIDGET: (&str, &str) = (
    "pkg/widget.py",
    "class Widget:\n    def __init__(self, x):\n        self.x = x\n",
);
const GADGET: (&str, &str) = ("pkg/gadget.py", "class Gadget:\n    pass\n");
const MAKE_WIDGET: (&str, &str) = (
    "pkg/app.py",
    "from .widget import Widget\n\n\ndef make_widget():\n    return Widget(1)\n",
);

fn provider() -> PythonProvider {
    PythonProvider::new().expect("PythonProvider::new")
}

#[test]
fn test_python_cross_file_call_with_init_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "make_widget", "Widget", "pkg/widget.py");
}

#[test]
fn test_python_cross_file_call_without_init_calls_class() {
    let app = (
        "pkg/app.py",
        "from .gadget import Gadget\n\n\ndef make_gadget():\n    return Gadget()\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "make_gadget",
        "Gadget",
        NodeKind::Class,
        "pkg/gadget.py",
    );
}

#[test]
fn test_python_same_file_call_calls_constructor() {
    let local = (
        "pkg/local.py",
        "class Local:\n    def __init__(self):\n        self.ready = True\n\n\ndef make_local():\n    return Local()\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "make_local", "Local", "pkg/local.py");
}

/// `widget.Widget()` through a module import names the same class.
#[test]
fn test_python_module_qualified_call_calls_constructor() {
    let app = (
        "pkg/app.py",
        "from . import widget\n\n\ndef make_qualified():\n    return widget.Widget(1)\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_constructor(&graph, "make_qualified", "Widget", "pkg/widget.py");
}

/// A function named like an unrelated class keeps its call edge.
#[test]
fn test_python_same_named_function_call_targets_function() {
    let util = ("pkg/util.py", "def Thing():\n    return 1\n");
    let model = ("pkg/model.py", "class Thing:\n    pass\n");
    let app = (
        "pkg/app.py",
        "from .util import Thing\n\n\ndef call_thing():\n    return Thing()\n",
    );
    let graph = graph_of(&provider(), &[util, model, app]);
    assert_only_call(&graph, "call_thing", "Thing", "pkg/util.py");
}

/// `obj.Widget()` is a method call on an untyped receiver, never a
/// construction of class `Widget`.
#[test]
fn test_python_member_call_named_like_class_no_constructor_call() {
    let app = (
        "pkg/app.py",
        "def use_factory(obj):\n    return obj.Widget()\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_no_instantiation_call(&graph, "use_factory", "Widget");
}

/// Python allows a lowercase class name: `obj.widget()` on an untyped
/// receiver stays a method call, whatever the member's case.
#[test]
fn test_python_member_call_named_like_lowercase_class_no_constructor_call() {
    let model = ("pkg/model.py", "class widget:\n    pass\n");
    let app = ("pkg/app.py", "def use(obj):\n    return obj.widget()\n");
    let graph = graph_of(&provider(), &[model, app]);
    assert_no_instantiation_call(&graph, "use", "widget");
}

/// The alias names no declared type; the import maps it back to `Widget`.
#[test]
#[ignore = "FU-2026-10-04-2dd3fcfafccb: Python imports never reach the import tier, so an alias cannot be mapped back to Widget"]
fn test_python_aliased_import_call_calls_constructor() {
    let app = (
        "pkg/app.py",
        "from pkg.widget import Widget as W\n\n\ndef make_aliased():\n    return W(1)\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_constructor(&graph, "make_aliased", "Widget", "pkg/widget.py");
}
