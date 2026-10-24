//! Contract C4: raw records (`docs/specs/contracts/C4-raw-records.md`).
//!
//! A raw record is a fact a parser emits beside its `RawNode`s: a path
//! literal, a SQL reference, an event topic, a transaction scope, a route,
//! a framework or fan-out reference, a call annotation, function metadata.
//! This module fixes four things that every producer and every consumer of
//! those records share:
//!
//! - the three parser phases, and in which phase a raw index is valid;
//! - the endpoint of a record: a node of the same file, the file itself, or
//!   unresolved;
//! - the map from a file-local raw index to a global node index, for the
//!   full build and for the session overlay;
//! - the evidence classes `Exact`, `Ambiguous`, `Heuristic`, `Unresolved`,
//!   and the confidence band each class may carry.
//!
//! No production code calls these types yet; each consuming slice adopts
//! them.

use std::cmp::Reverse;
use std::fmt;
use std::ops::Range;

use rkyv::{Archive, Deserialize, Serialize};

use crate::analyzer::types::{span_contains, RawNode, Span};
use crate::graph::{NodeKind, RelType};

// ── Parser phases ───────────────────────────────────────────────────────

/// The three phases of one file's parse, in order.
///
/// A raw index is the position of a node in the file's final
/// `LocalGraph.nodes`. Only `BindRecords` may write one, because only then
/// is the node list final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParsePhase {
    /// Run the queries and walks. Push nodes. A record found here keeps its
    /// source span (its site), never a node index.
    CollectCaptures,
    /// Make the node list final: dedup, reorder, promote kinds, stamp
    /// owners. After this phase the count, order, kind and span of every
    /// node are fixed.
    FinalizeNodes,
    /// Turn each record's site or name into a [`RawEndpoint`]. Nodes are
    /// read-only.
    BindRecords,
}

impl ParsePhase {
    pub const ORDER: [ParsePhase; 3] = [
        ParsePhase::CollectCaptures,
        ParsePhase::FinalizeNodes,
        ParsePhase::BindRecords,
    ];

    /// The phase after this one; `None` after `BindRecords`.
    pub const fn next(self) -> Option<ParsePhase> {
        match self {
            ParsePhase::CollectCaptures => Some(ParsePhase::FinalizeNodes),
            ParsePhase::FinalizeNodes => Some(ParsePhase::BindRecords),
            ParsePhase::BindRecords => None,
        }
    }

    /// Whether code in this phase may push, remove or reorder nodes, or
    /// change a node's kind or span.
    pub const fn may_change_nodes(self) -> bool {
        !matches!(self, ParsePhase::BindRecords)
    }

    /// Whether code in this phase may write a [`RawIdx`] into a record.
    pub const fn raw_indexes_valid(self) -> bool {
        matches!(self, ParsePhase::BindRecords)
    }

    /// Move to `to`. Only the next phase is allowed: a driver never skips a
    /// phase and never goes back.
    pub fn advance(self, to: ParsePhase) -> Result<ParsePhase, PhaseError> {
        match self.next() {
            Some(next) if next == to => Ok(to),
            _ => Err(PhaseError { from: self, to }),
        }
    }
}

/// A phase transition that skips a phase or goes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseError {
    pub from: ParsePhase,
    pub to: ParsePhase,
}

impl fmt::Display for PhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse phase {:?} cannot move to {:?}; the next phase is {:?}",
            self.from,
            self.to,
            self.from.next()
        )
    }
}

impl std::error::Error for PhaseError {}

// ── Raw index and endpoint ──────────────────────────────────────────────

/// Position of a node in one file's final `LocalGraph.nodes`. It is
/// file-local: the same value in two files names two different nodes.
/// [`RawIndexMap`] turns it into a global node index.
#[derive(
    Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[rkyv(derive(Debug))]
pub struct RawIdx(pub u32);

impl RawIdx {
    /// Largest raw index a 24-bit packed field holds (`RawTxScope`).
    pub const PACKED24_MAX: u32 = (1 << 24) - 1;

    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// The raw index of position `idx` in a list of `node_count` nodes;
    /// `None` when `idx` is out of range or does not fit in `u32`.
    pub fn checked(idx: usize, node_count: usize) -> Option<RawIdx> {
        u32::try_from(idx)
            .ok()
            .filter(|_| idx < node_count)
            .map(RawIdx)
    }

    /// The index for a 24-bit packed field. `None` above
    /// [`Self::PACKED24_MAX`]. The check runs in every build: a release
    /// build that packs `2^24` with `<< 8` shifts the high bits out and
    /// stores `0`, the file's first node. On `None` the producer drops the
    /// record and records a `BlindSpot`.
    pub fn to_packed24(self) -> Option<u32> {
        (self.0 <= Self::PACKED24_MAX).then_some(self.0)
    }
}

/// A name as the source writes it: trimmed and not empty. The field is
/// private, so [`SourceName::new`] is the only constructor outside this
/// module. [`RawEndpoint::validate`] checks a value read back from an
/// archive.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Hash)]
#[rkyv(derive(Debug))]
pub struct SourceName(Box<str>);

impl SourceName {
    /// The trimmed `name`; `None` when nothing is left.
    pub fn new(name: &str) -> Option<SourceName> {
        let name = name.trim();
        (!name.is_empty()).then(|| SourceName(name.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the invariant holds. Only an archived value can break it.
    pub fn is_valid(&self) -> bool {
        !self.0.is_empty() && self.0.trim().len() == self.0.len()
    }
}

/// Why an endpoint binds no node.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Hash)]
#[rkyv(derive(Debug))]
pub enum Unbound {
    /// The source names a declaration that no visible node of this file is.
    /// The spelling is kept, so a later pass can bind it across files (a
    /// Laravel `Ctrl@method` action, an imported handler).
    Named(SourceName),
    /// Code with no name and no node, such as an inline handler that the
    /// parser emits no closure node for. The record's own span gives the
    /// line.
    Anonymous,
}

/// The endpoint of a raw record, bound in [`ParsePhase::BindRecords`].
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Hash)]
#[rkyv(derive(Debug))]
pub enum RawEndpoint {
    /// A node of the same file, by its raw index.
    Node(RawIdx),
    /// The file itself: the record sits at module top level, or the fact
    /// belongs to the whole file. The builder maps it to the `File` node.
    File,
    /// No node of this file.
    Unresolved(Unbound),
}

impl RawEndpoint {
    /// An endpoint that names `name`, before any binding. `None` for an
    /// empty or whitespace-only name: the producer saw no reference.
    pub fn named(name: &str) -> Option<RawEndpoint> {
        SourceName::new(name).map(|name| RawEndpoint::Unresolved(Unbound::Named(name)))
    }

    pub fn node(&self) -> Option<RawIdx> {
        match self {
            RawEndpoint::Node(idx) => Some(*idx),
            RawEndpoint::File | RawEndpoint::Unresolved(_) => None,
        }
    }

