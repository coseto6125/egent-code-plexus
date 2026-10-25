//! Slice A of the owner-split design (`docs/specs/2026-10-23-owner-split-design.md`
//! §6.2): one `assign_file` numbering per file on every path that stores or
//! compares a declaration's uid. Four sources must agree node by node:
//!
//! | source | read through |
//! |---|---|
//! | full build | `graph.bin` of `GraphBuilder` |
//! | overlay view | `OverlayView::build` over the dirty parse |
//! | overlay reader | `OverlayView::build` over `decode_view_inputs` of the written fragments |
//! | incremental | `incremental::node_uids_for` |
//!
//! The 13 method fixtures are the ones `ecp-analyzer/tests/overload_members.rs`
//! pins: each parser emits two `save` members of type `K`. C has no methods,
//! so its fixture holds an `#ifdef` redefinition, which the index tombstones.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::{
    c::parser::CProvider, c_sharp::parser::CSharpProvider, cpp::parser::CppProvider,
    dart::parser::DartProvider, go::parser::GoProvider, java::parser::JavaProvider,
    javascript::parser::JavaScriptProvider, kotlin::parser::KotlinProvider,
    php::parser::PhpProvider, python::PythonProvider, ruby::parser::RubyProvider,
    rust::parser::RustProvider, swift::parser::SwiftProvider, typescript::TypeScriptProvider,
};
use ecp_cli::incremental::{
    build_old_hash_map, build_old_import_map, node_uids_for, symbol_hash_diff, FileDiffDecision,
};
use ecp_cli::session::overlay_reader::{accepted_fragments, decode_view_inputs};
use ecp_cli::session::overlay_writer::write_dirty_fragments_from_graphs;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::routing::SizeCap;
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, ZeroCopyGraph};
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use ecp_core::session::{layout, DirtyEvents, SessionMeta};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

fn parse(provider: &dyn LanguageProvider, path: &str, source: &str) -> LocalGraph {
    provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap()
}

fn full_build(local: LocalGraph) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn archive(graph: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(graph).unwrap()
}

fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap()
}

/// The uid `graph` stores for each node of `local`, in node order.
fn stored_uids(graph: &ArchivedZeroCopyGraph, local: &LocalGraph) -> Vec<u64> {
    base_indices(graph, local)
        .into_iter()
        .map(|i| graph.nodes[i as usize].uid.to_native())
        .collect()
}

/// The node of `graph` that holds each node of `local`, in node order: the
/// node of equal kind and span, the k-th one for the k-th repeat of that key.
fn base_indices(graph: &ArchivedZeroCopyGraph, local: &LocalGraph) -> Vec<u32> {
    let mut used: FxHashSet<usize> = FxHashSet::default();
    local
        .nodes
        .iter()
        .map(|raw| {
            let (i, _) = graph
                .nodes
                .iter()
                .enumerate()
                .find(|(i, n)| {
                    !used.contains(i)
                        && NodeKind::from(&n.kind) == raw.kind
                        && (
                            n.span.0.to_native(),
                            n.span.1.to_native(),
                            n.span.2.to_native(),
                            n.span.3.to_native(),
                        ) == raw.span
                })
                .unwrap_or_else(|| {
                    panic!(
                        "{}: the full build holds {:?} `{}` at {:?}",
                        local.file_path.display(),
                        raw.kind,
                        raw.name,
                        raw.span
                    )
                });
            used.insert(i);
            i as u32
        })
        .collect()
}

fn view_uids(view: &OverlayView) -> Vec<u64> {
    view.virtual_nodes().iter().map(|n| n.uid).collect()
}

fn session_dir(root: &Path, worktree: &Path) -> PathBuf {
    let sid = "uid-assign";
    let dir = root.join("sessions").join(sid);
    std::fs::create_dir_all(&dir).unwrap();
    let meta = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-10-25T00:00:00Z".into(),
        last_touched: "2026-10-25T00:00:00Z".into(),
        base_sha: "0000000000000000000000000000000000000000".into(),
        source_worktree: worktree.display().to_string(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&dir.join("session_meta.json"), &meta).unwrap();
    DirtyEvents::write_atomic(
        &layout::dirty_events_path(&dir),
        &DirtyEvents::empty(SizeCap::BUILT_IN),
    )
    .unwrap();
    dir
}

