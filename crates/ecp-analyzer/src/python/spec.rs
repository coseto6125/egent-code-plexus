//! Python `LangSpec` — capture-name → NodeKind table.
//!
//! `queries.scm` capture names that produce a standalone symbol node:
//! `function.name`, `class.name`, `property.name`, `variable.name`.
//! All other captures (`heritage`, `export`, `decorator`, `type`,
//! `import.*`, `route.*`, `fastapi.*`, `django.*`, `celery.*`,
//! `blind.*`, `reflection.*`) are metadata-only and intentionally
//! absent from this map — they drive framework detection in `parser.rs`
//! and must not produce spurious `RawNode` entries.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub(crate) fn module_candidates(base: &str, visit: &mut impl FnMut(&str) -> bool) {
    // Python loads packages before modules, and implementation files before stubs.
    for suffix in ["/__init__.py", "/__init__.pyi", ".py", ".pyi"] {
        let candidate = if base.is_empty() {
            suffix.trim_start_matches('/').to_string()
        } else {
            format!("{base}{suffix}")
        };
        if !visit(&candidate) {
            break;
        }
    }
}

pub struct PythonSpec;

impl LangSpec for PythonSpec {
    const NAME: &'static str = "python";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "function.name" => NodeKind::Function,
        "class.name"    => NodeKind::Class,
        "property.name" => NodeKind::Property,
        "variable.name" => NodeKind::Variable,
    };
}
