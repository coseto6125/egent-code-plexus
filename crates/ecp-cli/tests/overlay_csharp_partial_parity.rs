//! A bare C# call in one part of a `partial` type (codex ruling P-d): the
//! session overlay binds the member another part declares, at the same
//! confidence as a full reindex (the partial-type tier, 0.85). Where the
//! overlay cannot see the grouping, because the edit added the part, it
//! reports the call ([`GAP_CS_PARTIAL_BARE_CALL`]).
//!
//! Partial types exist only in C#, so this dimension has no other language.

#[path = "common/parity.rs"]
mod parity;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::c_sharp::partial::CsharpProjects;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType};
use ecp_core::session::view::{CallGap, OverlayFileInput, OverlayView, GAP_CS_PARTIAL_BARE_CALL};
use parity::{callees, edges_after_edit_with, provider_parse, CallEdge};

/// A builder that knows `Lib` is one project: the index merges the parts
/// of a type only inside a known project.
fn builder() -> GraphBuilder {
    let mut projects = CsharpProjects::new(usize::MAX);
    projects.add("Lib", true);
    GraphBuilder::new().with_csharp_projects(projects)
}

/// One part of `partial class <decl>` in namespace `Lib`, with `body` as its
/// members on line 5.
fn part(decl: &str, body: &str) -> String {
    format!(
        "namespace Lib\n{{\n    public partial class {decl}\n    {{\n        {body}\n    }}\n}}\n"
    )
}

/// `source` with its members one line further down.
fn shifted(source: &str) -> String {
    source.replacen("    {\n", "    {\n\n", 1)
}

/// `Other` declares `Helper` too, so a name-only lookup binds neither.
const OTHER: (&str, &str) = (
    "Lib/Other.cs",
    "namespace Lib\n{\n    public class Other\n    {\n        public void Helper() {}\n    }\n}\n",
);

/// The `Calls` edges out of `Run`, reindex and overlay, with `builder`.
fn run_calls(files: &[(&str, &str)], edits: &[(&str, &str)]) -> (Vec<CallEdge>, Vec<CallEdge>) {
    let provider = CSharpProvider::new().unwrap();
    let parity = edges_after_edit_with(
        &builder,
        &provider_parse(&provider),
        files,
        edits,
        RelType::Calls,
        &|_, name| name == "Run",
    );
    let strip = |edges: Vec<parity::ReasonedEdge>| -> Vec<CallEdge> {
        edges.into_iter().map(|edge| edge.edge).collect()
    };
    (strip(parity.reindex), strip(parity.overlay))
}

/// [`parity::call_gaps_after_edit`] over a [`builder`] that knows `Lib`.
fn gaps_after_edit(files: &[(&str, &str)], edits: &[(&str, &str)]) -> Vec<CallGap> {
    let provider = CSharpProvider::new().unwrap();
    let parse = provider_parse(&provider);
    let mut base = builder();
    for (path, source) in files {
        base.add_graph(parse(path, source));
    }
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base.build()).unwrap();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let inputs: Vec<OverlayFileInput> = edits
        .iter()
        .map(|(path, source)| parse(path, source).into())
        .collect();
    let view = OverlayView::build(graph, &inputs).expect("edits make a view");
    let cached = OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len())
        .expect("view cache round trip");
    cached.call_gaps().to_vec()
}

/// Stands for: a dirty part's bare `Helper()` binds the `Helper` the other
/// part declares (line 5) at 0.85 on both sides, and the overlay reports no
/// fallback. Before the fix the overlay had no partial-type tier: two
/// global candidates, no edge.
#[test]
fn test_overlay_partial_bare_call_binds_other_part_member_like_reindex() {
    let main = part("Reader", "public void Run() { Helper(); }");
    let async_part = part("Reader", "private void Helper() {}");
    let files = [
        ("Lib/Reader.cs", main.as_str()),
        ("Lib/Reader.Async.cs", async_part.as_str()),
        OTHER,
    ];
    let edited = shifted(&main);
    let edits = [("Lib/Reader.cs", edited.as_str())];
    let (reindex, overlay) = run_calls(&files, &edits);
    assert_eq!(
        callees(&reindex),
        [("Lib/Reader.Async.cs", "Helper", 5, 0.85)]
    );
    assert_eq!(overlay, reindex);
    assert_eq!(gaps_after_edit(&files, &edits), Vec::<CallGap>::new());
}

/// Stands for: the overlay groups a `partial` attribute class as the index
/// does (the `Annotation` part merges as a class), so a bare call in the
/// plain part binds the attribute part's member on both sides.
#[test]
fn test_overlay_partial_attribute_class_bare_call_binds_attribute_part_member() {
    let attribute = part(
        "MarkAttribute : System.Attribute",
        "public void Helper() {}",
    );
    let plain = part("MarkAttribute", "public void Run() { Helper(); }");
    let files = [
        ("Lib/Mark.Attr.cs", attribute.as_str()),
        ("Lib/Mark.More.cs", plain.as_str()),
        OTHER,
    ];
    let edited = shifted(&plain);
    let edits = [("Lib/Mark.More.cs", edited.as_str())];
    let (reindex, overlay) = run_calls(&files, &edits);
    assert_eq!(callees(&reindex), [("Lib/Mark.Attr.cs", "Helper", 5, 0.85)]);
    assert_eq!(overlay, reindex);
}

/// Stands for: a part the edit adds is invisible to the overlay, which
/// keeps the last index's grouping. The reindex merges the new part and
/// binds its `Helper` at 0.85; the overlay cannot, and reports the call.
#[test]
fn test_overlay_partial_part_added_by_edit_reports_gap() {
    let main = part("Reader", "public void Run() { Helper(); }");
    let async_part = part("Reader", "private void Helper() {}");
    let files = [("Lib/Reader.cs", main.as_str()), OTHER];
    let edited = shifted(&main);
    let edits = [
        ("Lib/Reader.cs", edited.as_str()),
        ("Lib/Reader.Async.cs", async_part.as_str()),
    ];
    let (reindex, _) = run_calls(&files, &edits);
    assert_eq!(
        callees(&reindex),
        [("Lib/Reader.Async.cs", "Helper", 5, 0.85)]
    );
    let gaps = gaps_after_edit(&files, &edits);
    let want = CallGap {
        path: "Lib/Reader.cs".to_string(),
        line: 6,
        caller: "Run".to_string(),
        member: "Helper".to_string(),
        reason: GAP_CS_PARTIAL_BARE_CALL.to_string(),
    };
    assert_eq!(gaps, [want]);
}

/// Guard: a same-named type of another namespace is no part, so a bare
/// call the overlay leaves unbound reports nothing.
#[test]
fn test_overlay_same_name_type_other_namespace_reports_no_gap() {
    let main = part("Reader", "public void Run() { Helper(); }");
    let elsewhere = "namespace App\n{\n    public class Reader\n    {\n        public void Helper() {}\n    }\n}\n";
    let files = [
        ("Lib/Reader.cs", main.as_str()),
        ("App/Reader.cs", elsewhere),
        OTHER,
    ];
    let edited = shifted(&main);
    let edits = [("Lib/Reader.cs", edited.as_str())];
    let (reindex, overlay) = run_calls(&files, &edits);
    assert_eq!(overlay, reindex);
    assert_eq!(gaps_after_edit(&files, &edits), Vec::<CallGap>::new());
}
