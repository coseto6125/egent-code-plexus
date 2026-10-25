//! Roundtrip + validation tests for the v2 fragment path feeding
//! `OverlayView` (FU-2026-06-10-398a846bca42): `write_dirty_fragments_from_graphs`
//! must persist owner_class / calls / imports, and `accepted_fragments` must
//! refuse entries that would serve stale overlay data (reverted files, v1 bins).

#[path = "common/parity.rs"]
mod parity;

use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_cli::session::overlay_reader::{accepted_fragments, decode_view_inputs};
use ecp_cli::session::overlay_writer::{
    write_dirty_fragments_from_graphs, FragmentFileV3, FragmentSymbolV3,
};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, NameRef, NameRefKind, RawImport, RawNode, VariantRef};
use ecp_core::contracts::dirty_event::{PathChange, PathClass};
use ecp_core::graph::NodeKind;
use ecp_core::session::{
    layout, DirtyEntry, DirtyEventEntry, DirtyEvents, DirtyFiles, OverlayFileInput, SessionMeta,
    FRAGMENT_FORMAT_V3, FRAGMENT_FORMAT_V4,
};
use parity::{callees, calls_after_edit, Parity};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The inputs `Engine::overlay_view` builds from on a cache miss.
fn load_view_inputs(session_dir: &Path, worktree: &Path) -> Vec<OverlayFileInput> {
    let (accepted, _) = accepted_fragments(session_dir, worktree).unwrap();
    decode_view_inputs(&accepted)
}

fn make_session_dir(tmp: &Path, sid: &str) -> PathBuf {
    let session_dir = tmp.join("sessions").join(sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-06-10T10:00:00Z".into(),
        last_touched: "2026-06-10T10:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: "/work/x".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();
    DirtyEvents::write_atomic(
        &layout::dirty_events_path(&session_dir),
        &DirtyEvents::empty(ecp_core::config::resolve_size_cap(Path::new("/work/x"))),
    )
    .unwrap();
    session_dir
}

/// The manifest entry today's namespace holds for a legacy `entry`: the same
/// mtime, hash, bin and format, recorded as a modified source file.
fn event_from_legacy(entry: &DirtyEntry) -> DirtyEventEntry {
    DirtyEventEntry {
        change: PathChange::Modified,
        class: PathClass::SOURCE,
        mtime_ns: entry.mtime_ns,
        fragment_id: Some(entry.fragment_id.clone()),
        content_hash: Some(entry.content_hash.clone()),
        parse_failed: entry.parse_failed,
        dirty_symbols: entry.dirty_symbols.clone(),
        format: entry.format,
    }
}

fn rn(name: &str, owner: Option<&str>, calls: &[&str]) -> RawNode {
    RawNode {
        name: name.to_string(),
        kind: if owner.is_some() {
            NodeKind::Method
        } else {
            NodeKind::Function
        },
        span: (1, 0, 5, 0),
        is_exported: true,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: calls.iter().map(|c| c.to_string()).collect(),
        field_reads: Vec::new(),
        owner_class: owner.map(str::to_string),
        content_hash: 7,
    }
}

/// Write a real worktree file and return its stat mtime in nanoseconds —
/// the same value `apply_l1_overlay_updates` records into the manifest.
fn write_worktree_file(worktree: &Path, rel: &str) -> u64 {
    let p = worktree.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "pub fn x() {}\n").unwrap();
    std::fs::metadata(&p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

fn graph_for(rel: &str, nodes: Vec<RawNode>, imports: Vec<RawImport>) -> LocalGraph {
    LocalGraph {
        file_path: PathBuf::from(rel),
        content_hash: [1, 2, 3, 4, 5, 6, 7, 8],
        nodes,
        imports,
        ..Default::default()
    }
}

#[test]
fn v2_roundtrip_preserves_owner_calls_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "rt-sid");
    let mtime = write_worktree_file(&worktree, "src/dirty.rs");

    let graph = graph_for(
        "src/dirty.rs",
        vec![
            rn("free_fn", None, &["callee_a", "callee_b"]),
            rn("a_method", Some("MyType"), &[]),
        ],
        vec![RawImport {
            source: "crate::other".into(),
            imported_name: "callee_a".into(),
            alias: None,
            binding_kind: None,
        }],
    );
    write_dirty_fragments_from_graphs(&session_dir, &[graph], &[mtime]).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert_eq!(inputs.len(), 1, "one valid v2 entry expected");
    let file = &inputs[0];
    assert_eq!(file.rel_path, "src/dirty.rs");
    assert_eq!(file.imports.len(), 1);
    assert_eq!(file.imports[0].imported_name, "callee_a");

    let free_fn = file.symbols.iter().find(|s| s.name == "free_fn").unwrap();
    assert_eq!(free_fn.calls, vec!["callee_a", "callee_b"]);
    // span row 1 (0-based) → line 2 (1-based Node convention).
    assert_eq!(free_fn.start_line, 2);

    let method = file.symbols.iter().find(|s| s.name == "a_method").unwrap();
    assert_eq!(
        method.owner_class.as_deref(),
        Some("MyType"),
        "owner_class must survive the roundtrip — method uids depend on it"
    );
}