    /// `Ok` when a `Node` endpoint is below `node_count`, the node count of
    /// the file the record belongs to, and a name is trimmed and not empty.
    pub fn validate(&self, node_count: usize) -> Result<(), EndpointError> {
        match self {
            RawEndpoint::Node(idx) if idx.index() >= node_count => Err(EndpointError::OutOfRange {
                idx: *idx,
                node_count,
            }),
            RawEndpoint::Unresolved(Unbound::Named(name)) if !name.is_valid() => {
                Err(EndpointError::BadName)
            }
            _ => Ok(()),
        }
    }
}

/// A [`RawEndpoint`] that breaks its invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    OutOfRange {
        idx: RawIdx,
        node_count: usize,
    },
    /// A name that is empty or not trimmed.
    BadName,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndpointError::OutOfRange { idx, node_count } => write!(
                f,
                "raw index {} is out of range for a file with {node_count} nodes",
                idx.0
            ),
            EndpointError::BadName => {
                f.write_str("an unresolved endpoint has an empty or untrimmed name")
            }
        }
    }
}

impl std::error::Error for EndpointError {}

// ── Binding ─────────────────────────────────────────────────────────────

/// The innermost node of a kind in `accept` whose span contains `site`.
///
/// Spans of one syntax tree nest or are disjoint, so the containers of one
/// site nest: the innermost has the latest start, then the earliest end.
/// Between identical spans the lower index wins, as in
/// [`crate::analyzer::types::innermost_enclosing`]. `None` when no accepted
/// node contains the site.
pub fn bind_enclosing(nodes: &[RawNode], site: Span, accept: &[NodeKind]) -> Option<RawIdx> {
    nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| accept.contains(&n.kind) && span_contains(n.span, site))
        .min_by_key(|(i, n)| {
            let (r1, c1, r2, c2) = n.span;
            (Reverse((r1, c1)), (r2, c2), *i)
        })
        .and_then(|(i, _)| RawIdx::checked(i, nodes.len()))
}

/// Kinds whose span opens a scope that can hide a nested declaration.
const SCOPE_CONTAINERS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Constructor,
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Enum,
    NodeKind::Impl,
    NodeKind::Module,
    NodeKind::Namespace,
];

/// Scope containers whose members a bare name does not reach in every
/// language: Python needs `self.`, Java an implicit `this`. Visibility
/// through them is never proven here; area 1's resolver owns those rules.
const TYPE_SCOPES: &[NodeKind] = &[
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Enum,
    NodeKind::Impl,
];

/// The scope that declares `nodes[candidate]`, when `site` provably sees
/// it: `Some(None)` for a declaration at file top level, `Some(Some(c))`
/// for one whose innermost scope container is `c`. `None` when a container
/// of the candidate does not contain the site, or is a type scope.
fn visible_scope(nodes: &[RawNode], candidate: usize, site: Span) -> Option<Option<usize>> {
    let span = nodes[candidate].span;
    let mut innermost: Option<usize> = None;
    for (i, n) in nodes.iter().enumerate() {
        let encloses = i != candidate
            && n.span != span
            && SCOPE_CONTAINERS.contains(&n.kind)
            && span_contains(n.span, span);
        if !encloses {
            continue;
        }
        if TYPE_SCOPES.contains(&n.kind) || !span_contains(n.span, site) {
            return None;
        }
        innermost = match innermost {
            Some(c) if scope_depth(nodes, Some(c)) >= scope_depth(nodes, Some(i)) => Some(c),
            _ => Some(i),
        };
    }
    Some(innermost)
}

/// Orders scopes that contain one site from outer to inner: file top level
/// first, then by later start, then by earlier end.
fn scope_depth(
    nodes: &[RawNode],
    scope: Option<usize>,
) -> Option<((u32, u32), Reverse<(u32, u32)>)> {
    scope.map(|c| {
        let (r1, c1, r2, c2) = nodes[c].span;
        ((r1, c1), Reverse((r2, c2)))
    })
}

/// Bind a name the source writes at `site` (a route handler, a framework
/// target) to the nodes of the same file whose kind is in `accept`.
///
/// A candidate counts only when `site` provably sees it: every scope
/// container around the candidate also contains the site, and none of them
/// is a type scope. Among the visible candidates the innermost scope
/// shadows the outer ones. A candidate the site cannot be proven to see
/// never binds; its raw index is kept in [`Bound::out_of_scope`].
///
/// | Visible matches in the innermost scope | Endpoint | Evidence |
/// |---|---|---|
/// | 1 | `Node` | `Exact` |
/// | 2 or more | `Unresolved(Named)` | `Ambiguous` with the true count |
/// | 0 | `Unresolved(Named)` | `Unresolved` |
/// | empty name | `Unresolved(Anonymous)` | `Unresolved` |
///
/// Two matches never bind the first.
pub fn bind_name(nodes: &[RawNode], site: Span, name: &str, accept: &[NodeKind]) -> Bound {
    let Some(source) = SourceName::new(name) else {
        return Bound {
            endpoint: RawEndpoint::Unresolved(Unbound::Anonymous),
            evidence: Evidence::Unresolved,
            out_of_scope: Box::default(),
        };
    };
    let mut visible: Vec<(RawIdx, Option<usize>)> = Vec::new();
    let mut hidden: Vec<RawIdx> = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        if n.name != source.as_str() || !accept.contains(&n.kind) {
            continue;
        }
        let Some(idx) = RawIdx::checked(i, nodes.len()) else {
            continue;
        };
        match visible_scope(nodes, i, site) {
            Some(scope) => visible.push((idx, scope)),
            None => hidden.push(idx),
        }
    }
    let innermost = visible
        .iter()
        .map(|&(_, scope)| scope_depth(nodes, scope))
        .max();
    let winners: Vec<RawIdx> = visible
        .iter()
        .filter(|&&(_, scope)| Some(scope_depth(nodes, scope)) == innermost)
        .map(|&(idx, _)| idx)
        .collect();
    let named = RawEndpoint::Unresolved(Unbound::Named(source));
    let (endpoint, evidence) = match winners.as_slice() {
        [only] => (RawEndpoint::Node(*only), Evidence::Exact),
        [] => (named, Evidence::Unresolved),
        several => (
            named,
            Evidence::Ambiguous {
                candidates: u32::try_from(several.len()).unwrap_or(u32::MAX),
            },
        ),
    };
    Bound {
        endpoint,
        evidence,
        out_of_scope: hidden.into_boxed_slice(),
    }
}

/// An endpoint and the evidence that bound it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    endpoint: RawEndpoint,
    evidence: Evidence,
    out_of_scope: Box<[RawIdx]>,
}

impl Bound {
    /// `Ok` when the pair is coherent and a name is trimmed and not empty:
    ///
    /// | Endpoint | Evidence allowed |
    /// |---|---|
    /// | `Node` | `Exact`, `Heuristic` |
    /// | `File` | `Exact` |
    /// | `Unresolved` | `Ambiguous` (2 or more), `Unresolved` |
    pub fn new(endpoint: RawEndpoint, evidence: Evidence) -> Result<Bound, EvidenceError> {
        if let RawEndpoint::Unresolved(Unbound::Named(name)) = &endpoint {
            if !name.is_valid() {
                return Err(EvidenceError::BadName);
            }
        }
        let coherent = match (&endpoint, evidence) {
            (_, Evidence::Ambiguous { candidates }) if candidates < 2 => {
                return Err(EvidenceError::TooFewCandidates(candidates));
            }
            (RawEndpoint::Node(_), Evidence::Exact | Evidence::Heuristic) => true,
            (RawEndpoint::File, Evidence::Exact) => true,
            (RawEndpoint::Unresolved(_), Evidence::Ambiguous { .. } | Evidence::Unresolved) => true,
            _ => false,
        };
        match coherent {
            true => Ok(Bound {
                endpoint,
                evidence,
                out_of_scope: Box::default(),
            }),
            false => Err(EvidenceError::EndpointMismatch(evidence)),
        }
    }