/// The inputs `Engine::overlay_view` builds from on a cache miss: the
/// fragment `write_dirty_fragments_from_graphs` writes for `local` (whose
/// file holds `source` on disk), read back through `accepted_fragments` and
/// `decode_view_inputs`.
fn reader_inputs(local: &LocalGraph, source: &str) -> Vec<OverlayFileInput> {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let file = worktree.join(&local.file_path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, source).unwrap();
    let mtime = std::fs::metadata(&file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let session = session_dir(tmp.path(), &worktree);
    write_dirty_fragments_from_graphs(&session, std::slice::from_ref(local), &[mtime]).unwrap();
    let (accepted, _) = accepted_fragments(&session, &worktree).unwrap();
    let inputs = decode_view_inputs(&accepted);
    assert_eq!(inputs.len(), 1, "the reader returns the one dirty file");
    inputs
}

/// One language's fixture: `overload_sets` sets of same-name members that
/// keep overloads, and `redefinitions` later same-uid declarations of a kind
/// that keeps none (`None`: not counted, the parser decides).
struct Case {
    provider: Box<dyn LanguageProvider>,
    path: &'static str,
    source: &'static str,
    overload_sets: usize,
    redefinitions: Option<usize>,
}

fn assert_four_paths_equal(case: Case) {
    let provider = case.provider.as_ref();
    let (path, source) = (case.path, case.source);
    let dirty = parse(provider, path, source);
    // The base commit holds the same file. Pairing reads anchors, not uids
    // (`assert_overloads_pair_nothing`); this test compares uids only.
    let full_bytes = archive(&full_build(parse(provider, path, source)));
    let full_graph = access(&full_bytes);
    let full = stored_uids(full_graph, &dirty);

    let view = OverlayView::build(
        full_graph,
        &[OverlayFileInput::from(parse(provider, path, source))],
    )
    .unwrap();
    let reader = OverlayView::build(full_graph, &reader_inputs(&dirty, source)).unwrap();
    assert_eq!(view_uids(&view), full, "{path}: overlay view vs full build");
    assert_eq!(
        view_uids(&reader),
        full,
        "{path}: overlay reader vs full build"
    );
    assert_eq!(
        node_uids_for(&dirty),
        full,
        "{path}: incremental vs full build"
    );

    // `NodeKind` has no `Hash`; its `u8` discriminant keys the set.
    let mut sets: FxHashMap<(u8, Option<&str>, &str), Vec<u64>> = FxHashMap::default();
    for (raw, &uid) in dirty.nodes.iter().zip(&full) {
        if ecp_core::uid::keeps_overloads(raw.kind) {
            sets.entry((
                raw.kind as u8,
                raw.owner_class.as_deref(),
                raw.name.as_str(),
            ))
            .or_default()
            .push(uid);
        }
    }
    let overloads: Vec<&Vec<u64>> = sets.values().filter(|uids| uids.len() > 1).collect();
    assert_eq!(
        overloads.len(),
        case.overload_sets,
        "{path}: the fixture's overload sets"
    );
    for uids in overloads {
        let distinct: FxHashSet<u64> = uids.iter().copied().collect();
        assert_eq!(distinct.len(), uids.len(), "{path}: one uid per overload");
    }

    if let Some(expected) = case.redefinitions {
        // The index's tombstone: the bit-inverse of an earlier survivor's uid.
        let tombstones = (0..full.len())
            .filter(|&j| full[..j].contains(&!full[j]))
            .count();
        assert_eq!(tombstones, expected, "{path}: tombstoned redefinitions");
    }
}

macro_rules! four_path_cases {
    ($($test:ident => $case:expr;)*) => {$(
        #[test]
        fn $test() {
            assert_four_paths_equal($case);
        }
    )*};
}

// The overload fixtures that more than one test reads: two `save` members
// of `K`, plus `audit` and `run`, which each have one declaration.
const TS_K: &str = "class K {\n audit() {}\n save(id: number) {}\n save(name: string) { this.audit(); }\n run() { this.save(1); }\n}\n";
const JAVA_K: &str = "class K {\n void audit() {}\n void save(int id) {}\n void save(String name) { audit(); }\n void run() { save(1); }\n}\n";
const KOTLIN_K: &str = "class K {\n fun audit() {}\n fun save(id: Int) {}\n fun save(name: String) { audit() }\n fun run() { save(1) }\n}\n";
const CSHARP_K: &str = "class K {\n void audit() {}\n void save(int id) {}\n void save(string name) { audit(); }\n void run() { save(1); }\n}\n";
const RUST_K: &str = "struct K;\nimpl K {\n fn audit(&self) {}\n fn save(&self) {}\n fn run(&self) { self.save(); }\n}\nimpl A for K { fn save(&self) { self.audit(); } }\ntrait A { fn save(&self); }\n";
const SWIFT_K: &str = "class K {\n func audit() {}\n func save(id: Int) {}\n func save(name: String) { audit() }\n func run() { save(id: 1) }\n}\n";
const CPP_K: &str = "class K {\n void audit() {}\n void save(int id) {}\n void save(const char* name) { audit(); }\n void run() { save(1); }\n};\n";
const DART_K: &str = "class K {\n void audit() {}\n void save(int id) {}\n void save(String name) { audit(); }\n void run() { save(1); }\n}\n";

fn methods(provider: Box<dyn LanguageProvider>, path: &'static str, source: &'static str) -> Case {
    Case {
        provider,
        path,
        source,
        overload_sets: 1,
        redefinitions: None,
    }
}

four_path_cases! {
    test_uid_assign_typescript_overloads_four_paths_equal_uids => methods(
        Box::new(TypeScriptProvider::new().unwrap()),
        "k.ts",
        TS_K,
    );
    test_uid_assign_javascript_overloads_four_paths_equal_uids => methods(
        Box::new(JavaScriptProvider::new().unwrap()),
        "k.js",
        "class K {\n audit() {}\n save(id) {}\n save(id, name) { this.audit(); }\n run() { this.save(1); }\n}\n",
    );
    test_uid_assign_python_overloads_four_paths_equal_uids => methods(
        Box::new(PythonProvider::new().unwrap()),
        "k.py",
        "class K:\n    def audit(self):\n        pass\n    def save(self, id):\n        pass\n    def save(self, id, name):\n        self.audit()\n    def run(self):\n        self.save(1)\n",
    );
    test_uid_assign_java_overloads_four_paths_equal_uids => methods(
        Box::new(JavaProvider::new().unwrap()),
        "K.java",
        JAVA_K,
    );
    test_uid_assign_kotlin_overloads_four_paths_equal_uids => methods(
        Box::new(KotlinProvider::new().unwrap()),
        "K.kt",
        KOTLIN_K,
    );
    test_uid_assign_csharp_overloads_four_paths_equal_uids => methods(
        Box::new(CSharpProvider::new().unwrap()),
        "K.cs",
        CSHARP_K,
    );
    test_uid_assign_go_overloads_four_paths_equal_uids => methods(
        Box::new(GoProvider::new().unwrap()),
        "k.go",
        "package demo\ntype K struct{}\nfunc (k *K) audit() {}\nfunc (k *K) save(id int) {}\nfunc (k *K) save(name string) { k.audit() }\nfunc (k *K) run() { k.save(1) }\n",
    );
    test_uid_assign_rust_overloads_four_paths_equal_uids => methods(
        Box::new(RustProvider::new().unwrap()),
        "k.rs",
        RUST_K,
    );
    test_uid_assign_php_overloads_four_paths_equal_uids => methods(
        Box::new(PhpProvider::new().unwrap()),
        "K.php",
        "<?php\nclass K {\n function audit() {}\n function save($id) {}\n function save($id, $name) { $this->audit(); }\n function run() { $this->save(1); }\n}\n",
    );
    test_uid_assign_ruby_overloads_four_paths_equal_uids => methods(
        Box::new(RubyProvider::new().unwrap()),
        "k.rb",
        "class K\n  def audit\n  end\n  def save(id)\n  end\n  def save(id, name)\n    audit()\n  end\n  def run\n    save(1)\n  end\nend\n",
    );
    test_uid_assign_swift_overloads_four_paths_equal_uids => methods(
        Box::new(SwiftProvider::new().unwrap()),
        "K.swift",
        SWIFT_K,
    );
    test_uid_assign_c_overloads_four_paths_equal_uids => Case {
        provider: Box::new(CProvider::new().unwrap()),
        path: "k.c",
        source: "#ifdef A\nint save(int id) { return 0; }\n#else\nint save(int id) { return 1; }\n#endif\nint run(void) { return save(1); }\n",
        overload_sets: 0,
        redefinitions: Some(1),
    };
    test_uid_assign_cpp_overloads_four_paths_equal_uids => methods(
        Box::new(CppProvider::new().unwrap()),
        "k.cpp",
        CPP_K,
    );
    test_uid_assign_dart_overloads_four_paths_equal_uids => methods(
        Box::new(DartProvider::new().unwrap()),
        "k.dart",
        DART_K,
    );
}

/// `class Conv` and `class Conv<T> : Conv` in one C# file, each with a
/// `Can` method, in source order and with the generic one first.
const CSHARP_CONV: [&str; 2] = [
    "namespace N {\n class Conv {\n  bool Can(int t) { return true; }\n }\n class Conv<T> : Conv {\n  bool Can(int t) { return false; }\n }\n}\n",
    "namespace N {\n class Conv<T> : Conv {\n  bool Can(int t) { return false; }\n }\n class Conv {\n  bool Can(int t) { return true; }\n }\n}\n",
];

/// Stands for: a C# type of another generic arity (`Conv<T>` beside
/// `Conv`) is no tombstone on any path that numbers the file. The second
/// class in source order takes overload uid 1 of the pair's base uid on the
/// full build, the overlay view, the overlay reader over the written
/// fragment, and incremental mode, in either declaration order (codex
/// ruling P-a). Before the fix every path tombstoned it alike.
#[test]
fn test_uid_assign_csharp_types_of_two_arities_four_paths_equal_uids() {
    for source in CSHARP_CONV {
        assert_four_paths_equal(Case {
            provider: Box::new(CSharpProvider::new().unwrap()),
            path: "N/Conv.cs",
            source,
            overload_sets: 1,
            redefinitions: Some(0),
        });
        let dirty = parse(&CSharpProvider::new().unwrap(), "N/Conv.cs", source);
        let bytes = archive(&full_build(dirty.clone()));
        let uids = stored_uids(access(&bytes), &dirty);
        let classes: Vec<usize> = (0..dirty.nodes.len())
            .filter(|&i| dirty.nodes[i].kind == NodeKind::Class)
            .collect();
        assert_eq!(classes.len(), 2, "{source}");
        let first = &dirty.nodes[classes[0]];
        let base = ecp_core::uid::compute(
            NodeKind::Class,
            "N/Conv.cs",
            first.owner_class.as_deref(),
            "Conv",
        );
        assert_eq!(
            [uids[classes[0]], uids[classes[1]]],
            [base, ecp_core::uid::overload(base, 1)],
            "{source}"
        );
    }
}

/// A Unix file named `.\K.java` is stored `./K.java`, and one named
/// `.\.\K.java` is stored `././K.java`: the builder only turns `\` into
/// `/`. Every path hashes that stored name, and none panics on the doubled
/// prefix.
#[test]
fn test_uid_assign_dot_backslash_file_name_hashes_stored_path() {
    let java = JavaProvider::new().unwrap();
    let source = "class K {\n void save(int id) {}\n void save(String name) {}\n}\n";
    for (walked, stored) in [(".\\K.java", "./K.java"), (".\\.\\K.java", "././K.java")] {
        let dirty = parse(&java, walked, source);
        let full_bytes = archive(&full_build(parse(&java, walked, source)));
        let full_graph = access(&full_bytes);
        let full = stored_uids(full_graph, &dirty);
        let saves = named(&dirty, "save");
        assert_eq!(saves.len(), 2, "{walked}: the fixture's two overloads");
        let first = &dirty.nodes[saves[0]];
        let base = ecp_core::uid::compute(first.kind, stored, first.owner_class.as_deref(), "save");
        assert_eq!(
            [full[saves[0]], full[saves[1]]],
            [base, ecp_core::uid::overload(base, 1)],
            "{walked}: the uid hashes the stored path `{stored}`"
        );
        let view = OverlayView::build(
            full_graph,
            &[OverlayFileInput::from(parse(&java, walked, source))],
        )
        .unwrap();
        assert_eq!(
            view_uids(&view),
            full,
            "{walked}: overlay view vs full build"
        );
        assert_eq!(
            node_uids_for(&dirty),
            full,
            "{walked}: incremental vs full build"
        );
    }
}

/// The index of each node of `local` named `name`.
fn named(local: &LocalGraph, name: &str) -> Vec<usize> {
    (0..local.nodes.len())
        .filter(|&i| local.nodes[i].name == name)
        .collect()
}

/// Editing one overload's body re-resolves that overload only: its twin
/// compares with its own old body hash, not with the first overload's.
#[test]
fn test_incremental_edit_one_overload_skips_unchanged_twin() {
    let java = JavaProvider::new().unwrap();
    let path = "K.java";
    let before = "class K {\n void audit() {}\n void save(int id) {}\n void save(String name) { audit(); }\n}\n";
    let after = "class K {\n void audit() {}\n void save(int id) { audit(); }\n void save(String name) { audit(); }\n}\n";
    let old_graph = full_build(parse(&java, path, before));
    let old_hashes = build_old_hash_map(&old_graph.nodes);
    let old_imports = build_old_import_map(&[parse(&java, path, before)]);
    let edited = parse(&java, path, after);
    let fresh_bytes = archive(&full_build(parse(&java, path, after)));
    let fresh = stored_uids(access(&fresh_bytes), &edited);

    // Truth from the two full builds: a node re-resolves when the old build
    // stored no equal body under the uid the new build gives it.
    let expected: FxHashSet<u64> = edited
        .nodes
        .iter()
        .zip(&fresh)
        .filter(|(n, uid)| n.content_hash == 0 || old_hashes.get(*uid) != Some(&n.content_hash))
        .map(|(_, &uid)| uid)
        .collect();
    let saves = named(&edited, "save");
    assert_eq!(saves.len(), 2, "the fixture holds two overloads");
    assert!(
        expected.contains(&fresh[saves[0]]),
        "the edited overload re-resolves"
    );
    assert!(
        !expected.contains(&fresh[saves[1]]),
        "the fixture leaves the twin's body unchanged"
    );

    let result = symbol_hash_diff(
        &old_hashes,
        &old_imports,
        &FxHashMap::default(),
        std::slice::from_ref(&edited),
        &[],
    );
    match result.decisions.get(path) {
        Some(FileDiffDecision::PartialResolve { changed_uids }) => {
            assert_eq!(changed_uids, &expected, "re-resolved uids")
        }
        other => panic!("expected PartialResolve, got {other:?}"),
    }
    assert_eq!(
        result.skipped_count,
        edited.nodes.len() - expected.len(),
        "the unchanged twin counts as skipped"
    );
}

/// A dirty Java file with `save(int)` / `save(String)`: each virtual
/// overload keeps the full build's uid, but the two share an anchor, so
/// neither replaces a base node and both base overloads are suppressed.
/// R-PI flips this from "each replaces the base node of its own uid": the
/// ordinal proves nothing about which overload an edit kept. A `Signature`
/// discriminator (C1 Open point 2) would make each anchor unique again.
#[test]
fn test_overlay_view_java_two_save_overloads_pair_nothing() {
    let java = JavaProvider::new().unwrap();
    let path = "K.java";
    let before = "class K {\n void save(int id) {}\n void save(String name) {}\n}\n";
    let after =
        "class K {\n void save(int id) { log(); }\n void save(String name) { log(); }\n void log() {}\n}\n";
    let base_bytes = archive(&full_build(parse(&java, path, before)));
    let base = access(&base_bytes);
    let view =
        OverlayView::build(base, &[OverlayFileInput::from(parse(&java, path, after))]).unwrap();

    let base_saves: Vec<u32> = (0..base.nodes.len() as u32)
        .filter(|&i| base.nodes[i as usize].name.resolve(&base.string_pool) == "save")
        .collect();
    let virtual_saves: Vec<u32> = (view.base_len()..)
        .zip(view.virtual_nodes())
        .filter(|(_, n)| n.name == "save")
        .map(|(idx, _)| idx)
        .collect();
    assert_eq!(base_saves.len(), 2, "the base holds both overloads");
    assert_eq!(virtual_saves.len(), 2, "the view holds both overloads");
    for (k, (&base_idx, &virt)) in base_saves.iter().zip(&virtual_saves).enumerate() {
        let node = view.node(virt).unwrap();
        assert_eq!(
            node.uid,
            base.nodes[base_idx as usize].uid.to_native(),
            "overload {k} keeps its base uid"
        );
        assert_eq!(node.replaced_base, None, "overload {k} replaces nothing");
        assert_eq!(
            view.redirect(base_idx),
            None,
            "base overload {k} is suppressed"
        );
    }
}

/// Two `namespace Acme` blocks in one dirty C# file: the view keeps the
/// first and tombstones the second exactly as a full build of the file does
/// (`tests/baselines/strict_parity/dirty_csharp.txt` held the difference).
#[test]
fn test_overlay_view_csharp_two_namespace_blocks_tombstones_second() {
    let cs = CSharpProvider::new().unwrap();
    let path = "src/Lib.cs";
    let one = "namespace Acme\n{\n    public static class Lib\n    {\n        public static int Helper()\n        {\n            return 1;\n        }\n    }\n}\n";
    let two =
        format!("{one}\nnamespace Acme\n{{\n    public static class Extra\n    {{\n    }}\n}}\n");
    let base_bytes = archive(&full_build(parse(&cs, path, one)));
    let base = access(&base_bytes);
    let full_bytes = archive(&full_build(parse(&cs, path, &two)));
    let full = access(&full_bytes);
    let view = OverlayView::build(base, &[OverlayFileInput::from(parse(&cs, path, &two))]).unwrap();

    let full_namespaces: Vec<(u64, &str, u32)> = full
        .nodes
        .iter()
        .filter(|n| NodeKind::from(&n.kind) == NodeKind::Namespace)
        .map(|n| {
            (
                n.uid.to_native(),
                n.name.resolve(&full.string_pool),
                n.span.0.to_native() + 1,
            )
        })
        .collect();
    assert_eq!(full_namespaces.len(), 2, "the full build holds both blocks");
    assert_eq!(
        (
            full_namespaces[1].0,
            full_namespaces[1].1,
            full_namespaces[1].2
        ),
        (!full_namespaces[0].0, "", 12),
        "the full build tombstones the second block"
    );

    let virtual_namespaces: Vec<_> = view
        .virtual_nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::Namespace)
        .collect();
    assert_eq!(
        virtual_namespaces
            .iter()
            .map(|n| (n.uid, n.name.as_str(), n.start_line))
            .collect::<Vec<_>>(),
        full_namespaces,
        "the view's namespaces equal the full build's"
    );
    let tombstone = virtual_namespaces[1];
    assert_eq!(tombstone.owner_class, None, "a tombstone has no owner");
    assert_eq!(
        tombstone.replaced_base, None,
        "the base holds one block, so the tombstone replaces nothing"
    );
    assert!(!tombstone.later_overload, "a namespace is not an overload");
}