#[test]
fn reverted_file_entry_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "rev-sid");
    let real_mtime = write_worktree_file(&worktree, "src/dirty.rs");

    let graph = graph_for("src/dirty.rs", vec![rn("free_fn", None, &[])], vec![]);
    // Manifest records a DIFFERENT mtime than the file on disk — the
    // file was reverted/touched after the fragment write.
    write_dirty_fragments_from_graphs(&session_dir, &[graph], &[real_mtime + 1]).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert!(
        inputs.is_empty(),
        "a stale manifest entry must never beat fresh base data: {inputs:?}"
    );
}

#[test]
fn v1_format_entry_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "v1-sid");
    let mtime = write_worktree_file(&worktree, "src/old.rs");

    let manifest = layout::dirty_events_path(&session_dir);
    let mut df = DirtyEvents::read(&manifest).unwrap();
    df.entries.insert(
        "src/old.rs".into(),
        DirtyEventEntry {
            change: PathChange::Modified,
            class: PathClass::SOURCE,
            mtime_ns: mtime,
            content_hash: Some("cafe".into()),
            fragment_id: Some("cafe".into()),
            parse_failed: false,
            dirty_symbols: vec![],
            format: 1,
        },
    );
    DirtyEvents::write_atomic(&manifest, &df).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert!(
        inputs.is_empty(),
        "v1 bins carry no calls/imports — the view must treat them as clean"
    );
}

/// The session ecp 0.14.8 left for a dirty `app.py`: a v2 bin in a legacy
/// `dirty_files.json` with no builder fingerprint. Only the recorded mtime is
/// moved to the copied file's, so the mtime check alone would accept it. The
/// reader opens `l1/` only and never the legacy namespace, so it must serve
/// the base graph instead.
#[test]
fn test_load_view_inputs_session_written_by_0_14_8_returns_no_input() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/overlay-written-by-0.14.8");
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::copy(fixture.join("app.py"), worktree.join("app.py")).unwrap();
    let mtime = std::fs::metadata(worktree.join("app.py"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let session_dir = make_session_dir(tmp.path(), "old-sid");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture.join("dirty_files.json")).unwrap())
            .unwrap();
    manifest["entries"]["app.py"]["mtime_ns"] = mtime.into();
    std::fs::write(
        layout::legacy_dirty_files_path(&session_dir),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(layout::legacy_overlay_dir(&session_dir)).unwrap();
    std::fs::copy(
        fixture.join("graph_overlay/553e94275e6085db.bin"),
        layout::legacy_overlay_dir(&session_dir).join("553e94275e6085db.bin"),
    )
    .unwrap();
    let old = DirtyFiles::read(&layout::legacy_dirty_files_path(&session_dir)).unwrap();
    assert!(!old.from_current_builder());
    assert_eq!(old.entries["app.py"].mtime_ns, mtime);

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert!(
        inputs.is_empty(),
        "another builder's fragment beat the base graph: {inputs:?}"
    );
}

