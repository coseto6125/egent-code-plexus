//! FU-2026-10-05-0ee319339782: a Python call through a package re-export
//! (`pkg/__init__.py`: `from .core import compute`) or through a submodule
//! from-import (`from pkg.sub import mod; mod.compute()`) binds through the
//! import tier (0.95), following the re-export one hop. Before the fix these
//! calls kept the global tier: 0.7 for a unique name, no edge for an
//! ambiguous one.
//!
//! The guard tests pin what must not change: a second hop is not followed,
//! a cycle ends, a missing re-exported name keeps the global tier, a module
//! attribute shadows a submodule, and Kotlin's same shape resolves as before.
//! Their expected values are what ecp 0.14.8 produced on the same fixtures.
mod instantiation_calls_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_core::graph::ZeroCopyGraph;
use instantiation_calls_support::{assert_call_targets, graph_of};

fn import_tier() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

fn global_tier() -> f32 {
    ResolutionTier::Global.base_confidence()
}

/// `pkg/__init__.py` re-exports `compute` from `pkg/core.py`; it has no
/// `__all__`.
const PACKAGE: &[(&str, &str)] = &[
    ("pkg/__init__.py", "from .core import compute\n"),
    ("pkg/core.py", "def compute():\n    return 1\n"),
    ("pkg/sub/__init__.py", ""),
];

/// Two more `compute`s make the name ambiguous to the global tier.
const NAMESAKES: &[(&str, &str)] = &[
    ("pkg/other.py", "def compute():\n    return 2\n"),
    ("pkg/sub/mod.py", "def compute():\n    return 3\n"),
];

fn python(files: &[&[(&str, &str)]], caller: (&str, &str)) -> ZeroCopyGraph {
    let mut all: Vec<(&str, &str)> = files.iter().flat_map(|f| f.iter().copied()).collect();
    all.push(caller);
    graph_of(&PythonProvider::new().unwrap(), &all)
}

fn go(import: &str, call: &str) -> String {
    format!("{import}\n\ndef go():\n    return {call}\n")
}

#[test]
fn test_python_reexport_from_import_ambiguous_name_binds_import_tier() {
    for (import, call) in [
        ("from pkg import compute", "compute()"),
        ("from pkg import compute as c", "c()"),
    ] {
        let app = go(import, call);
        let graph = python(&[PACKAGE, NAMESAKES], ("app.py", &app));
        assert_call_targets(
            &graph,
            "go",
            &[("pkg/core.py", "", "compute", import_tier())],
        );
    }
}

#[test]
fn test_python_reexport_from_import_unique_name_binds_import_tier() {
    let app = go("from pkg import compute", "compute()");
    let graph = python(&[PACKAGE], ("app.py", &app));
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/core.py", "", "compute", import_tier())],
    );
}

#[test]
fn test_python_reexport_module_import_ambiguous_name_binds_import_tier() {
    for (import, call) in [
        ("import pkg", "pkg.compute()"),
        ("import pkg as p", "p.compute()"),
    ] {
        let app = go(import, call);
        let graph = python(&[PACKAGE, NAMESAKES], ("app.py", &app));
        assert_call_targets(
            &graph,
            "go",
            &[("pkg/core.py", "", "compute", import_tier())],
        );
    }
}

/// `from pkg.sub import mod`: `pkg/sub/__init__.py` declares no `mod`, so the
/// import binds the submodule, as a namespace import of `pkg.sub.mod` would.
#[test]
fn test_python_submodule_from_import_ambiguous_name_binds_submodule() {
    for (caller, import, call, target) in [
        (
            "app.py",
            "from pkg.sub import mod",
            "mod.compute()",
            "pkg/sub/mod.py",
        ),
        (
            "app.py",
            "from pkg.sub import mod as m",
            "m.compute()",
            "pkg/sub/mod.py",
        ),
        (
            "pkg/app.py",
            "from .sub import mod",
            "mod.compute()",
            "pkg/sub/mod.py",
        ),
        (
            "pkg/app.py",
            "from . import other",
            "other.compute()",
            "pkg/other.py",
        ),
    ] {
        let app = go(import, call);
        let graph = python(&[PACKAGE, NAMESAKES], (caller, &app));
        assert_call_targets(&graph, "go", &[(target, "", "compute", import_tier())]);
    }
}

/// A package attribute named like the submodule shadows it, as in Python:
/// the call keeps its previous path (no edge on ecp 0.14.8).
#[test]
fn test_python_submodule_from_import_module_attribute_keeps_previous_path() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", ""),
            (
                "pkg/sub/__init__.py",
                "class mod:\n    @staticmethod\n    def compute():\n        return 4\n",
            ),
            ("pkg/sub/mod.py", "def compute():\n    return 3\n"),
        ]],
        ("app.py", &go("from pkg.sub import mod", "mod.compute()")),
    );
    assert_call_targets(&graph, "go", &[]);
}

