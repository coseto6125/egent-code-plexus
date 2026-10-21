//! Full reindex vs. session overlay over the same edited tree, compared
//! edge by edge (FU-2026-10-15-61d656576fc8). Shared by the overlay parity
//! tests through `#[path]`.

#![allow(dead_code)]

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use std::path::Path;

/// One edge by identity, never by node index or uid (a uid collision would
/// hide on both sides alike): source and target as file, kind, name and
/// 1-based start line. The confidence compares exactly. The `Calls` tests
/// name the source the caller and the target the callee.
#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct CallEdge {
    pub caller_file: String,
    pub caller: String,
    pub caller_kind: &'static str,
    pub caller_line: u32,
    pub callee_file: String,
    pub callee: String,
    pub callee_kind: &'static str,
    pub callee_line: u32,
    pub confidence: f32,
}

/// A [`CallEdge`] of any rel with its reason. An overlay `Calls` edge carries
/// the uncommitted-edit marker instead of the index's reason, so only rels
/// whose reasons the overlay rebuilds verbatim (`References`) compare them.
#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct ReasonedEdge {
    pub edge: CallEdge,
    pub reason: String,
}

/// The `Calls` edges whose caller `selects` (by file and name), from the
/// full reindex of the edited tree and from the overlay of the edits over
/// the base index.
#[derive(Debug)]
pub struct Parity {
    pub reindex: Vec<CallEdge>,
    pub overlay: Vec<CallEdge>,
}

/// [`Parity`] for one rel, reasons included.
#[derive(Debug)]
pub struct EdgeParity {
    pub reindex: Vec<ReasonedEdge>,
    pub overlay: Vec<ReasonedEdge>,
}

fn edges(
    graph: &ArchivedZeroCopyGraph,
    merged: &MergedGraph<'_>,
    rel: RelType,
    selects: &dyn Fn(&str, &str) -> bool,
) -> Vec<ReasonedEdge> {
    let mut edges: Vec<ReasonedEdge> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == rel)
        .filter_map(|edge| {
            let caller = merged.node(edge.source)?;
            let callee = merged.node(edge.target)?;
            let caller_file = caller.file_path(merged)?;
            let caller_name = caller.name(merged);
            selects(caller_file, caller_name).then(|| ReasonedEdge {
                edge: CallEdge {
                    caller_file: caller_file.to_string(),
                    caller: caller_name.to_string(),
                    caller_kind: caller.kind().as_str(),
                    caller_line: caller.start_line(),
                    callee_file: callee.file_path(merged).unwrap_or_default().to_string(),
                    callee: callee.name(merged).to_string(),
                    callee_kind: callee.kind().as_str(),
                    callee_line: callee.start_line(),
                    confidence: edge.confidence(),
                },
                reason: edge.reason(graph).to_string(),
            })
        })
        .collect();
    edges.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    edges.dedup();
    edges
}

/// `files` with `edits` applied: an edit replaces a file's source or adds a
/// file.
fn edited_tree<'s>(
    files: &[(&'s str, &'s str)],
    edits: &[(&'s str, &'s str)],
) -> Vec<(&'s str, &'s str)> {
    let mut edited: Vec<(&str, &str)> = files
        .iter()
        .map(|&(path, source)| {
            edits
                .iter()
                .find(|(edited, _)| *edited == path)
                .map_or((path, source), |&(_, new)| (path, new))
        })
        .collect();
    edited.extend(
        edits
            .iter()
            .filter(|(path, _)| files.iter().all(|(file, _)| file != path)),
    );
    edited
}

/// Index `files` with builders from `new_builder`, parsed by `parse`, and
/// compare the `rel` edges whose source `selects` between the reindex of the
/// edited tree and the overlay of `edits` over the base index.
pub fn edges_after_edit_with(
    new_builder: &dyn Fn() -> GraphBuilder,
    parse: &dyn Fn(&str, &str) -> LocalGraph,
    files: &[(&str, &str)],
    edits: &[(&str, &str)],
    rel: RelType,
    selects: &dyn Fn(&str, &str) -> bool,
) -> EdgeParity {
    let build_all = |tree: &[(&str, &str)]| {
        let mut builder = new_builder();
        for (path, source) in tree {
            builder.add_graph(parse(path, source));
        }
        builder.build()
    };
    let full =
        rkyv::to_bytes::<rkyv::rancor::Error>(&build_all(&edited_tree(files, edits))).unwrap();
    let full = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&full).unwrap();
    let reindex = edges(full, &MergedGraph::new(full, None), rel, selects);

    let base = rkyv::to_bytes::<rkyv::rancor::Error>(&build_all(files)).unwrap();
    let base = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&base).unwrap();
    let inputs: Vec<OverlayFileInput> = edits
        .iter()
        .map(|(path, source)| parse(path, source).into())
        .collect();
    let view = OverlayView::build(base, &inputs).expect("edits make a view");
    let overlay = edges(base, &MergedGraph::new(base, Some(&view)), rel, selects);
    EdgeParity { reindex, overlay }
}

/// [`edges_after_edit_with`] over a plain [`GraphBuilder`].
pub fn edges_after_edit(
    parse: &dyn Fn(&str, &str) -> LocalGraph,
    files: &[(&str, &str)],
    edits: &[(&str, &str)],
    rel: RelType,
    selects: &dyn Fn(&str, &str) -> bool,
) -> EdgeParity {
    edges_after_edit_with(&GraphBuilder::new, parse, files, edits, rel, selects)
}

/// Index `files`, parsed by `parse`; `edits` replace a file's source or
/// add a file.
pub fn calls_after_edit(
    parse: &dyn Fn(&str, &str) -> LocalGraph,
    files: &[(&str, &str)],
    edits: &[(&str, &str)],
    selects: &dyn Fn(&str, &str) -> bool,
) -> Parity {
    let EdgeParity { reindex, overlay } =
        edges_after_edit(parse, files, edits, RelType::Calls, selects);
    let strip = |edges: Vec<ReasonedEdge>| -> Vec<CallEdge> {
        let mut calls: Vec<CallEdge> = edges.into_iter().map(|e| e.edge).collect();
        calls.dedup();
        calls
    };
    Parity {
        reindex: strip(reindex),
        overlay: strip(overlay),
    }
}

/// `calls_after_edit` with one provider.
pub fn calls_after_edit_in(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    edits: &[(&str, &str)],
    selects: &dyn Fn(&str, &str) -> bool,
) -> Parity {
    calls_after_edit(&provider_parse(provider), files, edits, selects)
}

/// `provider`'s parse, as the parity helpers take it.
pub fn provider_parse(provider: &dyn LanguageProvider) -> impl Fn(&str, &str) -> LocalGraph + '_ {
    move |path, source| {
        provider
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap()
    }
}

pub fn named(caller: &'static str) -> impl Fn(&str, &str) -> bool {
    move |_, name| name == caller
}

/// A callee as the tests write it: `(file, name, line, confidence)`.
pub fn callees(edges: &[CallEdge]) -> Vec<(&str, &str, u32, f32)> {
    edges
        .iter()
        .map(|edge| {
            (
                edge.callee_file.as_str(),
                edge.callee.as_str(),
                edge.callee_line,
                edge.confidence,
            )
        })
        .collect()
}