/// The fragment carries what the overlay's export rule reads: each
/// symbol's export flag and the file's export aliases.
#[test]
fn test_load_view_inputs_export_flag_and_aliases_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "exp-sid");
    let mtime = write_worktree_file(&worktree, "src/a.ts");
    let mut private = rn("f", None, &[]);
    private.is_exported = false;
    let mut graph = graph_for("src/a.ts", vec![private, rn("g", None, &[])], vec![]);
    let aliases = vec![ecp_core::analyzer::types::RawExportAlias {
        exported: "f".into(),
        local: "g".into(),
    }];
    graph.export_aliases = Some(aliases.clone().into_boxed_slice());
    write_dirty_fragments_from_graphs(&session_dir, &[graph], &[mtime]).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert_eq!(inputs.len(), 1);
    let exported: Vec<(&str, bool)> = inputs[0]
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.is_exported))
        .collect();
    assert_eq!(exported, [("f", false), ("g", true)]);
    assert_eq!(inputs[0].export_aliases, aliases);
}

fn type_node(name: &str, heritage: &[&str], field_reads: &[String]) -> RawNode {
    RawNode {
        kind: NodeKind::Class,
        heritage: heritage.iter().map(|h| h.to_string()).collect(),
        field_reads: field_reads.to_vec(),
        ..rn(name, None, &[])
    }
}

/// The fragment carries what the overlay's heritage tier and variant pass
/// read: each type's declared bases in order, and only the enum-variant
/// entries of `field_reads`, still encoded.
#[test]
fn test_load_view_inputs_heritage_and_variant_refs_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "her-sid");
    let mtime = write_worktree_file(&worktree, "src/a.rs");
    let used = VariantRef::encode("Color::Red", false);
    let matched = VariantRef::encode("Color::Blue", true);
    let callback = NameRef::encode("callback", NameRefKind::Function);
    let reads = vec![
        used.clone(),
        "plain_field".to_string(),
        callback,
        matched.clone(),
    ];
    let graph = graph_for(
        "src/a.rs",
        vec![
            type_node("Child", &["Base", "Iface"], &[]),
            type_node("Lone", &[], &[]),
            rn("paint", None, &[]),
            RawNode {
                field_reads: reads.clone(),
                ..rn("uses", None, &[])
            },
        ],
        vec![],
    );
    write_dirty_fragments_from_graphs(&session_dir, &[graph], &[mtime]).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert_eq!(inputs.len(), 1);
    let sym = |name: &str| inputs[0].symbols.iter().find(|s| s.name == name).unwrap();
    assert_eq!(sym("Child").heritage, ["Base", "Iface"], "declared order");
    assert!(sym("Lone").heritage.is_empty(), "a type with no bases");
    assert!(sym("paint").variant_refs.is_empty());
    assert_eq!(
        sym("uses").variant_refs,
        reads,
        "all field reads, including tagged name references, stay raw and in order"
    );
}

/// A typed dirty factory enters the session through the writer and reader,
/// rather than the direct `LocalGraph -> OverlayFileInput` conversion used by
/// resolver parity tests. Its return type is required for `make().bar()`.
#[test]
fn test_write_dirty_fragments_factory_return_type_roundtrips() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "return-sid");
    let mtime = write_worktree_file(&worktree, "src/factory.ts");
    let graph = TypeScriptProvider::new()
        .unwrap()
        .parse_file(
            Path::new("src/factory.ts"),
            b"export class Other { bar(): void {} }\nexport function make(): Other { return new Other(); }\n",
        )
        .unwrap();
    write_dirty_fragments_from_graphs(&session_dir, &[graph], &[mtime]).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree);
    let factory = inputs[0]
        .symbols
        .iter()
        .find(|symbol| symbol.name == "make")
        .unwrap();
    assert_eq!(factory.return_type.as_deref(), Some("Other"));
}

/// Two writes for one file leave one manifest entry, the later one: the
/// reader serves that fragment's heritage, never the first write's.
#[test]
fn test_load_view_inputs_two_fragments_for_one_file_serves_the_later() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "two-sid");
    let mtime = write_worktree_file(&worktree, "src/a.rs");
    let mut first = graph_for("src/a.rs", vec![type_node("T", &["Old"], &[])], vec![]);
    first.content_hash = [1; 8];
    let mut second = graph_for("src/a.rs", vec![type_node("T", &["New"], &[])], vec![]);
    second.content_hash = [2; 8];
    write_dirty_fragments_from_graphs(&session_dir, &[first], &[mtime]).unwrap();
    write_dirty_fragments_from_graphs(&session_dir, &[second], &[mtime]).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].symbols[0].heritage, ["New"]);
}

