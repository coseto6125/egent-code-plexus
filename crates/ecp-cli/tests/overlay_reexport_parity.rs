//! A dirty caller's call through a re-export resolves in the session
//! overlay exactly as in a full reindex of the same tree
//! (FU-2026-10-15-61d656576fc8): through a Python package's
//! `from .core import compute` or `from .core import *` (the later binding
//! of `__init__.py` winning), a Python from-imported submodule (shadowed by
//! an import of the package), and a TS / JS barrel's `export *`,
//! `export { render } from`, `export { draw as render } from` and its
//! default-export forms, where a plain import of the barrel exports nothing.
//!
//! The reindex follows a re-export one hop by reading the re-exporting
//! module's imports. `graph.bin` now keeps every Python / TS / JS file's
//! imports, so the overlay reads a clean barrel's from the archive and a
//! dirty one's from its fresh parse.
//!
//! Each case runs over five dirty sets: the caller alone; caller and
//! barrel; caller and target; the barrel alone; the target alone. The
//! caller+barrel edit points the barrel at another module, so only a dirty
//! barrel's own facts give the reindex's answer. The barrel-alone and
//! target-alone edits keep the binding and shift lines: a clean caller is
//! not re-resolved, so its base edge must already agree. Each case also
//! pins the reindex's answer, so both sides cannot agree on a wrong one.

#[path = "common/parity.rs"]
mod parity;

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use parity::{callees, calls_after_edit_in, named};

/// A re-export case: the committed tree, the caller's edit adding the call,
/// the barrel edit that rebinds the name, the target edit that moves it,
/// and the callee each of those must reach.
struct Case<'a> {
    files: &'a [(&'a str, &'a str)],
    caller: (&'a str, &'a str),
    /// The barrel edited to re-export from `rebound.0`.
    barrel: (&'a str, &'a str),
    rebound: (&'a str, &'a str, u32),
    /// The barrel with only a comment line added.
    barrel_comment: (&'a str, &'a str),
    /// The target with the callee moved down one line.
    target: (&'a str, &'a str),
    /// `(file, name, line)` the call reaches before any barrel edit.
    expected: (&'a str, &'a str, u32),
}

/// `(label, committed tree, edits, the callee the call must reach)`.
type DirtySet<'a> = (
    &'a str,
    &'a [(&'a str, &'a str)],
    &'a [(&'a str, &'a str)],
    (&'a str, &'a str, u32),
);

fn check(provider: &dyn LanguageProvider, case: &Case<'_>) {
    let (file, name, line) = case.expected;
    let (rebound_file, rebound_name, rebound_line) = case.rebound;
    // The clean-caller sets start from a tree whose caller already calls.
    let calling: Vec<(&str, &str)> = case
        .files
        .iter()
        .map(|&entry| {
            if entry.0 == case.caller.0 {
                case.caller
            } else {
                entry
            }
        })
        .collect();
    let dirty_sets: [DirtySet<'_>; 5] = [
        ("caller", case.files, &[case.caller], (file, name, line)),
        (
            "caller+barrel",
            case.files,
            &[case.caller, case.barrel],
            (rebound_file, rebound_name, rebound_line),
        ),
        (
            "caller+target",
            case.files,
            &[case.caller, case.target],
            (file, name, line + 1),
        ),
        (
            "barrel",
            &calling,
            &[case.barrel_comment],
            (file, name, line),
        ),
        ("target", &calling, &[case.target], (file, name, line + 1)),
    ];
    for (label, files, edits, (file, name, line)) in dirty_sets {
        let parity = calls_after_edit_in(provider, files, edits, &named("go"));
        assert_eq!(
            callees(&parity.reindex),
            [(file, name, line, 0.95)],
            "{label}: the reindex must follow the re-export"
        );
        assert_eq!(
            parity.overlay, parity.reindex,
            "{label}: overlay vs reindex"
        );
    }
}

#[test]
fn test_overlay_python_package_reexport_matches_reindex() {
    check(
        &PythonProvider::new().unwrap(),
        &Case {
            files: &[
                ("pkg/__init__.py", "from .core import compute\n"),
                ("pkg/core.py", "def compute():\n    return 1\n"),
                ("pkg/other.py", "def compute():\n    return 2\n"),
                ("app.py", "from pkg import compute\n\ndef go():\n    pass\n"),
            ],
            caller: (
                "app.py",
                "from pkg import compute\n\ndef go():\n    return compute()\n",
            ),
            barrel: ("pkg/__init__.py", "from .other import compute\n"),
            rebound: ("pkg/other.py", "compute", 1),
            barrel_comment: ("pkg/__init__.py", "# api\nfrom .core import compute\n"),
            target: ("pkg/core.py", "\ndef compute():\n    return 1\n"),
            expected: ("pkg/core.py", "compute", 1),
        },
    );
}

