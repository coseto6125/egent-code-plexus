//! Parser fixes for Python. FU-2026-10-30-cb6cc554a170 removes the dead
//! `@export` capture; the export flag must not change.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

/// FU-2026-10-30-cb6cc554a170 guard: `is_exported` is the leading-underscore
/// rule for functions and classes, before and after the dead capture goes.
#[test]
fn test_python_export_flag_follows_leading_underscore() {
    let g = PythonProvider::new()
        .expect("PythonProvider::new")
        .parse_file(
            Path::new("a.py"),
            b"def pub(): pass\ndef _priv(): pass\nclass Pub: pass\nclass _Priv: pass\ndef __dunder__(): pass\n",
        )
        .expect("parse_file");
    let flags: Vec<(&str, NodeKind, bool)> = g
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.kind, n.is_exported))
        .collect();
    assert_eq!(
        flags,
        [
            ("pub", NodeKind::Function, true),
            ("_priv", NodeKind::Function, false),
            ("Pub", NodeKind::Class, true),
            ("_Priv", NodeKind::Class, false),
            ("__dunder__", NodeKind::Function, false),
        ]
    );
}
