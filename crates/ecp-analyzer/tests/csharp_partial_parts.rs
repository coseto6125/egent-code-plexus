//! C# `partial` types: the parts of one type keep their own nodes, and one
//! deterministic representative stands for the type (codex ruling P-part).
//!
//! - Every other part has one `PartOf` edge to the representative.
//! - `Extends` from a subclass ends at the representative.
//! - The representative is the part that declares a base list, else the part
//!   whose file stem is the type name, else the first normalized path.
//! - The file order and the node-id order change no representative.
//! - A chained call whose receiver type is a partial type binds the member
//!   in the part that declares it, in the full build and in the overlay.
//! - Parts in another project, of an unknown project, of another arity,
//!   scope or kind, or without `partial` join no group (P-d's merge key).
//!
//! Partial types exist only in C#, so this dimension has no other language.

mod overrides_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::c_sharp::partial::{partial_groups, CsharpProjects};
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::RawIndexMap;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use overrides_support::{edge_ends, graph_of_projects};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;

fn csharp() -> CSharpProvider {
    CSharpProvider::new().unwrap()
}

/// One part of `partial class <decl>` in namespace `Lib`, with `body` as its
/// members.
fn part(decl: &str, body: &str) -> String {
    format!(
        "namespace Lib\n{{\n    public partial class {decl}\n    {{\n        {body}\n    }}\n}}\n"
    )
}

/// `(source file, target file)` of every `PartOf` edge, sorted.
fn part_of_files(graph: &ZeroCopyGraph) -> Vec<(String, String)> {
    let mut ends: Vec<(String, String)> = edge_ends(graph, RelType::PartOf)
        .into_iter()
        .map(|(source, target)| (source.file, target.file))
        .collect();
    ends.sort();
    ends
}

/// [`part_of_files`] of the graph of `files` in projects `projects`, after
/// asserting that the files in reverse order build the same edges.
fn part_of_files_any_order(files: &[(&str, &str)], projects: &[&str]) -> Vec<(String, String)> {
    let forward = part_of_files(&graph_of_projects(&csharp(), files, projects));
    let reversed: Vec<(&str, &str)> = files.iter().rev().copied().collect();
    assert_eq!(
        part_of_files(&graph_of_projects(&csharp(), &reversed, projects)),
        forward,
        "the file order changes no PartOf edge"
    );
    forward
}

fn pair(source: &str, target: &str) -> (String, String) {
    (source.to_string(), target.to_string())
}

/// The file of the representative of every group `partial_groups` finds,
/// with the local graphs in the order given: no builder sort, so the node
/// ids follow the order given.
fn representative_files(files: &[(&str, &str)], project: &str) -> Vec<String> {
    let graphs: Vec<LocalGraph> = files
        .iter()
        .map(|(path, src)| {
            csharp()
                .parse_file(Path::new(path), src.as_bytes())
                .unwrap()
        })
        .collect();
    let raw_map = RawIndexMap::new(0, graphs.iter().map(|lg| lg.nodes.len())).unwrap();
    let mut projects = CsharpProjects::new(usize::MAX);
    projects.add(project, true);
    let mut starts = Vec::new();
    let mut next = 0u32;
    for lg in &graphs {
        starts.push(next);
        next += lg.nodes.len() as u32;
    }
    partial_groups(&graphs, &raw_map, &projects)
        .iter()
        .map(|group| {
            let file = starts.partition_point(|&start| start <= group[0]) - 1;
            graphs[file].file_path.to_string_lossy().into_owned()
        })
        .collect()
}

/// Stands for: the part that declares a base list is the representative,
/// even when another part's file stem is the type name and sorts first.
/// Before the fix no `PartOf` edge existed, and the lowest node id
/// (`Lib/Reader.cs`) stood for the type.
#[test]
fn test_partial_groups_base_list_part_is_representative() {
    let main = part("Reader", "public void Read() {}");
    let io = part("Reader : System.IDisposable", "public void Dispose() {}");
    assert_eq!(
        part_of_files_any_order(
            &[
                ("Lib/Reader.cs", main.as_str()),
                ("Lib/Reader.io.cs", io.as_str())
            ],
            &["Lib"]
        ),
        [pair("Lib/Reader.cs", "Lib/Reader.io.cs")]
    );
}

