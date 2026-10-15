//! FU-2026-10-05-0ee319339782: a Python call through a package re-export
//! (`pkg/__init__.py`: `from .core import compute`) binds through the import
//! tier (0.95), following the re-export one hop. Before the fix these calls
//! kept the global tier: 0.7 for a unique name, no edge for an ambiguous one.
//!
//! FU-2026-10-15-2c1620b091e0: a submodule bound by a from-import
//! (`from pkg.sub import mod; mod.compute()`) binds `pkg/sub/mod.py` the same
//! way. FU-2026-10-15-73b85f10c8fa: a star re-export (`from .core import *`)
//! is followed one hop like an explicit one.
//!
//! The guard tests pin what must not change: a second hop is not followed,
//! a cycle ends, a missing re-exported name keeps the global tier, a module
//! attribute shadows a submodule, a from-imported object resolves as before,
//! and Kotlin's and TypeScript's same shapes resolve as before.
//! Their expected values are what ecp 0.14.8 produced on the same fixtures.
mod instantiation_calls_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
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

/// `from pkg.sub import mod`: `pkg/sub/__init__.py` declares no `mod`, so the
/// import binds the submodule, as `import pkg.sub.mod as mod` would. ecp
/// 0.14.8 drew no edge for any of the four shapes.
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

/// A unique member: ecp 0.14.8 bound it at the global tier (0.7).
#[test]
fn test_python_submodule_from_import_unique_name_binds_import_tier() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", ""),
            ("pkg/sub/__init__.py", ""),
            ("pkg/sub/mod.py", "def compute():\n    return 3\n"),
        ]],
        ("app.py", &go("from pkg.sub import mod", "mod.compute()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/sub/mod.py", "", "compute", import_tier())],
    );
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

/// `from m import obj` binds an instance: the method call resolves as on
/// ecp 0.14.8, no edge when ambiguous, the global tier when unique.
#[test]
fn test_python_from_import_object_member_call_resolves_as_before() {
    let module: &[(&str, &str)] = &[(
        "m.py",
        "class Thing:\n    def run(self):\n        return 1\n\nobj = Thing()\n",
    )];
    let app = go("from m import obj", "obj.run()");
    let ambiguous = python(
        &[module, &[("other.py", "def run():\n    return 2\n")]],
        ("app.py", &app),
    );
    assert_call_targets(&ambiguous, "go", &[]);
    let unique = python(&[module], ("app.py", &app));
    assert_call_targets(&unique, "go", &[("m.py", "Thing", "run", global_tier())]);
}

/// `ghost` is neither declared by `pkg.sub` nor a submodule file: the call
/// keeps the global tier (ecp 0.14.8 alike).
#[test]
fn test_python_from_import_name_neither_declared_nor_submodule_keeps_global_tier() {
    let package: &[(&str, &str)] = &[
        ("pkg/__init__.py", ""),
        ("pkg/sub/__init__.py", ""),
        ("pkg/x.py", "def compute():\n    return 5\n"),
    ];
    let app = go("from pkg.sub import ghost", "ghost.compute()");
    let unique = python(&[package], ("app.py", &app));
    assert_call_targets(&unique, "go", &[("pkg/x.py", "", "compute", global_tier())]);
    let ambiguous = python(
        &[package, &[("pkg/y.py", "def compute():\n    return 6\n")]],
        ("app.py", &app),
    );
    assert_call_targets(&ambiguous, "go", &[]);
}

/// The submodule exists but does not declare the member: the call keeps the
/// global tier (ecp 0.14.8 alike).
#[test]
fn test_python_submodule_from_import_member_missing_keeps_global_tier() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", ""),
            ("pkg/sub/__init__.py", ""),
            ("pkg/sub/mod.py", "def other():\n    return 3\n"),
            ("pkg/x.py", "def compute():\n    return 5\n"),
        ]],
        ("app.py", &go("from pkg.sub import mod", "mod.compute()")),
    );
    assert_call_targets(&graph, "go", &[("pkg/x.py", "", "compute", global_tier())]);
}

/// A namespace package (no `__init__.py`) holds submodules too. ecp 0.14.8
/// drew no edge.
#[test]
fn test_python_submodule_from_namespace_package_binds_submodule() {
    let graph = python(
        &[&[
            ("pkg/sub/mod.py", "def compute():\n    return 3\n"),
            ("pkg/other.py", "def compute():\n    return 2\n"),
        ]],
        ("app.py", &go("from pkg.sub import mod", "mod.compute()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/sub/mod.py", "", "compute", import_tier())],
    );
}

