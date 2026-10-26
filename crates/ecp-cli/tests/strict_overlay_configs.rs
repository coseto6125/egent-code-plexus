//! Strict overlay parity for dirty resolution configs (R-A4d): an edited
//! tsconfig, `go.mod`, `pyproject.toml`, `.csproj` or `composer.json` makes
//! the overlay view equal a fresh full index for the clean files it
//! rescopes. Each scenario runs the production index, fragment write,
//! closure refresh (with the dirty configs) and read
//! (`common/strict_parity.rs`); a scenario with no baseline file under
//! `tests/baselines/strict_parity/` expects no difference.
//!
//! Language coverage: TypeScript, Go, Python, C# and PHP, the ecosystems
//! whose configs the C2 scope table reads. Cargo.toml is kept as indexed
//! (its rows come from the Rust module tree, not a config reader), and the
//! other mainstream languages read no resolution config, so a dirty config
//! changes nothing for them.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::session::closure::{CLOSURE_LIMIT, REBUILT_RELS};
use ecp_core::session::merged::MergedGraph;
use strict_parity::{check, snapshot, Edit, Scenario, Snapshot};

const LANE_R_A4D: &str = "R-A4d (dirty resolution configs)";

const TSCONFIG_A: &str = r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/a/*"]}}}"#;
const TSCONFIG_B: &str = r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/b/*"]}}}"#;
const UTIL_A: &str = "export function util(): number {\n  return 1;\n}\n";
const UTIL_B: &str = "export function util(): number {\n  return 2;\n}\n";
const TS_MAIN: &str =
    "import { util } from '@/util';\n\nexport function run(): number {\n  return util();\n}\n";

const TS_FILES: [(&str, &str); 4] = [
    ("tsconfig.json", TSCONFIG_A),
    ("src/a/util.ts", UTIL_A),
    ("src/b/util.ts", UTIL_B),
    ("src/main.ts", TS_MAIN),
];

/// Some `rel` edge runs from a node of the file `from` to a node of the
/// file `to`.
fn binds(snap: &Snapshot, rel: &str, from: &str, to: &str) -> bool {
    snap.edge.iter().any(|line| {
        line.strip_prefix(rel)
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|rest| rest.split_once(" -> "))
            .is_some_and(|(source, target)| {
                source.contains(&format!(" {from}:")) && target.contains(&format!(" {to}:"))
            })
    })
}

/// The tsconfig remaps `@/*` from `src/a/*` to `src/b/*`: the clean
/// `main.ts` that imports `@/util` binds `src/b/util.ts`, as in a fresh
/// index.
#[test]
fn test_overlay_view_tsconfig_alias_change_rebinds_clean_caller() {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: LANE_R_A4D,
        files: &TS_FILES,
        edits: &[Edit::Write("tsconfig.json", TSCONFIG_B)],
    });
    let (full, overlay) = observed.snapshots();
    assert!(
        binds(&full, "Calls", "src/main.ts", "src/b/util.ts"),
        "the fresh index binds the new target: {:#?}",
        full.edge
    );
    assert_eq!(observed.configs.applied, vec!["tsconfig.json".to_string()]);
    assert!(observed.configs.kept.is_empty());
    assert!(
        binds(&overlay, "Calls", "src/main.ts", "src/b/util.ts"),
        "{:#?}",
        overlay.edge
    );
    assert!(
        !binds(&overlay, "Calls", "src/main.ts", "src/a/util.ts"),
        "{:#?}",
        overlay.edge
    );
    let lines = observed.lines();
    assert!(lines.is_empty(), "{lines:#?}");
}

/// The root tsconfig is deleted: the clean caller's `@/util` resolves as
/// a fresh index of the tree without it resolves it.
#[test]
fn test_check_tsconfig_deleted_matches_baseline() {
    check(&Scenario {
        name: "config_tsconfig_deleted",
        owner: LANE_R_A4D,
        files: &TS_FILES,
        edits: &[Edit::Delete("tsconfig.json")],
    });
}

/// The tsconfig edit and a dirty source in the same scope: the dirty file
/// and the rescoped clean caller both resolve with the new alias.
#[test]
fn test_check_tsconfig_and_dirty_source_same_scope_matches_baseline() {
    check(&Scenario {
        name: "config_tsconfig_and_source",
        owner: LANE_R_A4D,
        files: &TS_FILES,
        edits: &[
            Edit::Write("tsconfig.json", TSCONFIG_B),
            Edit::Write(
                "src/b/util.ts",
                "export function util(): number {\n  return 3;\n}\n\nexport function more(): number {\n  return util();\n}\n",
            ),
        ],
    });
}

/// Two dirty tsconfigs in nested scopes, each remapping its own alias: the
/// callers of both scopes rebind.
#[test]
fn test_check_tsconfig_nested_scopes_both_dirty_matches_baseline() {
    let web = |dir: &str| {
        format!(r#"{{"compilerOptions":{{"baseUrl":".","paths":{{"~/*":["lib/{dir}/*"]}}}}}}"#)
    };
    let (web_x, web_y) = (web("x"), web("y"));
    check(&Scenario {
        name: "config_tsconfig_nested",
        owner: LANE_R_A4D,
        files: &[
            TS_FILES[0],
            TS_FILES[1],
            TS_FILES[2],
            TS_FILES[3],
            ("packages/web/package.json", r#"{"name":"web"}"#),
            ("packages/web/tsconfig.json", web_x.as_str()),
            ("packages/web/lib/x/util.ts", UTIL_A),
            ("packages/web/lib/y/util.ts", UTIL_B),
            (
                "packages/web/main.ts",
                "import { util } from '~/util';\n\nexport function web(): number {\n  return util();\n}\n",
            ),
        ],
        edits: &[
            Edit::Write("tsconfig.json", TSCONFIG_B),
            Edit::Write("packages/web/tsconfig.json", web_y.as_str()),
        ],
    });
}

/// A tsconfig the parser rejects keeps its archived rows: the view names
/// it with every rebuilt kind, and the clean caller keeps its indexed
/// binding instead of a guess.
#[test]
fn test_overlay_view_malformed_tsconfig_kept_and_named() {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: LANE_R_A4D,
        files: &TS_FILES,
        edits: &[
            Edit::Write("tsconfig.json", "{ \"compilerOptions\": "),
            Edit::Write("src/other.ts", "export const other = 1;\n"),
        ],
    });
    assert_eq!(observed.stale.configs, vec!["tsconfig.json".to_string()]);
    assert_eq!(observed.configs.kept, vec!["tsconfig.json".to_string()]);
    assert!(observed.configs.applied.is_empty());
    for rel in REBUILT_RELS {
        assert!(
            observed.stale.kinds.iter().any(|k| k == rel.as_str()),
            "{rel:?} in {:?}",
            observed.stale.kinds
        );
    }
    let view = observed.view().expect("a dirty view");
    assert_eq!(view.stale_closure(), &observed.stale, "the view carries it");
    let overlay = snapshot(&MergedGraph::new(observed.base(), Some(&view)));
    assert!(
        binds(&overlay, "Calls", "src/main.ts", "src/a/util.ts"),
        "the archived binding stands: {:#?}",
        overlay.edge
    );
}

/// A root tsconfig edit rescopes every TS file: past `CLOSURE_LIMIT` the
/// view re-resolves none and names every rebuilt kind.
#[test]
fn test_overlay_view_tsconfig_rescope_past_limit_is_unbounded() {
    let callers: Vec<(String, &str)> = (0..=CLOSURE_LIMIT)
        .map(|i| (format!("src/c{i:03}.ts"), TS_MAIN))
        .collect();
    let mut files: Vec<(&str, &str)> = TS_FILES.to_vec();
    files.extend(
        callers
            .iter()
            .map(|(path, source)| (path.as_str(), *source)),
    );
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: LANE_R_A4D,
        files: &files,
        edits: &[Edit::Write("tsconfig.json", TSCONFIG_B)],
    });
    assert!(
        observed
            .stale
            .unbounded
            .is_some_and(|n| n as usize > CLOSURE_LIMIT),
        "{:?}",
        observed.stale
    );
    for rel in REBUILT_RELS {
        assert!(
            observed.stale.kinds.iter().any(|k| k == rel.as_str()),
            "{rel:?} in {:?}",
            observed.stale.kinds
        );
    }
    assert!(
        observed
            .inputs
            .iter()
            .all(|input| !input.rel_path.starts_with("src/c")),
        "no clean file re-resolved"
    );
}

/// The `go.mod` module path changes: the clean importer of the new path
/// binds, the importer of the old one no longer does, and a namesake in
/// another package stays apart.
#[test]
fn test_check_go_mod_module_path_changed_matches_baseline() {
    check(&Scenario {
        name: "config_go_mod_module_path",
        owner: LANE_R_A4D,
        files: &[
            ("go.mod", "module example.com/old\n\ngo 1.21\n"),
            ("util/util.go", "package util\n\nfunc Help() int {\n\treturn 1\n}\n"),
            ("other/other.go", "package other\n\nfunc Help() int {\n\treturn 2\n}\n"),
            (
                "main.go",
                "package main\n\nimport \"example.com/old/util\"\n\nfunc main() {\n\tutil.Help()\n}\n",
            ),
            (
                "cmd/tool/tool.go",
                "package main\n\nimport \"example.com/new/util\"\n\nfunc run() {\n\tutil.Help()\n}\n",
            ),
        ],
        edits: &[Edit::Write("go.mod", "module example.com/new\n\ngo 1.21\n")],
    });
}

/// The `pyproject.toml` package root moves from `src` to `lib`: the clean
/// `from pkg.util import helper` binds the copy under the new root.
#[test]
fn test_check_pyproject_package_root_moved_matches_baseline() {
    let pyproject = |root: &str| format!("[tool.setuptools.packages.find]\nwhere = [\"{root}\"]\n");
    let (before, after) = (pyproject("src"), pyproject("lib"));
    check(&Scenario {
        name: "config_pyproject_root_moved",
        owner: LANE_R_A4D,
        files: &[
            ("pyproject.toml", before.as_str()),
            ("src/pkg/__init__.py", ""),
            ("src/pkg/util.py", "def helper():\n    return 1\n"),
            ("lib/pkg/__init__.py", ""),
            ("lib/pkg/util.py", "def helper():\n    return 2\n"),
            (
                "app.py",
                "from pkg.util import helper\n\n\ndef run():\n    return helper()\n",
            ),
        ],
        edits: &[Edit::Write("pyproject.toml", after.as_str())],
    });
}

/// A `.csproj` gains a `ProjectReference`: the project's rows and the
/// repo-wide entries are re-read, and the clean caller resolves as in a
/// fresh index.
#[test]
fn test_check_csproj_project_reference_added_matches_baseline() {
    const PROJECT: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n</Project>\n";
    check(&Scenario {
        name: "config_csproj_reference_added",
        owner: LANE_R_A4D,
        files: &[
            ("App/App.csproj", PROJECT),
            ("Lib/Lib.csproj", PROJECT),
            (
                "Lib/Util.cs",
                "namespace Lib\n{\n    public static class Util\n    {\n        public static int Help()\n        {\n            return 1;\n        }\n    }\n}\n",
            ),
            (
                "App/Program.cs",
                "using Lib;\n\nnamespace App\n{\n    public static class Program\n    {\n        public static int Run()\n        {\n            return Util.Help();\n        }\n    }\n}\n",
            ),
        ],
        edits: &[Edit::Write(
            "App/App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <ProjectReference Include=\"../Lib/Lib.csproj\" />\n  </ItemGroup>\n</Project>\n",
        )],
    });
}

/// A `composer.json` PSR-4 prefix maps to another directory: the clean
/// caller of `App\Util` resolves as in a fresh index.
#[test]
fn test_check_composer_psr4_remapped_matches_baseline() {
    let composer = |dir: &str| format!(r#"{{"autoload":{{"psr-4":{{"App\\":"{dir}/"}}}}}}"#);
    let (before, after) = (composer("src"), composer("lib"));
    let util = |n: u32| {
        format!(
            "<?php\n\nnamespace App;\n\nclass Util\n{{\n    public static function help()\n    {{\n        return {n};\n    }}\n}}\n"
        )
    };
    let (util_src, util_lib) = (util(1), util(2));
    check(&Scenario {
        name: "config_composer_psr4_remapped",
        owner: LANE_R_A4D,
        files: &[
            ("composer.json", before.as_str()),
            ("src/Util.php", util_src.as_str()),
            ("lib/Util.php", util_lib.as_str()),
            (
                "main.php",
                "<?php\n\nuse App\\Util;\n\nfunction run()\n{\n    return Util::help();\n}\n",
            ),
        ],
        edits: &[Edit::Write("composer.json", after.as_str())],
    });
}