/// Stands for: with no base list, the part whose file stem is the type name
/// is the representative (Newtonsoft `JsonReader.cs` beside
/// `JsonReader.Async.cs`). Before the fix the lowest node id,
/// `Reader.Async.cs`, stood for the type.
#[test]
fn test_partial_groups_stem_part_is_representative() {
    let main = part("Reader", "public void Read() {}");
    let async_part = part("Reader", "public void ReadAsync() {}");
    assert_eq!(
        part_of_files_any_order(
            &[
                ("Lib/Reader.Async.cs", async_part.as_str()),
                ("Lib/Reader.cs", main.as_str())
            ],
            &["Lib"]
        ),
        [pair("Lib/Reader.Async.cs", "Lib/Reader.cs")]
    );
}

/// Stands for: two parts that both declare a base list tie on the first
/// tier, and the file stem breaks the tie, as every tier breaks its ties by
/// the next.
#[test]
fn test_partial_groups_two_base_lists_stem_breaks_tie() {
    let main = part("Reader : System.IDisposable", "public void Dispose() {}");
    let async_part = part(
        "Reader : System.IAsyncDisposable",
        "public void ReadAsync() {}",
    );
    assert_eq!(
        part_of_files_any_order(
            &[
                ("Lib/Reader.Async.cs", async_part.as_str()),
                ("Lib/Reader.cs", main.as_str())
            ],
            &["Lib"]
        ),
        [pair("Lib/Reader.Async.cs", "Lib/Reader.cs")]
    );
}

/// Stands for: with no base list and no file named after the type, the
/// first normalized path is the representative; the other parts point at
/// it, one edge each.
#[test]
fn test_partial_groups_no_base_no_stem_first_path_is_representative() {
    let one = part("Reader", "public void A() {}");
    let two = part("Reader", "public void B() {}");
    let three = part("Reader", "public void C() {}");
    assert_eq!(
        part_of_files_any_order(
            &[
                ("Lib/b/ReaderB.cs", two.as_str()),
                ("Lib/c/ReaderC.cs", three.as_str()),
                ("Lib/a/ReaderA.cs", one.as_str()),
            ],
            &["Lib"]
        ),
        [
            pair("Lib/b/ReaderB.cs", "Lib/a/ReaderA.cs"),
            pair("Lib/c/ReaderC.cs", "Lib/a/ReaderA.cs"),
        ]
    );
}

/// Stands for: the node-id order changes no representative. `partial_groups`
/// numbers the nodes in the order given, so the reversed list gives the
/// representative another id, never another file.
#[test]
fn test_partial_groups_reversed_node_ids_same_representative() {
    let main = part("Reader", "public void Read() {}");
    let async_part = part("Reader", "public void ReadAsync() {}");
    let forward = [
        ("Lib/Reader.cs", main.as_str()),
        ("Lib/Reader.Async.cs", async_part.as_str()),
    ];
    let reversed = [forward[1], forward[0]];
    assert_eq!(representative_files(&forward, "Lib"), ["Lib/Reader.cs"]);
    assert_eq!(representative_files(&reversed, "Lib"), ["Lib/Reader.cs"]);
}

/// Stands for: two parts in one file are one node, so the group holds that
/// node once and gets one `PartOf` edge, from the other file's part.
#[test]
fn test_partial_groups_two_parts_in_one_file_one_edge() {
    let twice = "namespace Lib\n{\n    public partial class Reader\n    {\n        public void A() {}\n    }\n    public partial class Reader\n    {\n        public void B() {}\n    }\n}\n";
    let async_part = part("Reader", "public void ReadAsync() {}");
    assert_eq!(
        part_of_files_any_order(
            &[
                ("Lib/Reader.cs", twice),
                ("Lib/Reader.Async.cs", async_part.as_str())
            ],
            &["Lib"]
        ),
        [pair("Lib/Reader.Async.cs", "Lib/Reader.cs")]
    );
}

/// A merge-key case: its label, its two files, and the dirs that hold a
/// `.csproj`.
type MismatchCase<'a> = (&'a str, [(&'a str, &'a str); 2], &'a [&'a str]);