/// A session whose `l1/dirty_events.json` is stamped with `fingerprint` and names
/// one v3 bin (`old_fn`, written with the v3 shape: no heritage, no variant
/// refs) for `src/a.rs`. Returns `(session_dir, worktree)`.
fn session_with_v3_bin(tmp: &Path, fingerprint: &str) -> (PathBuf, PathBuf) {
    let worktree = tmp.join("wt");
    let mtime = write_worktree_file(&worktree, "src/a.rs");
    let session_dir = make_session_dir(tmp, "v3-sid");
    let file = FragmentFileV3 {
        symbols: vec![FragmentSymbolV3 {
            name: "old_fn".into(),
            kind: NodeKind::Function,
            span: (1, 0, 5, 0),
            owner_class: None,
            is_exported: true,
            calls: vec![],
        }],
        imports: vec![],
        export_aliases: vec![],
    };
    std::fs::create_dir_all(layout::fragments_dir(&session_dir)).unwrap();
    std::fs::write(
        layout::fragment_path(&session_dir, "v3frag"),
        rkyv::to_bytes::<rkyv::rancor::Error>(&file).unwrap(),
    )
    .unwrap();
    let manifest = layout::dirty_events_path(&session_dir);
    let mut df = DirtyEvents::read(&manifest).unwrap();
    df.builder_fingerprint = fingerprint.to_string();
    df.entries.insert(
        "src/a.rs".into(),
        DirtyEventEntry {
            change: PathChange::Modified,
            class: PathClass::SOURCE,
            mtime_ns: mtime,
            content_hash: Some("h".into()),
            fragment_id: Some("v3frag".into()),
            parse_failed: false,
            dirty_symbols: vec![],
            format: FRAGMENT_FORMAT_V3,
        },
    );
    DirtyEvents::write_atomic(&manifest, &df).unwrap();
    (session_dir, worktree)
}

/// Contract: the view serves only fragments of the running format. A v3 bin
/// has no heritage, so a view built from it would show a class with no bases
/// and bind its members as if the class were unrelated to its parents.
#[test]
fn test_load_view_inputs_v3_entry_under_current_fingerprint_returns_no_input() {
    let tmp = tempfile::tempdir().unwrap();
    let (session_dir, worktree) =
        session_with_v3_bin(tmp.path(), ecp_core::registry::BUILDER_FINGERPRINT);
    assert!(DirtyEvents::read(&layout::dirty_events_path(&session_dir))
        .unwrap()
        .is_current(ecp_core::config::resolve_size_cap(&worktree)));

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert!(inputs.is_empty(), "a v3 fragment was served: {inputs:?}");
}