/// One node of a view or a graph: `(merged idx, kind, name, owner, start
/// line, end line)`, lines 1-based.
type NodeRow = (u32, NodeKind, String, String, u32, u32);

/// The nodes named `name`, keyed by the trait of the `Impl` node whose lines
/// hold each one.
fn by_trait(rows: &[NodeRow], name: &str) -> FxHashMap<String, u32> {
    rows.iter()
        .filter(|r| r.2 == name)
        .map(|r| {
            let block = rows
                .iter()
                .find(|b| b.1 == NodeKind::Impl && b.4 <= r.4 && r.5 <= b.5)
                .unwrap_or_else(|| panic!("`{name}` at line {} is in no impl", r.4));
            (block.3.clone(), r.0)
        })
        .collect()
}

fn graph_rows(graph: &ArchivedZeroCopyGraph) -> Vec<NodeRow> {
    let pool = &graph.string_pool;
    (0u32..)
        .zip(graph.nodes.iter())
        .map(|(i, n)| {
            (
                i,
                NodeKind::from(&n.kind),
                n.name.resolve(pool).to_string(),
                n.owner_class.resolve(pool).to_string(),
                n.span.0.to_native() + 1,
                n.span.2.to_native() + 1,
            )
        })
        .collect()
}

fn view_rows(view: &OverlayView) -> Vec<NodeRow> {
    (view.base_len()..)
        .zip(view.virtual_nodes())
        .map(|(i, n)| {
            (
                i,
                n.kind,
                n.name.clone(),
                n.owner_class.clone().unwrap_or_default(),
                n.start_line,
                n.end_line,
            )
        })
        .collect()
}