/// The same from-import twice binds once. ecp 0.14.8 drew no edge.
#[test]
fn test_python_submodule_from_import_listed_twice_binds_submodule() {
    let graph = python(
        &[PACKAGE, NAMESAKES],
        (
            "app.py",
            "from pkg.sub import mod\nfrom pkg.sub import mod\n\ndef go():\n    return mod.compute()\n",
        ),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/sub/mod.py", "", "compute", import_tier())],
    );
}

/// The parser emits `from m import *` as a `*` import with no local binding,
/// the shape TS / JS `export * from` has.
#[test]
fn test_python_parse_star_import_emits_unbound_star_import() {
    let graph = PythonProvider::new()
        .unwrap()
        .parse_file(
            std::path::Path::new("pkg/__init__.py"),
            b"from .core import *\nfrom pkg.util import *\n",
        )
        .unwrap();
    let stars: Vec<(&str, &str, Option<&str>)> = graph
        .imports
        .iter()
        .map(|import| {
            (
                import.source.as_str(),
                import.imported_name.as_str(),
                import.alias.as_deref(),
            )
        })
        .collect();
    assert_eq!(stars, [(".core", "*", None), ("pkg/util", "*", None)]);
}

/// `pkg/__init__.py`: `from .core import *`. ecp 0.14.8 drew no edge.
#[test]
fn test_python_star_reexport_ambiguous_name_binds_import_tier() {
    let package: &[(&str, &str)] = &[
        ("pkg/__init__.py", "from .core import *\n"),
        ("pkg/core.py", "def compute():\n    return 1\n"),
        ("pkg/other.py", "def compute():\n    return 2\n"),
    ];
    for (import, call) in [
        ("from pkg import compute", "compute()"),
        ("import pkg", "pkg.compute()"),
    ] {
        let graph = python(&[package], ("app.py", &go(import, call)));
        assert_call_targets(
            &graph,
            "go",
            &[("pkg/core.py", "", "compute", import_tier())],
        );
    }
}

/// A unique name: ecp 0.14.8 bound it at the global tier (0.7).
#[test]
fn test_python_star_reexport_unique_name_binds_import_tier() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", "from .core import *\n"),
            ("pkg/core.py", "def compute():\n    return 1\n"),
        ]],
        ("app.py", &go("from pkg import compute", "compute()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/core.py", "", "compute", import_tier())],
    );
}

/// Two star sources both declare `compute`: neither binds, and the global
/// tier drops the ambiguous name (no edge on ecp 0.14.8).
#[test]
fn test_python_star_reexport_two_sources_define_name_binds_nothing() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", "from .a import *\nfrom .b import *\n"),
            ("pkg/a.py", "def compute():\n    return 1\n"),
            ("pkg/b.py", "def compute():\n    return 2\n"),
        ]],
        ("app.py", &go("from pkg import compute", "compute()")),
    );
    assert_call_targets(&graph, "go", &[]);
}

/// Of two star sources only one declares `compute`: it binds. ecp 0.14.8
/// drew no edge.
#[test]
fn test_python_star_reexport_one_of_two_sources_defines_name_binds_it() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", "from .a import *\nfrom .b import *\n"),
            ("pkg/a.py", "def compute():\n    return 1\n"),
            ("pkg/b.py", "def other():\n    return 2\n"),
            ("pkg/other.py", "def compute():\n    return 2\n"),
        ]],
        ("app.py", &go("from pkg import compute", "compute()")),
    );
    assert_call_targets(&graph, "go", &[("pkg/a.py", "", "compute", import_tier())]);
}

/// An explicit re-export shadows every star re-export, so the star import
/// leaves the explicit hop's answer (`pkg/b.py`, 0.95, before the star was
/// followed) as it was. ecp 0.14.8, without the hop, drew no edge.
#[test]
fn test_python_star_reexport_explicit_import_shadows_star() {
    let graph = python(
        &[&[
            (
                "pkg/__init__.py",
                "from .a import *\nfrom .b import compute\n",
            ),
            ("pkg/a.py", "def compute():\n    return 1\n"),
            ("pkg/b.py", "def compute():\n    return 2\n"),
        ]],
        ("app.py", &go("from pkg import compute", "compute()")),
    );
    assert_call_targets(&graph, "go", &[("pkg/b.py", "", "compute", import_tier())]);
}