    pub fn endpoint(&self) -> &RawEndpoint {
        &self.endpoint
    }

    pub fn evidence(&self) -> Evidence {
        self.evidence
    }

    /// Same-name candidates of the file, of an accepted kind, that the site
    /// cannot be proven to see (a nested function, a class member). No pass
    /// binds the record to them; they back a caveat or a `BlindSpot` hint.
    pub fn out_of_scope(&self) -> &[RawIdx] {
        &self.out_of_scope
    }
}

// ── Evidence ────────────────────────────────────────────────────────────

/// The confidence that separates a guess from a fact. An `Ambiguous` edge
/// stays strictly below it. A `Heuristic` edge stays strictly below it,
/// unless its `RelType` is heuristic by kind ([`RelType::is_heuristic`]).
/// An `Exact` edge is at or above it.
pub const HEURISTIC_CEILING: f32 = 0.7;

/// How an endpoint or an edge was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Evidence {
    /// By structure (a raw index from span containment, the declaration's
    /// own record) or by one unique scoped match.
    Exact,
    /// Several candidates matched equally. None is picked; `candidates` is
    /// the true count, at least 2.
    Ambiguous { candidates: u32 },
    /// Picked by a rule that can be wrong: a repo-unique basename, path
    /// proximity, a name-and-type mirror, a templated URL.
    Heuristic,
    /// No candidate. Never an edge.
    Unresolved,
}

impl Evidence {
    pub const fn emits_edge(self) -> bool {
        !matches!(self, Evidence::Unresolved)
    }

    /// The class a stored edge reads back as. Storage keeps no class
    /// column (Open point 1), so a reader sees `Exact` or `Heuristic` only.
    pub fn of_stored(confidence: f32, rel: RelType) -> Evidence {
        match rel.is_heuristic() || confidence < HEURISTIC_CEILING {
            true => Evidence::Heuristic,
            false => Evidence::Exact,
        }
    }

    /// `Ok` when an edge of type `rel` with this evidence may carry
    /// `confidence`.
    pub fn check(self, confidence: f32, rel: RelType) -> Result<(), EvidenceError> {
        let in_range = confidence > 0.0 && confidence <= 1.0;
        if !in_range {
            return Err(EvidenceError::ConfidenceOutOfRange(confidence));
        }
        match self {
            Evidence::Unresolved => Err(EvidenceError::NoEdge),
            Evidence::Exact if rel.is_heuristic() => Err(EvidenceError::ExactOnHeuristicRel(rel)),
            Evidence::Exact if confidence < HEURISTIC_CEILING => {
                Err(EvidenceError::ExactBelowCeiling(confidence))
            }
            Evidence::Exact => Ok(()),
            Evidence::Ambiguous { candidates } if candidates < 2 => {
                Err(EvidenceError::TooFewCandidates(candidates))
            }
            Evidence::Ambiguous { .. } if confidence >= HEURISTIC_CEILING => {
                Err(EvidenceError::GuessAtOrAboveCeiling(confidence))
            }
            Evidence::Ambiguous { .. } => Ok(()),
            Evidence::Heuristic if rel.is_heuristic() || confidence < HEURISTIC_CEILING => Ok(()),
            Evidence::Heuristic => Err(EvidenceError::GuessAtOrAboveCeiling(confidence)),
        }
    }
}

/// An evidence class that does not fit its endpoint or its edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EvidenceError {
    /// `Unresolved` evidence produces no edge.
    NoEdge,
    /// Confidence is not in `(0, 1]`, or is NaN.
    ConfidenceOutOfRange(f32),
    /// An `Exact` edge below [`HEURISTIC_CEILING`].
    ExactBelowCeiling(f32),
    /// An `Exact` edge of a `RelType` that is heuristic by kind.
    ExactOnHeuristicRel(RelType),
    /// A `Heuristic` or `Ambiguous` edge at or above [`HEURISTIC_CEILING`]
    /// on a `RelType` that is not heuristic by kind.
    GuessAtOrAboveCeiling(f32),
    /// `Ambiguous` with fewer than two candidates.
    TooFewCandidates(u32),
    /// The evidence class does not fit the endpoint variant.
    EndpointMismatch(Evidence),
    /// An unresolved name that is empty or not trimmed.
    BadName,
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvidenceError::NoEdge => f.write_str("unresolved evidence produces no edge"),
            EvidenceError::ConfidenceOutOfRange(c) => {
                write!(f, "confidence {c} is not in (0, 1]")
            }
            EvidenceError::ExactBelowCeiling(c) => write!(
                f,
                "an exact edge has confidence {c}, below {HEURISTIC_CEILING}"
            ),
            EvidenceError::ExactOnHeuristicRel(rel) => {
                write!(
                    f,
                    "{rel:?} is heuristic by kind; an exact edge cannot use it"
                )
            }
            EvidenceError::GuessAtOrAboveCeiling(c) => write!(
                f,
                "a heuristic or ambiguous edge has confidence {c}, at or above {HEURISTIC_CEILING}"
            ),
            EvidenceError::TooFewCandidates(n) => {
                write!(
                    f,
                    "an ambiguous binding needs 2 or more candidates, not {n}"
                )
            }
            EvidenceError::EndpointMismatch(e) => {
                write!(f, "evidence {e:?} does not fit this endpoint")
            }
            EvidenceError::BadName => f.write_str("an unresolved name is empty or not trimmed"),
        }
    }
}

impl std::error::Error for EvidenceError {}

// ── Raw index to global node index ──────────────────────────────────────

/// Position of a `LocalGraph` in the build input, after the builder sorts
/// it by path (`GraphBuilder::build`). In the overlay: position of a dirty
/// file in the view's input list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileOrd(pub u32);

impl FileOrd {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// The one map from `(file, raw index)` to a global node index.
///
/// Global indexes `base .. end()` hold every raw node of every file, file
/// after file, in input order. A raw node keeps its slot when the builder
/// tombstones it, so the map reads node counts only, never uids. Nodes the
/// builder makes later (routes, entry points, detector nodes, processes,
/// `File` nodes) sit at `end()` and above. The full build uses `base` 0;
/// the overlay uses the base graph's node count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawIndexMap {
    /// `starts[f]` is the first slot of file `f`; the last entry is `end()`.
    starts: Box<[u32]>,
    /// `(tombstone slot, surviving slot)`, strictly sorted by tombstone.
    merged: Box<[(u32, u32)]>,
}

impl RawIndexMap {
    /// The map for files with these node counts, in input order.
    pub fn new<I>(base: u32, node_counts: I) -> Result<RawIndexMap, IndexMapError>
    where
        I: IntoIterator<Item = usize>,
    {
        let counts = node_counts.into_iter();
        let mut starts = Vec::with_capacity(counts.size_hint().0 + 1);
        let mut next = base;
        starts.push(next);
        for count in counts {
            next = u32::try_from(count)
                .ok()
                .and_then(|count| next.checked_add(count))
                .ok_or(IndexMapError::Overflow)?;
            starts.push(next);
        }
        Ok(RawIndexMap {
            starts: starts.into_boxed_slice(),
            merged: Box::default(),
        })
    }