fn fmt_impls(first: &str, second: &str) -> String {
    let block = |t: &str| {
        format!(
            "impl {t} for A {{\n    fn fmt(&self) {{\n        fn pad() {{}}\n        pad();\n    }}\n}}\n"
        )
    };
    format!("struct A;\n{}{}", block(first), block(second))
}

/// A dirty Rust file swaps `impl Display for A { fn fmt }` and `impl Debug
/// for A { fn fmt }`. The two `fmt` swap stored uids, because the second in
/// source order is overload 1, so a uid pairing would hand each base `fmt`'s
/// clean callers to the other trait's `fmt`. The anchor carries the trait,
/// so each base `fmt` redirects to its own trait's virtual `fmt`, and a
/// base in-edge follows the redirect. The nested `pad` stays on its own too.
#[test]
fn test_overlay_view_swapped_impls_base_edges_stay_on_own_fmt() {
    let rust = RustProvider::new().unwrap();
    let path = "src/a.rs";
    let before = fmt_impls("Display", "Debug");
    let after = fmt_impls("Debug", "Display");
    let base_bytes = archive(&full_build(parse(&rust, path, &before)));
    let base = access(&base_bytes);
    let full_bytes = archive(&full_build(parse(&rust, path, &after)));
    let full = access(&full_bytes);
    let view =
        OverlayView::build(base, &[OverlayFileInput::from(parse(&rust, path, &after))]).unwrap();

    let (base_rows, full_rows, rows) = (graph_rows(base), graph_rows(full), view_rows(&view));
    for name in ["fmt", "pad"] {
        let (base_of, full_of, virt_of) = (
            by_trait(&base_rows, name),
            by_trait(&full_rows, name),
            by_trait(&rows, name),
        );
        assert_eq!(virt_of.len(), 2, "{name}: one per trait: {virt_of:?}");
        for t in ["Display", "Debug"] {
            let (base_idx, virt) = (base_of[t], virt_of[t]);
            let node = view.node(virt).unwrap();
            assert_eq!(
                node.uid,
                full.nodes[full_of[t] as usize].uid.to_native(),
                "{name} of {t}: the view's uid is the full build's"
            );
            assert_eq!(
                node.replaced_base,
                Some(base_idx),
                "{name} of {t} replaces its own trait's base node"
            );
            assert_eq!(
                view.redirect(base_idx),
                Some(virt),
                "{name} of {t}: redirect"
            );
        }
    }
    let base_display = base.nodes[by_trait(&base_rows, "fmt")["Display"] as usize]
        .uid
        .to_native();
    let virt_display = view.node(by_trait(&rows, "fmt")["Display"]).unwrap().uid;
    assert_ne!(
        base_display, virt_display,
        "the fixture swaps the stored uids, so a uid pairing would cross them"
    );
}