#[test]
fn test_overlay_python_star_reexport_matches_reindex() {
    check(
        &PythonProvider::new().unwrap(),
        &Case {
            files: &[
                ("pkg/__init__.py", "from .core import *\n"),
                ("pkg/core.py", "def compute():\n    return 1\n"),
                ("pkg/other.py", "def compute():\n    return 2\n"),
                ("app.py", "from pkg import compute\n\ndef go():\n    pass\n"),
            ],
            caller: (
                "app.py",
                "from pkg import compute\n\ndef go():\n    return compute()\n",
            ),
            barrel: ("pkg/__init__.py", "from .other import *\n"),
            rebound: ("pkg/other.py", "compute", 1),
            barrel_comment: ("pkg/__init__.py", "# api\nfrom .core import *\n"),
            target: ("pkg/core.py", "\ndef compute():\n    return 1\n"),
            expected: ("pkg/core.py", "compute", 1),
        },
    );
}

/// `from pkg.sub import mod; mod.compute()`: the "barrel" here is the
/// package that holds the submodule; the rebinding edit gives it its own
/// `mod`, which shadows the submodule, so the call falls to the module
/// that `mod` re-exports.
#[test]
fn test_overlay_python_submodule_from_import_matches_reindex() {
    check(
        &PythonProvider::new().unwrap(),
        &Case {
            files: &[
                ("pkg/__init__.py", ""),
                ("pkg/sub/__init__.py", ""),
                ("pkg/sub/mod.py", "def compute():\n    return 3\n"),
                ("pkg/other.py", "def compute():\n    return 2\n"),
                ("app.py", "from pkg.sub import mod\n\ndef go():\n    pass\n"),
            ],
            caller: (
                "app.py",
                "from pkg.sub import mod\n\ndef go():\n    return mod.compute()\n",
            ),
            barrel: ("pkg/sub/mod.py", "from ..other import compute\n"),
            rebound: ("pkg/other.py", "compute", 1),
            barrel_comment: ("pkg/sub/__init__.py", "# package\n"),
            target: ("pkg/sub/mod.py", "\ndef compute():\n    return 3\n"),
            expected: ("pkg/sub/mod.py", "compute", 1),
        },
    );
}

fn ecmascript_case(ext: &str, barrel_line: &str, local: &str) -> [String; 10] {
    [
        format!("lib/index.{ext}"),
        format!("{barrel_line}\n"),
        format!("lib/impl.{ext}"),
        format!("export function {local}() {{ return 1; }}\n"),
        format!("lib/other.{ext}"),
        format!("export function {local}() {{ return 2; }}\n"),
        format!("app.{ext}"),
        "import { render } from './lib/index';\nexport function go() { return 0; }\n".to_string(),
        "import { render } from './lib/index';\nexport function go() { return render(); }\n"
            .to_string(),
        format!("// api\n{barrel_line}\n"),
    ]
}

/// TS and JS barrels: `export *`, a named `export { render } from`, an
/// aliased `export { draw as render } from`, and an import the barrel's own
/// clause exports under another name (`export { draw as render }`, C2).
/// Before C2 neither side followed that import: no edge.
#[test]
fn test_overlay_ecmascript_barrel_reexports_match_reindex() {
    let ts = TypeScriptProvider::new().unwrap();
    let js = JavaScriptProvider::new().unwrap();
    for (provider, ext) in [
        (&ts as &dyn LanguageProvider, "ts"),
        (&js as &dyn LanguageProvider, "js"),
    ] {
        for (barrel_line, local) in [
            ("export * from './impl';", "render"),
            ("export { render } from './impl';", "render"),
            ("export { draw as render } from './impl';", "draw"),
            (
                "import { draw } from './impl';\nexport { draw as render };",
                "draw",
            ),
        ] {
            let [index, index_src, imp, imp_src, other, other_src, app, app_src, app_calls, index_comment] =
                ecmascript_case(ext, barrel_line, local);
            let rebind = barrel_line.replace("./impl", "./other");
            let moved = format!("\n{imp_src}");
            check(
                provider,
                &Case {
                    files: &[
                        (index.as_str(), index_src.as_str()),
                        (imp.as_str(), imp_src.as_str()),
                        (other.as_str(), other_src.as_str()),
                        (app.as_str(), app_src.as_str()),
                    ],
                    caller: (app.as_str(), app_calls.as_str()),
                    barrel: (index.as_str(), rebind.as_str()),
                    rebound: (other.as_str(), local, 1),
                    barrel_comment: (index.as_str(), index_comment.as_str()),
                    target: (imp.as_str(), moved.as_str()),
                    expected: (imp.as_str(), local, 1),
                },
            );
        }
    }
}

