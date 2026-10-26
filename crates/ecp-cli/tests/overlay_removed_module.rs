//! A module file the worktree removed is gone for the overlay's module
//! lookups (`ImportScope`, the Rust module layout and file stems): a dirty
//! importer binds no call into it, as a fresh full index of the edited tree
//! binds none. Each case deletes the callee's file and edits its importer,
//! through the production index, fragment write and read
//! (`common/strict_parity.rs`).

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::session::merged::MergedGraph;
use strict_parity::{snapshot, Edit, Scenario, Snapshot};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// `(case, removed callee file, importer file, files)`.
type Case = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const CASES: &[Case] = &[
    (
        "python",
        "pkg/gone.py",
        "main.py",
        &[
            ("pkg/__init__.py", "def init():\n    pass\n"),
            ("pkg/gone.py", "def gone():\n    pass\n"),
            (
                "main.py",
                "from pkg.gone import gone\n\n\ndef main():\n    gone()\n",
            ),
        ],
    ),
    (
        "typescript",
        "src/lib.ts",
        "src/main.ts",
        &[
            ("src/lib.ts", "export function helper(): number {\n  return 1;\n}\n"),
            (
                "src/main.ts",
                "import { helper } from './lib';\n\nexport function run(): number {\n  return helper();\n}\n",
            ),
        ],
    ),
    (
        "rust",
        "src/util.rs",
        "src/lib.rs",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"acme\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "mod util;\n\npub fn run() -> i32 {\n    util::helper()\n}\n",
            ),
            ("src/util.rs", "pub fn helper() -> i32 {\n    1\n}\n"),
        ],
    ),
];

/// The `Calls` edge lines of `snap` whose target lies in `file`.
fn calls_into(snap: &Snapshot, file: &str) -> Vec<String> {
    let target = format!(" {file}:");
    snap.edge
        .iter()
        .filter(|line| {
            line.starts_with("Calls ")
                && line
                    .split_once(" -> ")
                    .is_some_and(|(_, to)| to.contains(&target))
        })
        .cloned()
        .collect()
}

#[test]
fn test_overlay_view_removed_module_dirty_importer_binds_no_call_into_it() {
    for &(case, removed, importer, files) in CASES {
        let source = files
            .iter()
            .find(|(path, _)| *path == importer)
            .map(|(_, source)| *source)
            .expect("the importer is one of the files");
        let edited = format!("\n{source}");
        let observed = strict_parity::run(&Scenario {
            name: "",
            owner: OWNER,
            files,
            edits: &[Edit::Delete(removed), Edit::Write(importer, &edited)],
        });
        let base = snapshot(&MergedGraph::new(observed.base(), None));
        assert!(
            !calls_into(&base, removed).is_empty(),
            "{case}: the base index binds no call into {removed}, so the test proves nothing"
        );
        let (full, overlay) = observed.snapshots();
        assert_eq!(
            calls_into(&full, removed),
            Vec::<String>::new(),
            "{case}: full"
        );
        assert_eq!(
            calls_into(&overlay, removed),
            Vec::<String>::new(),
            "{case}: the overlay binds a call into the removed {removed}"
        );
    }
}