/// Rust tells `impl K { fn save }` and `impl A for K { fn save }` apart by
/// the trait: two anchors, so each `save` replaces its own base node while
/// the two keep the overload uids of the full build.
#[test]
fn test_overlay_view_rust_inherent_and_trait_save_each_replaces_own_base() {
    let rust = RustProvider::new().unwrap();
    let path = "k.rs";
    let local = parse(&rust, path, RUST_K);
    let base_bytes = archive(&full_build(parse(&rust, path, RUST_K)));
    let base = access(&base_bytes);
    let view =
        OverlayView::build(base, &[OverlayFileInput::from(parse(&rust, path, RUST_K))]).unwrap();
    let base_idx = base_indices(base, &local);
    let k_saves: Vec<usize> = (0..local.nodes.len())
        .filter(|&j| {
            local.nodes[j].name == "save" && local.nodes[j].owner_class.as_deref() == Some("K")
        })
        .collect();
    assert_eq!(k_saves.len(), 2, "the fixture's two `save` of `K`");
    for j in k_saves {
        let virt = view.base_len() + j as u32;
        assert_eq!(
            view.node(virt).unwrap().replaced_base,
            Some(base_idx[j]),
            "`save` {j} replaces its own base node"
        );
        assert_eq!(view.redirect(base_idx[j]), Some(virt));
    }
}