/// A dirty module's facts replace its archived ones, empty included: the
/// barrel emptied in the same edit re-exports nothing, so neither side may
/// follow the archived `from .core import compute` (or `export *`).
#[test]
fn test_overlay_dirty_barrel_emptied_overrides_archived_imports() {
    let python = PythonProvider::new().unwrap();
    let parity = calls_after_edit_in(
        &python,
        &[
            ("pkg/__init__.py", "from .core import compute\n"),
            ("pkg/core.py", "def compute():\n    return 1\n"),
            ("pkg/other.py", "def compute():\n    return 2\n"),
            ("app.py", "from pkg import compute\n\ndef go():\n    pass\n"),
        ],
        &[
            (
                "app.py",
                "from pkg import compute\n\ndef go():\n    return compute()\n",
            ),
            ("pkg/__init__.py", ""),
        ],
        &named("go"),
    );
    assert!(
        parity.reindex.is_empty(),
        "python reindex: {:?}",
        parity.reindex
    );
    assert_eq!(parity.overlay, parity.reindex, "python");

    let ts = TypeScriptProvider::new().unwrap();
    let parity = calls_after_edit_in(
        &ts,
        &[
            ("lib/index.ts", "export * from './impl';\n"),
            ("lib/impl.ts", "export function render() { return 1; }\n"),
            ("lib/other.ts", "export function render() { return 2; }\n"),
            (
                "app.ts",
                "import { render } from './lib/index';\nexport function go() { return 0; }\n",
            ),
        ],
        &[
            (
                "app.ts",
                "import { render } from './lib/index';\nexport function go() { return render(); }\n",
            ),
            ("lib/index.ts", "export const version = 1;\n"),
        ],
        &named("go"),
    );
    assert!(
        parity.reindex.is_empty(),
        "ts reindex: {:?}",
        parity.reindex
    );
    assert_eq!(parity.overlay, parity.reindex, "ts");
}

/// The export flag flips in the dirty target: `render` turns private, so
/// the import binds nothing there, and the global tier must not bind the
/// now-private declaration of the module the import names.
#[test]
fn test_overlay_dirty_target_export_flip_matches_reindex() {
    let ts = TypeScriptProvider::new().unwrap();
    let parity = calls_after_edit_in(
        &ts,
        &[
            ("lib/impl.ts", "export function render() { return 1; }\n"),
            (
                "app.ts",
                "import { render } from './lib/impl';\nexport function go() { return 0; }\n",
            ),
        ],
        &[
            (
                "app.ts",
                "import { render } from './lib/impl';\nexport function go() { return render(); }\n",
            ),
            (
                "lib/impl.ts",
                "function render() { return 1; }\nexport function other() { return 2; }\n",
            ),
        ],
        &named("go"),
    );
    assert!(parity.reindex.is_empty(), "reindex: {:?}", parity.reindex);
    assert_eq!(parity.overlay, parity.reindex);
}

/// codex c8: `pkg/__init__.py` binds `compute` twice; the later binding
/// (the star import of `core`) wins in both. The rebinding edit swaps the
/// two modules, so only a dirty package's own import order gives the
/// reindex's answer.
#[test]
fn test_overlay_python_later_init_binding_matches_reindex() {
    check(
        &PythonProvider::new().unwrap(),
        &Case {
            files: &[
                (
                    "pkg/__init__.py",
                    "from .other import compute\nfrom .core import *\n",
                ),
                ("pkg/core.py", "def compute():\n    return 1\n"),
                ("pkg/other.py", "def compute():\n    return 2\n"),
                ("app.py", "from pkg import compute\n\ndef go():\n    pass\n"),
            ],
            caller: (
                "app.py",
                "from pkg import compute\n\ndef go():\n    return compute()\n",
            ),
            barrel: (
                "pkg/__init__.py",
                "from .core import compute\nfrom .other import *\n",
            ),
            rebound: ("pkg/other.py", "compute", 1),
            barrel_comment: (
                "pkg/__init__.py",
                "# api\nfrom .other import compute\nfrom .core import *\n",
            ),
            target: ("pkg/core.py", "\ndef compute():\n    return 1\n"),
            expected: ("pkg/core.py", "compute", 1),
        },
    );
}