/// Stands for: P-d's merge key holds for the part relation. Parts in two
/// projects, parts of an unknown project, a `partial` beside a plain
/// declaration, another generic arity, another declaring scope (namespace
/// or enclosing type) and another kind get no `PartOf` edge. Each case
/// passes before the fix as well: it guards the shapes beside the scope.
#[test]
fn test_partial_groups_merge_key_mismatch_no_part_of_edge() {
    let main = part("Reader", "public void Read() {}");
    let async_part = part("Reader", "public void ReadAsync() {}");
    let plain = "namespace Lib\n{\n    public class Reader\n    {\n        public void ReadAsync() {}\n    }\n}\n";
    let generic = part("Reader<T>", "public void ReadAsync() {}");
    let other_namespace = "namespace Other\n{\n    public partial class Reader\n    {\n        public void ReadAsync() {}\n    }\n}\n";
    let nested = "namespace Lib\n{\n    public partial class Outer\n    {\n        public partial class Reader\n        {\n            public void ReadAsync() {}\n        }\n    }\n}\n";
    let as_struct = "namespace Lib\n{\n    public partial struct Reader\n    {\n        public void ReadAsync() {}\n    }\n}\n";
    let cases: [MismatchCase; 7] = [
        (
            "two projects",
            [
                ("P1/Reader.cs", main.as_str()),
                ("P2/Reader.Async.cs", async_part.as_str()),
            ],
            &["P1", "P2"],
        ),
        (
            "unknown project",
            [
                ("Lib/Reader.cs", main.as_str()),
                ("Lib/Reader.Async.cs", async_part.as_str()),
            ],
            &[],
        ),
        (
            "plain declaration",
            [
                ("Lib/Reader.cs", main.as_str()),
                ("Lib/Reader.Async.cs", plain),
            ],
            &["Lib"],
        ),
        (
            "generic arity",
            [
                ("Lib/Reader.cs", main.as_str()),
                ("Lib/ReaderOfT.cs", generic.as_str()),
            ],
            &["Lib"],
        ),
        (
            "namespace",
            [
                ("Lib/Reader.cs", main.as_str()),
                ("Lib/Other.cs", other_namespace),
            ],
            &["Lib"],
        ),
        (
            "enclosing type",
            [("Lib/Reader.cs", main.as_str()), ("Lib/Outer.cs", nested)],
            &["Lib"],
        ),
        (
            "kind",
            [
                ("Lib/Reader.cs", main.as_str()),
                ("Lib/ReaderStruct.cs", as_struct),
            ],
            &["Lib"],
        ),
    ];
    for (shape, files, projects) in cases {
        assert_eq!(
            part_of_files_any_order(&files, projects),
            [],
            "{shape}: two types, no part relation"
        );
    }
}

/// The Newtonsoft `JsonReader` shape: the stem part declares `ReadAsDecimal`,
/// the async part `ReadAsDecimalAsync`; `TextReader : Reader`; a factory
/// returns `Reader`; `Use.Run` calls both members through the factory.
/// `Other` declares both names too, so a name-only fallback binds neither.
fn reader_files() -> Vec<(&'static str, String)> {
    vec![
        (
            "Lib/Reader.cs",
            part("Reader", "public decimal? ReadAsDecimal() { return null; }"),
        ),
        (
            "Lib/Reader.Async.cs",
            part("Reader", "public void ReadAsDecimalAsync() {}"),
        ),
        (
            "Lib/TextReader.cs",
            "namespace Lib\n{\n    public class TextReader : Reader\n    {\n        public void Peek() {}\n    }\n}\n".to_string(),
        ),
        (
            "Lib/Factory.cs",
            "namespace Lib\n{\n    public static class Factory\n    {\n        public static Reader Create() { return null; }\n    }\n}\n".to_string(),
        ),
        (
            "Lib/Other.cs",
            "namespace Lib\n{\n    public class Other\n    {\n        public decimal? ReadAsDecimal() { return null; }\n        public void ReadAsDecimalAsync() {}\n    }\n}\n".to_string(),
        ),
        (
            "Lib/Use.cs",
            "namespace Lib\n{\n    public class Use\n    {\n        public void Run()\n        {\n            Factory.Create().ReadAsDecimal();\n            Factory.Create().ReadAsDecimalAsync();\n        }\n    }\n}\n".to_string(),
        ),
    ]
}