/// Without a `Signature` discriminator (C1 Open point 2: no parser
/// produces one, and the base graph stores no parameter list), two
/// overloads of one owner share an anchor. The view gives each the full
/// build's uid, pairs neither with a base node and suppresses both base
/// members; a member whose anchor is unique still replaces its own base
/// node. A `Signature` slice flips the overload half of this test.
fn assert_overloads_pair_nothing(provider: Box<dyn LanguageProvider>, path: &str, source: &str) {
    let provider = provider.as_ref();
    let local = parse(provider, path, source);
    let base_bytes = archive(&full_build(parse(provider, path, source)));
    let base = access(&base_bytes);
    let view = OverlayView::build(
        base,
        &[OverlayFileInput::from(parse(provider, path, source))],
    )
    .unwrap();
    assert_eq!(
        view_uids(&view),
        stored_uids(base, &local),
        "{path}: overlay view vs full build"
    );
    let base_idx = base_indices(base, &local);
    let mut sets: FxHashMap<(u8, Option<&str>, &str), Vec<usize>> = FxHashMap::default();
    for (j, raw) in local.nodes.iter().enumerate() {
        if ecp_core::uid::keeps_overloads(raw.kind) {
            sets.entry((
                raw.kind as u8,
                raw.owner_class.as_deref(),
                raw.name.as_str(),
            ))
            .or_default()
            .push(j);
        }
    }
    let overloads: Vec<&Vec<usize>> = sets.values().filter(|s| s.len() > 1).collect();
    assert_eq!(overloads.len(), 1, "{path}: the fixture's one overload set");
    for &j in overloads[0] {
        let virt = view.base_len() + j as u32;
        assert_eq!(
            view.node(virt).unwrap().replaced_base,
            None,
            "{path}: overload {j}"
        );
        assert_eq!(
            view.redirect(base_idx[j]),
            None,
            "{path}: base overload {j}"
        );
    }
    let unique: Vec<usize> = sets
        .values()
        .filter(|s| s.len() == 1)
        .map(|s| s[0])
        .collect();
    assert!(
        !unique.is_empty(),
        "{path}: the fixture's `audit` and `run`"
    );
    for j in unique {
        let virt = view.base_len() + j as u32;
        assert_eq!(
            view.node(virt).unwrap().replaced_base,
            Some(base_idx[j]),
            "{path}: member {j} has a unique anchor"
        );
        assert_eq!(view.redirect(base_idx[j]), Some(virt), "{path}: member {j}");
    }
}