#[test]
fn test_python_reexport_init_with_dunder_all_binds_import_tier() {
    let graph = python(
        &[
            &[
                (
                    "pkg/__init__.py",
                    "from .core import compute\n\n__all__ = [\"compute\"]\n",
                ),
                ("pkg/core.py", "def compute():\n    return 1\n"),
            ],
            NAMESAKES,
        ],
        ("app.py", &go("from pkg import compute", "compute()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/core.py", "", "compute", import_tier())],
    );
}

/// An aliased re-export binds the declaration under its own name.
#[test]
fn test_python_reexport_aliased_reexport_binds_declared_name() {
    let graph = python(
        &[
            &[
                ("pkg/__init__.py", "from .core import compute as calc\n"),
                ("pkg/core.py", "def compute():\n    return 1\n"),
            ],
            NAMESAKES,
        ],
        ("app.py", &go("from pkg import calc", "calc()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/core.py", "", "compute", import_tier())],
    );
}

/// The re-export's first module is not indexed (a compiled extension): the
/// next import binding the name decides.
#[test]
fn test_python_reexport_conditional_missing_module_binds_fallback_import() {
    let graph = python(
        &[
            &[
                (
                    "pkg/__init__.py",
                    "try:\n    from ._speedups import compute\nexcept ImportError:\n    from .core import compute\n",
                ),
                ("pkg/core.py", "def compute():\n    return 1\n"),
            ],
            NAMESAKES,
        ],
        ("app.py", &go("from pkg import compute", "compute()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/core.py", "", "compute", import_tier())],
    );
}

/// One hop: `pkg/__init__.py` → `pkg/mid.py` → `pkg/core.py` is two. The
/// second hop is not followed, so the call keeps the global tier: no edge
/// when the name is ambiguous, 0.7 when it is unique (ecp 0.14.8 alike).
#[test]
fn test_python_reexport_two_hop_chain_keeps_global_tier() {
    let chain: &[(&str, &str)] = &[
        ("pkg/__init__.py", "from .mid import compute\n"),
        ("pkg/mid.py", "from .core import compute\n"),
        ("pkg/core.py", "def compute():\n    return 1\n"),
    ];
    let app = go("from pkg import compute", "compute()");
    let ambiguous = python(
        &[chain, &[("pkg/other.py", "def compute():\n    return 2\n")]],
        ("app.py", &app),
    );
    assert_call_targets(&ambiguous, "go", &[]);
    let unique = python(&[chain], ("app.py", &app));
    assert_call_targets(
        &unique,
        "go",
        &[("pkg/core.py", "", "compute", global_tier())],
    );
}

/// The re-export's module does not declare the name: no import-tier edge;
/// the global tier keeps its previous answer (ecp 0.14.8 alike).
#[test]
fn test_python_reexport_name_missing_in_source_keeps_global_tier() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", "from .core import fetch\n"),
            ("pkg/core.py", "def other():\n    pass\n"),
            ("elsewhere/x.py", "def fetch():\n    pass\n"),
        ]],
        ("app.py", &go("from pkg import fetch", "fetch()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("elsewhere/x.py", "", "fetch", global_tier())],
    );
}

/// `a` re-exports `spin` from `b` and `b` from `a`: resolution ends, with no
/// edge, as no module declares `spin`.
#[test]
fn test_python_reexport_circular_reexport_terminates_without_edge() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", "from .b import spin\n"),
            ("pkg/b.py", "from .a import spin\n"),
        ]],
        ("app.py", &go("from pkg.a import spin", "spin()")),
    );
    assert_call_targets(&graph, "go", &[]);
}

/// Sibling language, same shape: a Kotlin file that imports `impl.helper`
/// re-exports nothing, so `import pkg.helper` resolves as on ecp 0.14.8:
/// no edge when ambiguous, the global tier when unique.
#[test]
fn test_kotlin_package_import_through_importing_file_resolves_as_before() {
    let kotlin = KotlinProvider::new().unwrap();
    let app = "package app\n\nimport pkg.helper\n\nfun go() { helper() }\n";
    let base = [
        ("src/pkg/Api.kt", "package pkg\n\nimport impl.helper\n"),
        ("src/impl/Impl.kt", "package impl\n\nfun helper() {}\n"),
        ("src/app/App.kt", app),
    ];
    let mut ambiguous = base.to_vec();
    ambiguous.push(("src/other/Other.kt", "package other\n\nfun helper() {}\n"));
    assert_call_targets(&graph_of(&kotlin, &ambiguous), "go", &[]);
    assert_call_targets(
        &graph_of(&kotlin, &base),
        "go",
        &[("src/impl/Impl.kt", "", "helper", global_tier())],
    );
}