fn reader_graph() -> ZeroCopyGraph {
    let files = reader_files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, s)| (*p, s.as_str())).collect();
    graph_of_projects(&csharp(), &refs, &["Lib"])
}

fn archive(graph: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(graph).unwrap()
}

fn access(bytes: &rkyv::util::AlignedVec) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap()
}

/// Stands for: `Extends` from a subclass ends at the representative, one
/// edge. Before the fix it ended at the lowest node id, `Reader.Async.cs`.
#[test]
fn test_resolve_base_csharp_partial_subclass_extends_representative() {
    let graph = reader_graph();
    let extends: Vec<(String, String)> = edge_ends(&graph, RelType::Extends)
        .into_iter()
        .filter(|(source, _)| source.name == "TextReader")
        .map(|(_, target)| (target.file, target.name))
        .collect();
    assert_eq!(extends, [pair("Lib/Reader.cs", "Reader")]);
}

/// `(file, name)` of every member that a reader reaches from `ty` across
/// the part relation ([`MergedGraph::partial_parts`]), each node once, as
/// `ecp inspect` lists them.
fn members_across_parts(merged: &MergedGraph<'_>, ty: u32) -> Vec<(String, String)> {
    let mut parts = merged.partial_parts(ty);
    if parts.is_empty() {
        parts.push(ty);
    }
    let mut seen = FxHashSet::default();
    let mut members: Vec<(String, String)> = parts
        .iter()
        .flat_map(|&part| merged.out_edges(part))
        .filter(|edge| edge.rel_type() == RelType::HasMethod && seen.insert(edge.target))
        .map(|edge| {
            let node = merged.node(edge.target).unwrap();
            (
                node.file_path(merged).unwrap().to_string(),
                node.name(merged).to_string(),
            )
        })
        .collect();
    members.sort();
    members
}

/// Stands for: an agent that follows `Extends` from a subclass reaches the
/// members of every part, each once, and a walk that starts at either part
/// reaches the same members. Before the fix no edge linked the parts, so
/// the async part's member was unreachable from `TextReader`.
#[test]
fn test_partial_parts_extends_from_subclass_reaches_every_part_member_once() {
    let bytes = archive(&reader_graph());
    let graph = access(&bytes);
    let merged = MergedGraph::new(graph, None);
    let text_reader = (0..merged.base_len())
        .find(|&i| merged.node(i).unwrap().name(&merged) == "TextReader")
        .unwrap();
    let base = merged
        .out_edges(text_reader)
        .find(|edge| edge.rel_type() == RelType::Extends)
        .unwrap()
        .target;
    let expected = [
        pair("Lib/Reader.Async.cs", "ReadAsDecimalAsync"),
        pair("Lib/Reader.cs", "ReadAsDecimal"),
    ];
    assert_eq!(members_across_parts(&merged, base), expected);
    let parts = merged.partial_parts(base);
    assert_eq!(parts.len(), 2, "two parts, the representative first");
    assert_eq!(parts[0], base, "Extends ends at the representative");
    assert_eq!(members_across_parts(&merged, parts[1]), expected);
    assert_eq!(
        merged.partial_parts(parts[1]),
        parts,
        "either part gives one group"
    );
}