macro_rules! overload_pairing_cases {
    ($($test:ident => ($provider:expr, $path:expr, $source:expr);)*) => {$(
        #[test]
        fn $test() {
            assert_overloads_pair_nothing(Box::new($provider), $path, $source);
        }
    )*};
}

overload_pairing_cases! {
    test_overlay_view_typescript_overloads_no_signature_pair_nothing =>
        (TypeScriptProvider::new().unwrap(), "k.ts", TS_K);
    test_overlay_view_java_overloads_no_signature_pair_nothing =>
        (JavaProvider::new().unwrap(), "K.java", JAVA_K);
    test_overlay_view_kotlin_overloads_no_signature_pair_nothing =>
        (KotlinProvider::new().unwrap(), "K.kt", KOTLIN_K);
    test_overlay_view_csharp_overloads_no_signature_pair_nothing =>
        (CSharpProvider::new().unwrap(), "K.cs", CSHARP_K);
    test_overlay_view_cpp_overloads_no_signature_pair_nothing =>
        (CppProvider::new().unwrap(), "k.cpp", CPP_K);
    test_overlay_view_swift_overloads_no_signature_pair_nothing =>
        (SwiftProvider::new().unwrap(), "K.swift", SWIFT_K);
    test_overlay_view_dart_overloads_no_signature_pair_nothing =>
        (DartProvider::new().unwrap(), "k.dart", DART_K);
}