    /// Add the slots that pass 1 tombstoned, each with the slot of the
    /// node that kept its identity. A survivor comes before its tombstone
    /// (first occurrence wins) and is never itself a tombstone.
    pub fn with_merged(
        mut self,
        mut merged: Vec<(u32, u32)>,
    ) -> Result<RawIndexMap, IndexMapError> {
        merged.sort_unstable();
        for pair in merged.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(IndexMapError::DuplicateTombstone(pair[0].0));
            }
        }
        for &(tombstone, survivor) in &merged {
            if !self.contains(tombstone) || !self.contains(survivor) {
                return Err(IndexMapError::MergedOutOfRange(tombstone));
            }
            if survivor >= tombstone {
                return Err(IndexMapError::SurvivorNotFirst(tombstone));
            }
            if merged.binary_search_by_key(&survivor, |&(t, _)| t).is_ok() {
                return Err(IndexMapError::MergedChain(tombstone));
            }
        }
        self.merged = merged.into_boxed_slice();
        Ok(self)
    }

    pub fn base(&self) -> u32 {
        self.starts[0]
    }

    /// The first global index after the raw range.
    pub fn end(&self) -> u32 {
        self.starts[self.starts.len() - 1]
    }

    pub fn file_count(&self) -> usize {
        self.starts.len() - 1
    }

    fn contains(&self, slot: u32) -> bool {
        (self.base()..self.end()).contains(&slot)
    }

    /// The slots of one file.
    pub fn range(&self, file: FileOrd) -> Option<Range<u32>> {
        let f = file.index();
        (f < self.file_count()).then(|| self.starts[f]..self.starts[f + 1])
    }

    /// The slot of a raw node, tombstone or not.
    pub fn slot(&self, file: FileOrd, raw: RawIdx) -> Option<u32> {
        let range = self.range(file)?;
        let slot = range.start.checked_add(raw.0)?;
        (slot < range.end).then_some(slot)
    }

    /// The node that owns the raw node's identity: its own slot, or the
    /// survivor when pass 1 tombstoned it. An edge starts or ends here,
    /// never at a tombstone.
    pub fn node(&self, file: FileOrd, raw: RawIdx) -> Option<u32> {
        let slot = self.slot(file, raw)?;
        Some(
            self.merged
                .binary_search_by_key(&slot, |&(t, _)| t)
                .map_or(slot, |i| self.merged[i].1),
        )
    }

    /// The file and raw index of a slot; `None` outside the raw range.
    pub fn locate(&self, slot: u32) -> Option<(FileOrd, RawIdx)> {
        if !self.contains(slot) {
            return None;
        }
        // The last file whose first slot is at or below `slot`; an empty
        // file shares its start with the next one and is skipped.
        let f = self.starts[..self.file_count()].partition_point(|&s| s <= slot) - 1;
        Some((FileOrd(f as u32), RawIdx(slot - self.starts[f])))
    }

    /// The global form of a record's endpoint; `None` when a `Node`
    /// endpoint or the file is out of range.
    pub fn resolve<'e>(
        &self,
        file: FileOrd,
        endpoint: &'e RawEndpoint,
    ) -> Option<GlobalEndpoint<'e>> {
        self.range(file)?;
        match endpoint {
            RawEndpoint::Node(raw) => self.node(file, *raw).map(GlobalEndpoint::Node),
            RawEndpoint::File => Some(GlobalEndpoint::File(file)),
            RawEndpoint::Unresolved(unbound) => Some(GlobalEndpoint::Unresolved(unbound)),
        }
    }
}

/// A record endpoint after [`RawIndexMap::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalEndpoint<'e> {
    /// A global node index that is never a tombstone.
    Node(u32),
    /// The file's `File` node. The builder fixes its index when it appends
    /// `File` nodes; the map does not know it (Open point 3).
    File(FileOrd),
    Unresolved(&'e Unbound),
}

/// A [`RawIndexMap`] input that breaks an invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMapError {
    /// The slots do not fit in `u32`.
    Overflow,
    MergedOutOfRange(u32),
    DuplicateTombstone(u32),
    SurvivorNotFirst(u32),
    MergedChain(u32),
}

impl fmt::Display for IndexMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexMapError::Overflow => f.write_str("raw node slots exceed u32::MAX"),
            IndexMapError::MergedOutOfRange(t) => {
                write!(
                    f,
                    "merged slot pair of tombstone {t} is outside the raw range"
                )
            }
            IndexMapError::DuplicateTombstone(t) => write!(f, "tombstone {t} is listed twice"),
            IndexMapError::SurvivorNotFirst(t) => {
                write!(f, "the survivor of tombstone {t} does not come before it")
            }
            IndexMapError::MergedChain(t) => {
                write!(f, "the survivor of tombstone {t} is itself a tombstone")
            }
        }
    }
}

impl std::error::Error for IndexMapError {}

// ── Record kinds ────────────────────────────────────────────────────────

const CALLABLE: &[NodeKind] = &[NodeKind::Function, NodeKind::Method, NodeKind::Constructor];
const CALLABLE_OR_CONST: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Constructor,
    NodeKind::Const,
];
const TYPE: &[NodeKind] = &[
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
];
const CALLABLE_OR_TYPE: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Constructor,
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
];

/// How [`ParsePhase::BindRecords`] finds a record's endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointRule {
    /// The innermost node of an accepted kind whose span contains the
    /// record's site ([`bind_enclosing`]). With no such node the endpoint
    /// is `File` when `file_allowed`; otherwise the record has no owner.
    Enclosing {
        accept: &'static [NodeKind],
        file_allowed: bool,
    },
    /// The record names its endpoint in source ([`bind_name`]).
    Named,
    /// The endpoint is always the file.
    FileOnly,
}

/// Each kind of raw record that carries an endpoint. Facts inside a
/// `RawNode` (`calls`, `field_reads`, `heritage`, `decorators`) start at
/// that node by construction and are not listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordKind {
    /// `RawImport`.
    Import,
    /// `RawRoute` from a registration call; its endpoint is the handler.
    Route,
    /// `RawFrameworkRef`; its endpoint is the source.
    FrameworkRef,
    /// `RawFanoutRef`; its endpoint is the source.
    FanoutRef,
    /// `RawPathLiteral`.
    PathLiteral,
    /// `RawSqlRef`.
    SqlRef,
    /// `RawSchemaField`; its endpoint is the owning type.
    SchemaField,
    /// `RawEventTopic`.
    EventTopic,
    /// `RawTxScope`.
    TxScope,
    /// `RawCallMeta`; its endpoint is the caller.
    CallMeta,
    /// `RawFunctionMeta`; its endpoint is the function it describes.
    FunctionMeta,
    /// An HTTP client call site. No record exists today: pass 1.6 reads
    /// the file again at build time (Open point 4). Until the overlay
    /// replays this record, an answer over a dirty route or fetch file
    /// names `Fetches` as stale.
    FetchSite,
}

impl RecordKind {
    pub const ALL: [RecordKind; 12] = [
        RecordKind::Import,
        RecordKind::Route,
        RecordKind::FrameworkRef,
        RecordKind::FanoutRef,
        RecordKind::PathLiteral,
        RecordKind::SqlRef,
        RecordKind::SchemaField,
        RecordKind::EventTopic,
        RecordKind::TxScope,
        RecordKind::CallMeta,
        RecordKind::FunctionMeta,
        RecordKind::FetchSite,
    ];