/// `(file, name)` of every `Calls` target from a node named `caller`, sorted.
fn call_targets(merged: &MergedGraph<'_>, caller: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Calls)
        .filter(|edge| merged.node(edge.source).unwrap().name(merged) == caller)
        .map(|edge| {
            let node = merged.node(edge.target).unwrap();
            (
                node.file_path(merged).unwrap().to_string(),
                node.name(merged).to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

/// What `Use.Run` must call: the factory twice, and each member in the part
/// that declares it, never `Other`'s.
fn expected_run_calls() -> Vec<(String, String)> {
    vec![
        pair("Lib/Factory.cs", "Create"),
        pair("Lib/Factory.cs", "Create"),
        pair("Lib/Reader.Async.cs", "ReadAsDecimalAsync"),
        pair("Lib/Reader.cs", "ReadAsDecimal"),
    ]
}

/// Stands for: a chained call whose receiver's return type is a partial
/// type binds through the one grouping (`type_ladder::visible_return_type`).
/// Before the fix the two parts were two candidate types, the ladder found
/// none, and neither member call got an edge (Newtonsoft
/// `token.CreateReader().ReadAsDecimal()`, `ReadTests.cs:91`).
#[test]
fn test_resolve_chained_member_csharp_partial_return_type_binds_declaring_part() {
    let bytes = archive(&reader_graph());
    let merged = MergedGraph::new(access(&bytes), None);
    let mut calls = call_targets(&merged, "Run");
    calls.dedup();
    let mut expected = expected_run_calls();
    expected.dedup();
    assert_eq!(calls, expected);
}

/// `(source file, target file)` of every `PartOf` edge of the merged graph.
fn merged_part_of_files(merged: &MergedGraph<'_>) -> Vec<(String, String)> {
    let file = |idx: u32| {
        merged
            .node(idx)
            .unwrap()
            .file_path(merged)
            .unwrap()
            .to_string()
    };
    let mut ends: Vec<(String, String)> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::PartOf)
        .map(|edge| (file(edge.source), file(edge.target)))
        .collect();
    ends.sort();
    ends
}

/// Stands for: the overlay reads the stored grouping. With the caller file
/// and the async part both dirty (content unchanged), the overlay keeps the
/// full build's `PartOf` edge and binds both chained calls to the parts
/// that declare them. Before the fix the overlay had no grouping, the two
/// parts were two candidate types, and neither call got an edge.
#[test]
fn test_overlay_view_csharp_partial_parts_match_full_build() {
    let full = reader_graph();
    let full_part_of = part_of_files(&full);
    assert_eq!(full_part_of, [pair("Lib/Reader.Async.cs", "Lib/Reader.cs")]);
    let bytes = archive(&full);
    let base = access(&bytes);
    let files = reader_files();
    let dirty: Vec<OverlayFileInput> = files
        .iter()
        .filter(|(path, _)| matches!(*path, "Lib/Use.cs" | "Lib/Reader.Async.cs"))
        .map(|(path, src)| {
            OverlayFileInput::from(
                csharp()
                    .parse_file(Path::new(path), src.as_bytes())
                    .unwrap(),
            )
        })
        .collect();
    let view = OverlayView::build(base, &dirty).expect("a dirty view");
    let merged = MergedGraph::new(base, Some(&view));
    assert_eq!(merged_part_of_files(&merged), full_part_of);
    let mut calls = call_targets(&merged, "Run");
    calls.dedup();
    let mut expected = expected_run_calls();
    expected.dedup();
    assert_eq!(calls, expected);
}

/// Stands for: the incremental path reads the same grouping. With every
/// symbol of both parts skipped as unchanged (`with_symbol_skip_set`), the
/// build keeps the full build's `PartOf` edge, and `TextReader`, which
/// re-resolves, extends the same representative.
#[test]
fn test_graph_builder_symbol_skip_set_partial_parts_match_full_build() {
    let full = reader_graph();
    let pool = full.string_pool.as_slice();
    let mut skip: FxHashMap<String, FxHashSet<u64>> = FxHashMap::default();
    for node in &full.nodes {
        let path = full.files[node.file_idx as usize].path.resolve(pool);
        if matches!(path, "Lib/Reader.cs" | "Lib/Reader.Async.cs") {
            skip.entry(path.to_string()).or_default().insert(node.uid);
        }
    }
    let mut projects = CsharpProjects::new(usize::MAX);
    projects.add("Lib", true);
    let mut builder = GraphBuilder::new()
        .with_csharp_projects(projects)
        .with_symbol_skip_set(skip);
    for (path, src) in reader_files() {
        builder.add_graph(
            csharp()
                .parse_file(Path::new(path), src.as_bytes())
                .unwrap(),
        );
    }
    let incremental = builder.build();
    assert_eq!(part_of_files(&incremental), part_of_files(&full));
    let extends = |graph: &ZeroCopyGraph| -> Vec<(String, String)> {
        edge_ends(graph, RelType::Extends)
            .into_iter()
            .map(|(source, target)| (source.name, target.file))
            .collect()
    };
    assert_eq!(extends(&incremental), extends(&full));
    assert_eq!(extends(&full), [pair("TextReader", "Lib/Reader.cs")]);
}