/// The session ecp 0.15.0 left for a dirty `app.py`
/// (`tests/fixtures/overlay-written-by-0.15.0`, see its README.txt): a v3 bin
/// in a legacy `v0.15.0+schema11` `dirty_files.json`. Only the recorded mtime
/// is moved to the copied file's, so the mtime check alone would accept it.
/// Returns
/// `(session_dir, worktree)`.
fn session_written_by_0_15_0(tmp: &Path) -> (PathBuf, PathBuf) {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/overlay-written-by-0.15.0");
    let worktree = tmp.join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::copy(fixture.join("app.py"), worktree.join("app.py")).unwrap();
    let mtime = std::fs::metadata(worktree.join("app.py"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let session_dir = make_session_dir(tmp, "sid-0-15-0");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture.join("dirty_files.json")).unwrap())
            .unwrap();
    manifest["entries"]["app.py"]["mtime_ns"] = mtime.into();
    std::fs::write(
        layout::legacy_dirty_files_path(&session_dir),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(layout::legacy_overlay_dir(&session_dir)).unwrap();
    std::fs::copy(
        fixture.join("graph_overlay/e4bdd84a3e2d78f5.bin"),
        layout::legacy_overlay_dir(&session_dir).join("e4bdd84a3e2d78f5.bin"),
    )
    .unwrap();
    (session_dir, worktree)
}

/// Contract: a manifest of the 0.15.0 release sits in the legacy namespace,
/// which the reader never opens, so the view serves the base graph. Pass: no
/// input, though the mtime matches. Fail: any input, which would bind calls
/// from a parse without heritage.
#[test]
fn test_load_view_inputs_session_written_by_0_15_0_returns_no_input() {
    let tmp = tempfile::tempdir().unwrap();
    let (session_dir, worktree) = session_written_by_0_15_0(tmp.path());
    let old = DirtyFiles::read(&layout::legacy_dirty_files_path(&session_dir)).unwrap();
    assert_eq!(old.builder_fingerprint.as_deref(), Some("v0.15.0+schema11"));
    assert_eq!(old.entries["app.py"].format, FRAGMENT_FORMAT_V3);
    assert!(!old.from_current_builder());

    let inputs = load_view_inputs(&session_dir, &worktree);
    assert!(inputs.is_empty(), "another builder's fragment: {inputs:?}");
}

/// The v4 bin ecp 0.16.2 left for a dirty `app.py`, with its entry carried into
/// `l1/dirty_events.json` under the running builder and cap: the one thing
/// that makes the reader reject it is the entry's format. V5 is the first
/// format with the facts required to rebuild dirty view edges.
fn session_written_by_0_16_2(tmp: &Path) -> (PathBuf, PathBuf) {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/overlay-written-by-0.16.2");
    let worktree = tmp.join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::copy(fixture.join("app.py"), worktree.join("app.py")).unwrap();
    let mtime = std::fs::metadata(worktree.join("app.py"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let session_dir = make_session_dir(tmp, "sid-0-16-2");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture.join("dirty_files.json")).unwrap())
            .unwrap();
    manifest["entries"]["app.py"]["mtime_ns"] = mtime.into();
    let legacy: DirtyFiles = serde_json::from_value(manifest).unwrap();
    let mut events = DirtyEvents::empty(ecp_core::config::resolve_size_cap(&worktree));
    events.entries = legacy
        .entries
        .iter()
        .map(|(path, entry)| (path.clone(), event_from_legacy(entry)))
        .collect();
    DirtyEvents::write_atomic(&layout::dirty_events_path(&session_dir), &events).unwrap();
    std::fs::create_dir_all(layout::fragments_dir(&session_dir)).unwrap();
    std::fs::copy(
        fixture.join("graph_overlay/2e3d1df0cf371fbf.bin"),
        layout::fragment_path(&session_dir, "2e3d1df0cf371fbf"),
    )
    .unwrap();
    (session_dir, worktree)
}

#[test]
fn test_load_view_inputs_v4_entry_under_current_fingerprint_returns_no_input() {
    let tmp = tempfile::tempdir().unwrap();
    let (session_dir, worktree) = session_written_by_0_16_2(tmp.path());
    let dirty = DirtyEvents::read(&layout::dirty_events_path(&session_dir)).unwrap();

    assert_eq!(dirty.entries["app.py"].format, FRAGMENT_FORMAT_V4);
    assert!(dirty.is_current(ecp_core::config::resolve_size_cap(&worktree)));
    assert!(
        load_view_inputs(&session_dir, &worktree).is_empty(),
        "v4 lacks the complete field reads and return types required by V5"
    );
}

/// The `m` of two traits `A` and `B` (`src/a.rs`, `src/b.rs`), both
/// default methods, and a clean `struct S`.
const RUST_TRAITS: &[(&str, &str)] = &[
    ("src/s.rs", "pub struct S;\n"),
    (
        "src/a.rs",
        "pub trait A {\n    fn m(&self) {}\n    fn a(&self);\n}\n",
    ),
    (
        "src/b.rs",
        "pub trait B {\n    fn m(&self) {}\n    fn b(&self);\n}\n",
    ),
];
const IMPL_A: &str = "use crate::s::S;\nuse crate::a::A;\nimpl A for S {\n    fn a(&self) {}\n}\n";
const IMPL_B: &str = "use crate::s::S;\nuse crate::b::B;\nimpl B for S {\n    fn b(&self) {}\n}\n";
const RUN: &str = "pub fn run(s: &S) {\n    s.m();\n}\n";

fn pipeline_parse(path: &str, source: &str) -> LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Contract: the supertypes of a clean type drop what a dirty file's old
/// content gave it. `src/imp.rs` switches `impl A for S` to `impl B for S`,
/// so a reindex sees `S: B` alone and binds `s.m()` to `B::m`. Fail: the
/// archived `A` beside the fresh `B` leaves two `m` and no edge.
#[test]
fn test_build_dirty_impl_switches_trait_binds_new_trait_member_like_reindex() {
    let (old, new) = (format!("{IMPL_A}{RUN}"), format!("{IMPL_B}{RUN}"));
    let files: Vec<(&str, &str)> = RUST_TRAITS
        .iter()
        .copied()
        .chain([("src/imp.rs", old.as_str())])
        .collect();
    let Parity { reindex, overlay } = calls_after_edit(
        &pipeline_parse,
        &files,
        &[("src/imp.rs", new.as_str())],
        &|file, name| file == "src/imp.rs" && name == "run",
    );
    assert_eq!(
        callees(&reindex),
        [("src/b.rs", "m", 2, 0.8)],
        "the reindex must bind B's `m`, or the fixture proves nothing"
    );
    assert_eq!(overlay, reindex);
}

/// Contract: a trait a clean file implements stays. `impl A for S` lives in
/// clean `src/ia.rs`, and the dirty `src/imp.rs` adds `impl B for S`: `S`
/// has `[A, B]`, two `m`, and the reindex binds none. Fail: an overlay edge
/// to `B::m`, which means the clean `A` was dropped.
#[test]
fn test_build_dirty_impl_beside_clean_impl_keeps_both_traits_like_reindex() {
    let (old, new) = (format!("use crate::s::S;\n{RUN}"), format!("{IMPL_B}{RUN}"));
    let files: Vec<(&str, &str)> = RUST_TRAITS
        .iter()
        .copied()
        .chain([("src/ia.rs", IMPL_A), ("src/imp.rs", old.as_str())])
        .collect();
    let Parity { reindex, overlay } = calls_after_edit(
        &pipeline_parse,
        &files,
        &[("src/imp.rs", new.as_str())],
        &|file, name| file == "src/imp.rs" && name == "run",
    );
    assert!(reindex.is_empty(), "two traits declare `m`: {reindex:#?}");
    assert_eq!(overlay, reindex);
}

/// Contract: the dirty-impl drop reaches Rust types only. Clean `s.py`
/// declares `class S(A)` beside the Rust `struct S` and `trait A`; the
/// dirty `src/imp.rs` removes `impl A for S`, and the dirty `run.py` calls
/// `self.m()` from `T(S)`. A reindex binds the Python `A.m`. Fail: no
/// overlay edge, which means the Rust removal dropped the Python heritage.
#[test]
fn test_build_dirty_impl_removed_keeps_python_namesake_heritage_like_reindex() {
    const PY_S: &str = "class A:\n    def m(self):\n        pass\nclass S(A):\n    pass\n";
    const PY_RUN_OLD: &str = "from s import S\n\nclass T(S):\n    def run(self):\n        pass\n";
    const PY_RUN_NEW: &str =
        "from s import S\n\nclass T(S):\n    def run(self):\n        self.m()\n";
    let files: Vec<(&str, &str)> = RUST_TRAITS
        .iter()
        .copied()
        .chain([
            ("src/imp.rs", IMPL_A),
            ("s.py", PY_S),
            ("run.py", PY_RUN_OLD),
        ])
        .collect();
    let Parity { reindex, overlay } = calls_after_edit(
        &pipeline_parse,
        &files,
        &[("src/imp.rs", "use crate::s::S;\n"), ("run.py", PY_RUN_NEW)],
        &|file, name| file == "run.py" && name == "run",
    );
    assert!(
        reindex
            .iter()
            .any(|edge| edge.callee_file == "s.py" && edge.callee == "m"),
        "the reindex must bind the Python `A.m`, or the fixture proves nothing: {reindex:#?}"
    );
    assert_eq!(overlay, reindex);
}
