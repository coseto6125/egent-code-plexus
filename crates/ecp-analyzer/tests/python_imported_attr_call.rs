//! `registry.handlers.dispatch(evt)` calls a method through a value a `from`
//! import names. The site is a member call: the free-only rule for bare
//! Python calls must not drop the method it binds.
mod name_ref_support;

use ecp_analyzer::python::parser::PythonProvider;
use name_ref_support::{calls, graph};

#[test]
fn test_resolve_call_python_member_of_imported_attribute_returns_method_edge() {
    let g = graph(
        &PythonProvider::new().unwrap(),
        &[
            (
                "app/handlers.py",
                "class Handlers:\n    def dispatch(self, evt):\n        return evt\n",
            ),
            (
                "app/registry.py",
                "from app.handlers import Handlers\n\nclass Registry:\n    handlers = Handlers()\n\nregistry = Registry()\n",
            ),
            (
                "app/main.py",
                "from app.registry import registry\n\ndef handle(evt):\n    return registry.handlers.dispatch(evt)\n",
            ),
        ],
    );
    assert_eq!(
        calls(&g, "handle"),
        vec![("app/handlers.py".to_string(), "dispatch".to_string())]
    );
}