    pub const fn endpoint_rule(self) -> EndpointRule {
        match self {
            RecordKind::Import | RecordKind::FetchSite => EndpointRule::FileOnly,
            RecordKind::Route => EndpointRule::Named,
            RecordKind::FrameworkRef => EndpointRule::Enclosing {
                accept: CALLABLE_OR_TYPE,
                file_allowed: true,
            },
            RecordKind::FanoutRef | RecordKind::EventTopic => EndpointRule::Enclosing {
                accept: CALLABLE,
                file_allowed: true,
            },
            RecordKind::PathLiteral | RecordKind::SqlRef => EndpointRule::Enclosing {
                accept: CALLABLE_OR_CONST,
                file_allowed: true,
            },
            RecordKind::SchemaField => EndpointRule::Enclosing {
                accept: TYPE,
                file_allowed: false,
            },
            RecordKind::TxScope | RecordKind::CallMeta | RecordKind::FunctionMeta => {
                EndpointRule::Enclosing {
                    accept: CALLABLE,
                    file_allowed: false,
                }
            }
        }
    }

    /// The edge kinds built from this record. A session overlay that does
    /// not replay the record leaves these edges stale for a dirty file.
    pub const fn rel_types(self) -> &'static [RelType] {
        match self {
            RecordKind::Import => &[RelType::Imports],
            RecordKind::Route => &[RelType::HandlesRoute],
            RecordKind::FrameworkRef | RecordKind::FanoutRef => &[RelType::References],
            RecordKind::PathLiteral => &[RelType::UsesPathLiteral],
            RecordKind::SqlRef => &[RelType::QueriesTable],
            RecordKind::SchemaField => &[RelType::HasProperty, RelType::MirrorsField],
            RecordKind::EventTopic => &[
                RelType::Publishes,
                RelType::Subscribes,
                RelType::EventTopicMirror,
            ],
            RecordKind::TxScope => &[RelType::OpensTxScope],
            RecordKind::CallMeta => &[RelType::Calls],
            RecordKind::FunctionMeta => &[],
            RecordKind::FetchSite => &[RelType::Fetches],
        }
    }

    /// The endpoint of a record of this kind found at `site`, by
    /// [`Self::endpoint_rule`]. `None` for a `Named` rule (use
    /// [`bind_name`]) and for an `Enclosing` rule with no container and no
    /// file fallback: the record has no owner, and the producer drops it
    /// with a count, never with a guess.
    pub fn bind_site(self, nodes: &[RawNode], site: Span) -> Option<RawEndpoint> {
        match self.endpoint_rule() {
            EndpointRule::FileOnly => Some(RawEndpoint::File),
            EndpointRule::Named => None,
            EndpointRule::Enclosing {
                accept,
                file_allowed,
            } => match bind_enclosing(nodes, site, accept) {
                Some(idx) => Some(RawEndpoint::Node(idx)),
                None => file_allowed.then_some(RawEndpoint::File),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, kind: NodeKind, span: Span) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        }
    }

    fn point(row: u32, col: u32) -> Span {
        (row, col, row, col)
    }

    fn named(name: &str) -> RawEndpoint {
        RawEndpoint::named(name).expect("non-blank name")
    }

    // ── ParsePhase ──

    #[test]
    fn test_advance_to_next_phase_returns_next() {
        assert_eq!(
            ParsePhase::CollectCaptures.advance(ParsePhase::FinalizeNodes),
            Ok(ParsePhase::FinalizeNodes)
        );
        assert_eq!(
            ParsePhase::FinalizeNodes.advance(ParsePhase::BindRecords),
            Ok(ParsePhase::BindRecords)
        );
    }

    #[test]
    fn test_advance_skip_or_back_returns_error() {
        assert_eq!(
            ParsePhase::CollectCaptures.advance(ParsePhase::BindRecords),
            Err(PhaseError {
                from: ParsePhase::CollectCaptures,
                to: ParsePhase::BindRecords
            })
        );
        assert!(ParsePhase::BindRecords
            .advance(ParsePhase::FinalizeNodes)
            .is_err());
        assert!(ParsePhase::FinalizeNodes
            .advance(ParsePhase::FinalizeNodes)
            .is_err());
    }

    #[test]
    fn test_next_last_phase_returns_none() {
        assert_eq!(ParsePhase::BindRecords.next(), None);
        assert!(ParsePhase::ORDER
            .windows(2)
            .all(|w| w[0].next() == Some(w[1])));
    }

    #[test]
    fn test_phase_rules_only_bind_records_writes_indexes() {
        for phase in ParsePhase::ORDER {
            assert_eq!(phase.raw_indexes_valid(), !phase.may_change_nodes());
        }
        assert!(ParsePhase::BindRecords.raw_indexes_valid());
        assert!(ParsePhase::FinalizeNodes.may_change_nodes());
    }

    // ── RawIdx and RawEndpoint ──

    #[test]
    fn test_checked_in_range_returns_index() {
        assert_eq!(RawIdx::checked(2, 3), Some(RawIdx(2)));
    }

    #[test]
    fn test_checked_out_of_range_returns_none() {
        assert_eq!(RawIdx::checked(3, 3), None);
        assert_eq!(RawIdx::checked(0, 0), None);
    }

    #[test]
    fn test_named_text_returns_unresolved_named() {
        assert_eq!(
            RawEndpoint::named(" login "),
            Some(RawEndpoint::Unresolved(Unbound::Named(SourceName(
                "login".into()
            ))))
        );
    }

    #[test]
    fn test_named_empty_or_whitespace_returns_none() {
        assert_eq!(RawEndpoint::named(""), None);
        assert_eq!(RawEndpoint::named("  \t"), None);
    }

    #[test]
    fn test_validate_node_out_of_range_returns_error() {
        assert_eq!(
            RawEndpoint::Node(RawIdx(2)).validate(2),
            Err(EndpointError::OutOfRange {
                idx: RawIdx(2),
                node_count: 2
            })
        );
    }

    #[test]
    fn test_validate_in_range_file_and_anonymous_return_ok() {
        assert_eq!(RawEndpoint::Node(RawIdx(1)).validate(2), Ok(()));
        assert_eq!(RawEndpoint::File.validate(0), Ok(()));
        assert_eq!(
            RawEndpoint::Unresolved(Unbound::Anonymous).validate(0),
            Ok(())
        );
    }

    #[test]
    fn test_validate_untrimmed_or_empty_name_returns_error() {
        for raw in ["", " ", " x ", "x\t"] {
            let endpoint = RawEndpoint::Unresolved(Unbound::Named(SourceName(raw.into())));
            assert_eq!(endpoint.validate(0), Err(EndpointError::BadName), "{raw:?}");
        }
        assert_eq!(named("x").validate(0), Ok(()));
    }

    #[test]
    fn test_source_name_new_trims_and_rejects_blank() {
        assert_eq!(
            SourceName::new(" x ").map(|n| n.as_str().to_string()),
            Some("x".into())
        );
        assert!(SourceName::new(" x ").is_some_and(|n| n.is_valid()));
        assert_eq!(SourceName::new(""), None);
        assert_eq!(SourceName::new(" \t\n"), None);
    }

    #[test]
    fn test_to_packed24_at_max_returns_index() {
        assert_eq!(
            RawIdx(RawIdx::PACKED24_MAX).to_packed24(),
            Some(RawIdx::PACKED24_MAX)
        );
        assert_eq!(RawIdx(0).to_packed24(), Some(0));
    }

    #[test]
    fn test_to_packed24_above_max_returns_none() {
        assert_eq!(RawIdx(1 << 24).to_packed24(), None);
        assert_eq!(RawIdx(u32::MAX).to_packed24(), None);
    }

    #[test]
    fn test_node_accessor_returns_index_only_for_node() {
        assert_eq!(RawEndpoint::Node(RawIdx(4)).node(), Some(RawIdx(4)));
        assert_eq!(RawEndpoint::File.node(), None);
    }

    // ── bind_enclosing ──

    #[test]
    fn test_bind_enclosing_nested_returns_innermost() {
        let nodes = [
            node("outer", NodeKind::Function, (0, 0, 10, 0)),
            node("inner", NodeKind::Function, (2, 4, 5, 0)),
        ];
        assert_eq!(
            bind_enclosing(&nodes, point(3, 8), CALLABLE),
            Some(RawIdx(1))
        );
        assert_eq!(
            bind_enclosing(&nodes, point(7, 0), CALLABLE),
            Some(RawIdx(0))
        );
    }

    #[test]
    fn test_bind_enclosing_inner_listed_first_returns_innermost() {
        let nodes = [
            node("inner", NodeKind::Function, (2, 4, 5, 0)),
            node("outer", NodeKind::Function, (0, 0, 10, 0)),
        ];
        assert_eq!(
            bind_enclosing(&nodes, point(3, 8), CALLABLE),
            Some(RawIdx(0))
        );
    }

    #[test]
    fn test_bind_enclosing_identical_spans_returns_lower_index() {
        let nodes = [
            node("a", NodeKind::Function, (1, 0, 4, 0)),
            node("b", NodeKind::Function, (1, 0, 4, 0)),
        ];
        assert_eq!(
            bind_enclosing(&nodes, point(2, 0), CALLABLE),
            Some(RawIdx(0))
        );
    }

    #[test]
    fn test_bind_enclosing_rejected_kind_skipped() {
        let nodes = [
            node("f", NodeKind::Function, (0, 0, 10, 0)),
            node("C", NodeKind::Class, (1, 0, 9, 0)),
        ];
        assert_eq!(
            bind_enclosing(&nodes, point(3, 0), CALLABLE),
            Some(RawIdx(0))
        );
        assert_eq!(bind_enclosing(&nodes, point(3, 0), TYPE), Some(RawIdx(1)));
    }

    #[test]
    fn test_bind_enclosing_no_container_returns_none() {
        let nodes = [node("f", NodeKind::Function, (5, 0, 8, 0))];
        assert_eq!(bind_enclosing(&nodes, point(1, 0), CALLABLE), None);
        assert_eq!(bind_enclosing(&[], point(1, 0), CALLABLE), None);
    }

    #[test]
    fn test_bind_enclosing_site_span_inside_returns_container() {
        let nodes = [node("f", NodeKind::Method, (0, 0, 10, 0))];
        assert_eq!(
            bind_enclosing(&nodes, (2, 3, 2, 20), CALLABLE),
            Some(RawIdx(0))
        );
        assert_eq!(bind_enclosing(&nodes, (2, 3, 11, 0), CALLABLE), None);
    }

    // ── bind_name ──

    #[test]
    fn test_bind_name_unique_returns_exact_node() {
        let nodes = [
            node("helper", NodeKind::Function, (0, 0, 1, 0)),
            node("login", NodeKind::Function, (2, 0, 3, 0)),
        ];
        let bound = bind_name(&nodes, point(9, 0), "login", CALLABLE);
        assert_eq!(bound.endpoint(), &RawEndpoint::Node(RawIdx(1)));
        assert_eq!(bound.evidence(), Evidence::Exact);
    }

    #[test]
    fn test_bind_name_two_matches_returns_ambiguous_with_count() {
        let nodes = [
            node("fmt", NodeKind::Method, (0, 0, 1, 0)),
            node("fmt", NodeKind::Method, (2, 0, 3, 0)),
            node("fmt", NodeKind::Method, (4, 0, 5, 0)),
        ];
        let bound = bind_name(&nodes, point(9, 0), "fmt", CALLABLE);
        assert_eq!(bound.endpoint(), &named("fmt"));
        assert_eq!(bound.evidence(), Evidence::Ambiguous { candidates: 3 });
    }

    #[test]
    fn test_bind_name_missing_returns_unresolved_named() {
        let nodes = [node("a", NodeKind::Function, (0, 0, 1, 0))];
        let bound = bind_name(&nodes, point(9, 0), "Ctrl@index", CALLABLE);
        assert_eq!(bound.endpoint(), &named("Ctrl@index"));
        assert_eq!(bound.evidence(), Evidence::Unresolved);
    }

    #[test]
    fn test_bind_name_other_kind_ignored() {
        let nodes = [
            node("login", NodeKind::Property, (0, 0, 0, 9)),
            node("login", NodeKind::Function, (2, 0, 3, 0)),
        ];
        let bound = bind_name(&nodes, point(9, 0), "login", CALLABLE);
        assert_eq!(bound.endpoint(), &RawEndpoint::Node(RawIdx(1)));
        assert_eq!(bound.evidence(), Evidence::Exact);
    }

    #[test]
    fn test_bind_name_empty_returns_anonymous_unresolved() {
        let nodes = [node("a", NodeKind::Function, (0, 0, 1, 0))];
        let bound = bind_name(&nodes, point(9, 0), " ", CALLABLE);
        assert_eq!(
            bound.endpoint(),
            &RawEndpoint::Unresolved(Unbound::Anonymous)
        );
        assert_eq!(bound.evidence(), Evidence::Unresolved);
    }

    #[test]
    fn test_bind_name_nested_candidate_from_module_site_returns_unresolved_with_index() {
        // `from x import handler` at module level; `setup` holds its own
        // `handler`; the route registration sits at module level.
        let nodes = [
            node("setup", NodeKind::Function, (0, 0, 5, 0)),
            node("handler", NodeKind::Function, (1, 4, 3, 0)),
        ];
        let bound = bind_name(&nodes, point(7, 0), "handler", CALLABLE);
        assert_eq!(bound.endpoint(), &named("handler"));
        assert_eq!(bound.evidence(), Evidence::Unresolved);
        assert_eq!(bound.out_of_scope(), &[RawIdx(1)]);
    }

    #[test]
    fn test_bind_name_nested_candidate_from_its_scope_returns_exact() {
        let nodes = [
            node("setup", NodeKind::Function, (0, 0, 5, 0)),
            node("handler", NodeKind::Function, (1, 4, 3, 0)),
        ];
        let bound = bind_name(&nodes, point(4, 4), "handler", CALLABLE);
        assert_eq!(bound.endpoint(), &RawEndpoint::Node(RawIdx(1)));
        assert_eq!(bound.evidence(), Evidence::Exact);
        assert!(bound.out_of_scope().is_empty());
    }

    #[test]
    fn test_bind_name_inner_scope_shadows_outer_returns_inner() {
        let nodes = [
            node("handler", NodeKind::Function, (0, 0, 1, 0)),
            node("setup", NodeKind::Function, (2, 0, 8, 0)),
            node("handler", NodeKind::Function, (3, 4, 5, 0)),
        ];
        let inside = bind_name(&nodes, point(6, 4), "handler", CALLABLE);
        assert_eq!(inside.endpoint(), &RawEndpoint::Node(RawIdx(2)));
        assert_eq!(inside.evidence(), Evidence::Exact);
        let outside = bind_name(&nodes, point(10, 0), "handler", CALLABLE);
        assert_eq!(outside.endpoint(), &RawEndpoint::Node(RawIdx(0)));
        assert_eq!(outside.evidence(), Evidence::Exact);
        assert_eq!(outside.out_of_scope(), &[RawIdx(2)]);
    }

    #[test]
    fn test_bind_name_class_member_returns_unresolved_with_index() {
        let nodes = [
            node("C", NodeKind::Class, (0, 0, 10, 0)),
            node("handler", NodeKind::Method, (1, 4, 3, 0)),
            node("init", NodeKind::Method, (4, 4, 8, 0)),
        ];
        let bound = bind_name(&nodes, point(5, 8), "handler", CALLABLE);
        assert_eq!(bound.endpoint(), &named("handler"));
        assert_eq!(bound.evidence(), Evidence::Unresolved);
        assert_eq!(bound.out_of_scope(), &[RawIdx(1)]);
    }

    // ── Bound ──

    #[test]
    fn test_bound_new_coherent_pairs_return_ok() {
        assert!(Bound::new(RawEndpoint::Node(RawIdx(0)), Evidence::Exact).is_ok());
        assert!(Bound::new(RawEndpoint::Node(RawIdx(0)), Evidence::Heuristic).is_ok());
        assert!(Bound::new(RawEndpoint::File, Evidence::Exact).is_ok());
        let f = named("f");
        assert!(Bound::new(f.clone(), Evidence::Ambiguous { candidates: 2 }).is_ok());
        assert!(Bound::new(f, Evidence::Unresolved).is_ok());
    }

    #[test]
    fn test_bound_new_bad_name_returns_error() {
        for raw in ["", " f", "f "] {
            let endpoint = RawEndpoint::Unresolved(Unbound::Named(SourceName(raw.into())));
            assert_eq!(
                Bound::new(endpoint, Evidence::Unresolved),
                Err(EvidenceError::BadName),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn test_bound_new_mismatched_pairs_return_error() {
        assert_eq!(
            Bound::new(RawEndpoint::Node(RawIdx(0)), Evidence::Unresolved),
            Err(EvidenceError::EndpointMismatch(Evidence::Unresolved))
        );
        assert_eq!(
            Bound::new(RawEndpoint::File, Evidence::Heuristic),
            Err(EvidenceError::EndpointMismatch(Evidence::Heuristic))
        );
        assert_eq!(
            Bound::new(RawEndpoint::Unresolved(Unbound::Anonymous), Evidence::Exact),
            Err(EvidenceError::EndpointMismatch(Evidence::Exact))
        );
    }

    #[test]
    fn test_bound_new_ambiguous_one_candidate_returns_error() {
        assert_eq!(
            Bound::new(
                RawEndpoint::Unresolved(Unbound::Anonymous),
                Evidence::Ambiguous { candidates: 1 }
            ),
            Err(EvidenceError::TooFewCandidates(1))
        );
    }

    // ── Evidence ──

    #[test]
    fn test_check_exact_at_ceiling_returns_ok() {
        assert_eq!(
            Evidence::Exact.check(HEURISTIC_CEILING, RelType::UsesPathLiteral),
            Ok(())
        );
        assert_eq!(Evidence::Exact.check(1.0, RelType::Calls), Ok(()));
    }

    #[test]
    fn test_check_exact_below_ceiling_returns_error() {
        assert_eq!(
            Evidence::Exact.check(0.6, RelType::Fetches),
            Err(EvidenceError::ExactBelowCeiling(0.6))
        );
    }

    #[test]
    fn test_check_exact_on_heuristic_rel_returns_error() {
        assert_eq!(
            Evidence::Exact.check(0.9, RelType::MirrorsField),
            Err(EvidenceError::ExactOnHeuristicRel(RelType::MirrorsField))
        );
    }

    #[test]
    fn test_check_heuristic_at_ceiling_on_plain_rel_returns_error() {
        assert_eq!(
            Evidence::Heuristic.check(0.9, RelType::Imports),
            Err(EvidenceError::GuessAtOrAboveCeiling(0.9))
        );
        assert_eq!(
            Evidence::Heuristic.check(HEURISTIC_CEILING, RelType::Imports),
            Err(EvidenceError::GuessAtOrAboveCeiling(HEURISTIC_CEILING))
        );
    }

    #[test]
    fn test_check_heuristic_below_ceiling_or_heuristic_rel_returns_ok() {
        assert_eq!(Evidence::Heuristic.check(0.6, RelType::Imports), Ok(()));
        assert_eq!(
            Evidence::Heuristic.check(0.85, RelType::EventTopicMirror),
            Ok(())
        );
    }

    #[test]
    fn test_check_ambiguous_below_ceiling_returns_ok_else_error() {
        let amb = Evidence::Ambiguous { candidates: 4 };
        assert_eq!(amb.check(0.25, RelType::References), Ok(()));
        assert_eq!(
            amb.check(0.9, RelType::MirrorsField),
            Err(EvidenceError::GuessAtOrAboveCeiling(0.9))
        );
        assert_eq!(
            Evidence::Ambiguous { candidates: 1 }.check(0.3, RelType::References),
            Err(EvidenceError::TooFewCandidates(1))
        );
    }

    #[test]
    fn test_check_unresolved_returns_no_edge() {
        assert_eq!(
            Evidence::Unresolved.check(0.5, RelType::Calls),
            Err(EvidenceError::NoEdge)
        );
        assert!(!Evidence::Unresolved.emits_edge());
        assert!(Evidence::Heuristic.emits_edge());
    }

    #[test]
    fn test_check_confidence_out_of_range_returns_error() {
        for c in [0.0, -0.1, 1.01, f32::NAN, f32::INFINITY] {
            assert!(matches!(
                Evidence::Exact.check(c, RelType::Calls),
                Err(EvidenceError::ConfidenceOutOfRange(_))
            ));
        }
    }

    #[test]
    fn test_of_stored_bands_return_exact_or_heuristic() {
        assert_eq!(Evidence::of_stored(1.0, RelType::Calls), Evidence::Exact);
        assert_eq!(
            Evidence::of_stored(HEURISTIC_CEILING, RelType::Calls),
            Evidence::Exact
        );
        assert_eq!(
            Evidence::of_stored(0.6, RelType::Calls),
            Evidence::Heuristic
        );
        assert_eq!(
            Evidence::of_stored(0.9, RelType::MirrorsField),
            Evidence::Heuristic
        );
    }

    #[test]
    fn test_of_stored_agrees_with_check() {
        for (c, rel) in [
            (1.0, RelType::Calls),
            (0.6, RelType::Fetches),
            (0.85, RelType::EventTopicMirror),
            (0.7, RelType::Imports),
        ] {
            assert_eq!(Evidence::of_stored(c, rel).check(c, rel), Ok(()));
        }
    }

    // ── RawIndexMap ──

    #[test]
    fn test_new_slots_follow_prefix_sum() {
        let map = RawIndexMap::new(0, [3, 0, 2]).unwrap();
        assert_eq!(map.file_count(), 3);
        assert_eq!(map.end(), 5);
        assert_eq!(map.range(FileOrd(0)), Some(0..3));
        assert_eq!(map.range(FileOrd(1)), Some(3..3));
        assert_eq!(map.slot(FileOrd(2), RawIdx(1)), Some(4));
        assert_eq!(map.range(FileOrd(3)), None);
    }

    #[test]
    fn test_new_with_base_offsets_every_slot() {
        let map = RawIndexMap::new(100, [2, 1]).unwrap();
        assert_eq!(map.base(), 100);
        assert_eq!(map.slot(FileOrd(0), RawIdx(0)), Some(100));
        assert_eq!(map.slot(FileOrd(1), RawIdx(0)), Some(102));
        assert_eq!(map.end(), 103);
    }

    #[test]
    fn test_new_no_files_returns_empty_range() {
        let map = RawIndexMap::new(7, std::iter::empty()).unwrap();
        assert_eq!(map.file_count(), 0);
        assert_eq!(map.end(), 7);
        assert_eq!(map.locate(7), None);
    }

    #[test]
    fn test_new_overflow_returns_error() {
        assert_eq!(
            RawIndexMap::new(u32::MAX - 1, [1, 1]),
            Err(IndexMapError::Overflow)
        );
        assert_eq!(
            RawIndexMap::new(0, [u32::MAX as usize, 1]),
            Err(IndexMapError::Overflow)
        );
    }

    #[test]
    fn test_slot_out_of_file_returns_none() {
        let map = RawIndexMap::new(0, [2, 2]).unwrap();
        assert_eq!(map.slot(FileOrd(0), RawIdx(2)), None);
        assert_eq!(map.slot(FileOrd(1), RawIdx(u32::MAX)), None);
    }

    #[test]
    fn test_locate_skips_empty_files() {
        let map = RawIndexMap::new(0, [0, 3, 0, 0, 2]).unwrap();
        assert_eq!(map.locate(0), Some((FileOrd(1), RawIdx(0))));
        assert_eq!(map.locate(3), Some((FileOrd(4), RawIdx(0))));
        assert_eq!(map.locate(5), None);
    }

    #[test]
    fn test_locate_inverts_slot() {
        let map = RawIndexMap::new(10, [2, 0, 3]).unwrap();
        for slot in map.base()..map.end() {
            let (file, raw) = map.locate(slot).unwrap();
            assert_eq!(map.slot(file, raw), Some(slot));
        }
        assert_eq!(map.locate(9), None);
    }

    #[test]
    fn test_node_merged_returns_survivor() {
        let map = RawIndexMap::new(0, [2, 2])
            .unwrap()
            .with_merged(vec![(3, 1)])
            .unwrap();
        assert_eq!(map.slot(FileOrd(1), RawIdx(1)), Some(3));
        assert_eq!(map.node(FileOrd(1), RawIdx(1)), Some(1));
        assert_eq!(map.node(FileOrd(1), RawIdx(0)), Some(2));
    }

    #[test]
    fn test_with_merged_bad_pairs_return_error() {
        let map = || RawIndexMap::new(0, [4]).unwrap();
        assert_eq!(
            map().with_merged(vec![(4, 0)]),
            Err(IndexMapError::MergedOutOfRange(4))
        );
        assert_eq!(
            map().with_merged(vec![(1, 2)]),
            Err(IndexMapError::SurvivorNotFirst(1))
        );
        assert_eq!(
            map().with_merged(vec![(2, 1), (2, 0)]),
            Err(IndexMapError::DuplicateTombstone(2))
        );
        assert_eq!(
            map().with_merged(vec![(3, 2), (2, 0)]),
            Err(IndexMapError::MergedChain(3))
        );
    }

    #[test]
    fn test_with_merged_unsorted_input_is_sorted() {
        let map = RawIndexMap::new(0, [4])
            .unwrap()
            .with_merged(vec![(3, 0), (2, 1)])
            .unwrap();
        assert_eq!(map.node(FileOrd(0), RawIdx(2)), Some(1));
        assert_eq!(map.node(FileOrd(0), RawIdx(3)), Some(0));
    }

    #[test]
    fn test_resolve_each_endpoint_variant() {
        let map = RawIndexMap::new(0, [1, 2]).unwrap();
        assert_eq!(
            map.resolve(FileOrd(1), &RawEndpoint::Node(RawIdx(1))),
            Some(GlobalEndpoint::Node(2))
        );
        assert_eq!(
            map.resolve(FileOrd(1), &RawEndpoint::File),
            Some(GlobalEndpoint::File(FileOrd(1)))
        );
        let unbound = RawEndpoint::Unresolved(Unbound::Anonymous);
        assert_eq!(
            map.resolve(FileOrd(0), &unbound),
            Some(GlobalEndpoint::Unresolved(&Unbound::Anonymous))
        );
    }

    #[test]
    fn test_resolve_out_of_range_returns_none() {
        let map = RawIndexMap::new(0, [1]).unwrap();
        assert_eq!(map.resolve(FileOrd(0), &RawEndpoint::Node(RawIdx(1))), None);
        assert_eq!(map.resolve(FileOrd(1), &RawEndpoint::File), None);
    }

    // ── RecordKind ──

    #[test]
    fn test_bind_site_enclosing_returns_node_then_file() {
        let nodes = [node("f", NodeKind::Function, (2, 0, 4, 0))];
        assert_eq!(
            RecordKind::PathLiteral.bind_site(&nodes, point(3, 4)),
            Some(RawEndpoint::Node(RawIdx(0)))
        );
        assert_eq!(
            RecordKind::PathLiteral.bind_site(&nodes, point(0, 4)),
            Some(RawEndpoint::File)
        );
    }

    #[test]
    fn test_bind_site_no_file_fallback_returns_none() {
        let nodes = [node("f", NodeKind::Function, (2, 0, 4, 0))];
        assert_eq!(RecordKind::TxScope.bind_site(&nodes, point(0, 4)), None);
        assert_eq!(RecordKind::SchemaField.bind_site(&nodes, point(3, 0)), None);
    }

    #[test]
    fn test_bind_site_named_and_file_only_rules() {
        let nodes = [node("f", NodeKind::Function, (0, 0, 9, 0))];
        assert_eq!(RecordKind::Route.bind_site(&nodes, point(1, 0)), None);
        assert_eq!(
            RecordKind::Import.bind_site(&nodes, point(1, 0)),
            Some(RawEndpoint::File)
        );
        assert_eq!(
            RecordKind::FetchSite.bind_site(&nodes, point(1, 0)),
            Some(RawEndpoint::File)
        );
    }

    #[test]
    fn test_rel_types_every_kind_but_function_meta_non_empty() {
        for kind in RecordKind::ALL {
            assert_eq!(
                kind.rel_types().is_empty(),
                kind == RecordKind::FunctionMeta,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn test_endpoint_rule_enclosing_accept_sets_non_empty() {
        for kind in RecordKind::ALL {
            if let EndpointRule::Enclosing { accept, .. } = kind.endpoint_rule() {
                assert!(!accept.is_empty(), "{kind:?}");
            }
        }
    }
}