/// Two `impl T for A<..>` blocks both write self type `A` and owner `T`, so
/// the base tombstones the second block and clears its owner. A member of
/// that block must take no trait on either side, or a comment-only edit
/// unpairs it: its clean callers vanish and no caveat names it.
#[test]
fn test_overlay_repeated_impl_block_member_pairs_after_comment_edit() {
    let provider = RustProvider::new().unwrap();
    let path = "src/a.rs";
    let source = "trait T {\n    fn a(&self);\n    fn b(&self);\n}\nstruct A<X>(X);\nimpl T for A<u8> {\n    fn a(&self) {}\n}\nimpl T for A<u16> {\n    fn b(&self) {}\n}\n";
    let base = parse(&provider, path, source);
    let base_bytes = archive(&full_build(base.clone()));
    let base_graph = access(&base_bytes);
    let b = base
        .nodes
        .iter()
        .position(|n| n.name == "b" && n.span.0 == 9)
        .expect("`b` of the second impl");
    let base_b = base_indices(base_graph, &base)[b];

    let edited = parse(&provider, path, &format!("{source}// edited\n"));
    let view = OverlayView::build(base_graph, &[OverlayFileInput::from(edited)]).unwrap();

    assert!(
        view.redirect(base_b).is_some(),
        "`b` keeps its base edges through the edit"
    );
    assert!(
        view.hidden_declarations().is_empty(),
        "nothing is ambiguous: {:?}",
        view.hidden_declarations()
    );
}
