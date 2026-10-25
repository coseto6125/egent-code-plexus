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

/// The uid `graph` stores for each node of `local`, in node order: the node
/// of equal kind and span, the k-th one for the k-th repeat of that key.
fn stored_uids(graph: &ArchivedZeroCopyGraph, local: &LocalGraph) -> Vec<u64> {
    let mut used: FxHashSet<usize> = FxHashSet::default();
    local
        .nodes
        .iter()
        .map(|raw| {
            let (i, node) = graph
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
            node.uid.to_native()
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
    // The base commit holds the same file, so each virtual node replaces the
    // base node of its own uid.
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
        "class K {\n audit() {}\n save(id: number) {}\n save(name: string) { this.audit(); }\n run() { this.save(1); }\n}\n",
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
        "class K {\n void audit() {}\n void save(int id) {}\n void save(String name) { audit(); }\n void run() { save(1); }\n}\n",
    );
    test_uid_assign_kotlin_overloads_four_paths_equal_uids => methods(
        Box::new(KotlinProvider::new().unwrap()),
        "K.kt",
        "class K {\n fun audit() {}\n fun save(id: Int) {}\n fun save(name: String) { audit() }\n fun run() { save(1) }\n}\n",
    );
    test_uid_assign_csharp_overloads_four_paths_equal_uids => methods(
        Box::new(CSharpProvider::new().unwrap()),
        "K.cs",
        "class K {\n void audit() {}\n void save(int id) {}\n void save(string name) { audit(); }\n void run() { save(1); }\n}\n",
    );
    test_uid_assign_go_overloads_four_paths_equal_uids => methods(
        Box::new(GoProvider::new().unwrap()),
        "k.go",
        "package demo\ntype K struct{}\nfunc (k *K) audit() {}\nfunc (k *K) save(id int) {}\nfunc (k *K) save(name string) { k.audit() }\nfunc (k *K) run() { k.save(1) }\n",
    );
    test_uid_assign_rust_overloads_four_paths_equal_uids => methods(
        Box::new(RustProvider::new().unwrap()),
        "k.rs",
        "struct K;\nimpl K {\n fn audit(&self) {}\n fn save(&self) {}\n fn run(&self) { self.save(); }\n}\nimpl A for K { fn save(&self) { self.audit(); } }\ntrait A { fn save(&self); }\n",
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
        "class K {\n func audit() {}\n func save(id: Int) {}\n func save(name: String) { audit() }\n func run() { save(id: 1) }\n}\n",
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
        "class K {\n void audit() {}\n void save(int id) {}\n void save(const char* name) { audit(); }\n void run() { save(1); }\n};\n",
    );
    test_uid_assign_dart_overloads_four_paths_equal_uids => methods(
        Box::new(DartProvider::new().unwrap()),
        "k.dart",
        "class K {\n void audit() {}\n void save(int id) {}\n void save(String name) { audit(); }\n void run() { save(1); }\n}\n",
    );
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
/// overload replaces the base node of its own uid, not the first one's.
#[test]
fn test_overlay_view_java_two_save_overloads_each_replaces_own_base() {
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
        assert_eq!(
            node.replaced_base,
            Some(base_idx),
            "overload {k} replaces its own base"
        );
        assert_eq!(
            view.redirect(base_idx),
            Some(virt),
            "base overload {k} redirects to it"
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