/// One hop: `pkg/__init__.py` → `pkg/mid.py` → `pkg/core.py`, all by star,
/// is two. The call keeps the global tier: no edge when ambiguous, 0.7 when
/// unique (ecp 0.14.8 alike).
#[test]
fn test_python_star_reexport_two_hop_chain_keeps_global_tier() {
    let chain: &[(&str, &str)] = &[
        ("pkg/__init__.py", "from .mid import *\n"),
        ("pkg/mid.py", "from .core import *\n"),
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

/// A star import re-exports from any module, not only a package
/// `__init__.py`, as an explicit re-export does. ecp 0.14.8 drew no edge.
#[test]
fn test_python_star_reexport_in_non_init_module_binds_import_tier() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", ""),
            ("pkg/hub.py", "from .core import *\n"),
            ("pkg/core.py", "def compute():\n    return 1\n"),
            ("pkg/other.py", "def compute():\n    return 2\n"),
        ]],
        ("app.py", &go("from pkg.hub import compute", "compute()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/core.py", "", "compute", import_tier())],
    );
}

/// The same star import twice binds once. ecp 0.14.8 drew no edge.
#[test]
fn test_python_star_reexport_listed_twice_binds_import_tier() {
    let graph = python(
        &[&[
            (
                "pkg/__init__.py",
                "from .core import *\nfrom .core import *\n",
            ),
            ("pkg/core.py", "def compute():\n    return 1\n"),
            ("pkg/other.py", "def compute():\n    return 2\n"),
        ]],
        ("app.py", &go("from pkg import compute", "compute()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/core.py", "", "compute", import_tier())],
    );
}

/// A star source with `__all__` exports only the names it lists, which the
/// index does not read: the star is not followed, and the call keeps the
/// global tier (ecp 0.14.8 alike), whether `__all__` lists the name or not.
#[test]
fn test_python_star_reexport_source_with_dunder_all_keeps_global_tier() {
    let app = go("from pkg import compute", "compute()");
    let namesake: &[(&str, &str)] = &[("pkg/other.py", "def compute():\n    return 2\n")];
    for all in ["[\"compute\"]", "[\"other\"]"] {
        let core =
            format!("__all__ = {all}\n\ndef compute():\n    return 1\n\ndef other():\n    pass\n");
        let package: &[(&str, &str)] = &[
            ("pkg/__init__.py", "from .core import *\n"),
            ("pkg/core.py", core.as_str()),
        ];
        let ambiguous = python(&[package, namesake], ("app.py", &app));
        assert_call_targets(&ambiguous, "go", &[]);
    }
    let unique = python(
        &[&[
            ("pkg/__init__.py", "from .core import *\n"),
            (
                "pkg/core.py",
                "__all__ = [\"compute\"]\n\ndef compute():\n    return 1\n",
            ),
        ]],
        ("app.py", &app),
    );
    assert_call_targets(
        &unique,
        "go",
        &[("pkg/core.py", "", "compute", global_tier())],
    );
}

/// A star import without `__all__` skips `_`-prefixed names: the call keeps
/// the global tier (ecp 0.14.8 alike).
#[test]
fn test_python_star_reexport_private_name_keeps_global_tier() {
    let graph = python(
        &[&[
            ("pkg/__init__.py", "from .core import *\n"),
            ("pkg/core.py", "def _helper():\n    return 1\n"),
        ]],
        ("app.py", &go("from pkg import _helper", "_helper()")),
    );
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/core.py", "", "_helper", global_tier())],
    );
}

/// The caller's own star import binds no name in the import tier: the call
/// resolves as on ecp 0.14.8, no edge when ambiguous, 0.7 when unique.
#[test]
fn test_python_star_import_in_caller_resolves_as_before() {
    let package: &[(&str, &str)] = &[
        ("pkg/__init__.py", ""),
        ("pkg/core.py", "def compute():\n    return 1\n"),
    ];
    let app = go("from pkg.core import *", "compute()");
    let ambiguous = python(
        &[
            package,
            &[("pkg/other.py", "def compute():\n    return 2\n")],
        ],
        ("app.py", &app),
    );
    assert_call_targets(&ambiguous, "go", &[]);
    let unique = python(&[package], ("app.py", &app));
    assert_call_targets(
        &unique,
        "go",
        &[("pkg/core.py", "", "compute", global_tier())],
    );
}

/// Sibling language, same shape: a TypeScript member call on an imported
/// object resolves as on ecp 0.14.8 (no edge, unique or ambiguous).
#[test]
fn test_typescript_imported_object_member_call_resolves_as_before() {
    let ts = TypeScriptProvider::new().unwrap();
    let base = [
        (
            "m.ts",
            "export class Thing { run() { return 1; } }\nexport const obj = new Thing();\n",
        ),
        (
            "app.ts",
            "import { obj } from './m';\nexport function go() { return obj.run(); }\n",
        ),
    ];
    assert_call_targets(&graph_of(&ts, &base), "go", &[]);
    let mut ambiguous = base.to_vec();
    ambiguous.push(("other.ts", "export function run() { return 2; }\n"));
    assert_call_targets(&graph_of(&ts, &ambiguous), "go", &[]);
}