/// codex c9: a package `__init__.py` that binds `mod` by import shadows the
/// submodule `pkg/mod.py` in both, whether the package is clean (archived
/// imports) or dirty (its fresh parse adds the import): no edge.
#[test]
fn test_overlay_python_submodule_shadowed_by_init_import_matches_reindex() {
    let python = PythonProvider::new().unwrap();
    let other = "class mod:\n    @staticmethod\n    def compute():\n        return 4\n";
    let shadowing = "from .other import mod\n";
    let caller = (
        "app.py",
        "from pkg import mod\n\ndef go():\n    return mod.compute()\n",
    );
    for (label, init, edits) in [
        ("clean package", shadowing, vec![caller]),
        (
            "dirty package",
            "",
            vec![caller, ("pkg/__init__.py", shadowing)],
        ),
    ] {
        let parity = calls_after_edit_in(
            &python,
            &[
                ("pkg/__init__.py", init),
                ("pkg/other.py", other),
                ("pkg/mod.py", "def compute():\n    return 3\n"),
                ("app.py", "from pkg import mod\n\ndef go():\n    pass\n"),
            ],
            &edits,
            &named("go"),
        );
        assert!(
            parity.reindex.is_empty(),
            "{label}: reindex bound {:?}",
            parity.reindex
        );
        assert_eq!(
            parity.overlay, parity.reindex,
            "{label}: overlay vs reindex"
        );
    }
}

/// codex c3 / R1 F5: the barrel's plain `import { render } from './legacy'`
/// exports nothing, so the `export *` of `impl` binds `render` in both.
#[test]
fn test_overlay_ecmascript_plain_import_does_not_shadow_star_matches_reindex() {
    let ts = TypeScriptProvider::new().unwrap();
    let js = JavaScriptProvider::new().unwrap();
    for (provider, ext) in [
        (&ts as &dyn LanguageProvider, "ts"),
        (&js as &dyn LanguageProvider, "js"),
    ] {
        let [index, _, imp, imp_src, other, other_src, app, app_src, app_calls, _] =
            ecmascript_case(ext, "", "render");
        let legacy = format!("lib/legacy.{ext}");
        let barrel = "import { render } from './legacy';\nexport * from './impl';\n";
        let rebind = barrel.replace("./impl", "./other");
        let comment = format!("// api\n{barrel}");
        let moved = format!("\n{imp_src}");
        check(
            provider,
            &Case {
                files: &[
                    (index.as_str(), barrel),
                    (imp.as_str(), imp_src.as_str()),
                    (other.as_str(), other_src.as_str()),
                    (legacy.as_str(), "export function render() { return 3; }\n"),
                    (app.as_str(), app_src.as_str()),
                ],
                caller: (app.as_str(), app_calls.as_str()),
                barrel: (index.as_str(), rebind.as_str()),
                rebound: (other.as_str(), "render", 1),
                barrel_comment: (index.as_str(), comment.as_str()),
                target: (imp.as_str(), moved.as_str()),
                expected: (imp.as_str(), "render", 1),
            },
        );
    }
}

/// R1 F4: the default-export barrel forms bind `impl`'s default export in
/// both: `export { default as render } from`, a default import the barrel
/// exports, and a default import through `export { default } from`.
#[test]
fn test_overlay_ecmascript_default_export_barrels_match_reindex() {
    let ts = TypeScriptProvider::new().unwrap();
    let js = JavaScriptProvider::new().unwrap();
    for (provider, ext) in [
        (&ts as &dyn LanguageProvider, "ts"),
        (&js as &dyn LanguageProvider, "js"),
    ] {
        let index = format!("lib/index.{ext}");
        let imp = format!("lib/impl.{ext}");
        let other = format!("lib/other.{ext}");
        let app = format!("app.{ext}");
        let imp_src = "export default function render() { return 1; }\n";
        let moved = format!("\n{imp_src}");
        for (barrel, import) in [
            (
                "export { default as render } from './impl';\n",
                "import { render } from './lib/index';",
            ),
            (
                "import render from './impl';\nexport { render };\n",
                "import { render } from './lib/index';",
            ),
            (
                "export { default } from './impl';\n",
                "import render from './lib/index';",
            ),
        ] {
            let app_src = format!("{import}\nexport function go() {{ return 0; }}\n");
            let app_calls = format!("{import}\nexport function go() {{ return render(); }}\n");
            let rebind = barrel.replace("./impl", "./other");
            let comment = format!("// api\n{barrel}");
            check(
                provider,
                &Case {
                    files: &[
                        (index.as_str(), barrel),
                        (imp.as_str(), imp_src),
                        (
                            other.as_str(),
                            "export default function render() { return 2; }\n",
                        ),
                        (app.as_str(), app_src.as_str()),
                    ],
                    caller: (app.as_str(), app_calls.as_str()),
                    barrel: (index.as_str(), rebind.as_str()),
                    rebound: (other.as_str(), "render", 1),
                    barrel_comment: (index.as_str(), comment.as_str()),
                    target: (imp.as_str(), moved.as_str()),
                    expected: (imp.as_str(), "render", 1),
                },
            );
        }
    }
}
