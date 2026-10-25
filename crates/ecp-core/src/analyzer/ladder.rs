//! The name tiers of the call-resolution ladder that the index
//! (`Resolver::resolve_symbol_with_import_binding`) and the session overlay
//! (`resolve_callee` in `session/view.rs`) both run: Tier 1 (the caller's
//! file), Tier 2.75 (the caller's bases) and Tier 3 (one name across the
//! project). The rules that pick live here once (B §2.2, row B5); each side
//! answers the questions through its own [`ResolveFacts`].
//!
//! Each side keeps what only it has: the decision dump (index), the Tier 3
//! memo (overlay), and a post-filter the overlay applies to every tier. The
//! file a base resolves to and the Tier 3 candidate stream read per-call
//! inputs (the caller's imports, the overlay's name tables) that neither
//! facts store holds, so they arrive as closures.

use crate::analyzer::outcome::{NoEdge, Resolution, Tier};
use crate::analyzer::reexport::{pick_call, ResolveTarget};
use crate::analyzer::same_file_pick::{prefer_free, prefer_owned, PickFacts, PickScope};
use crate::analyzer::type_ladder::TypeFacts;
use crate::file_category::{pick_global, FileMeta, GlobalPick};
use crate::graph::NodeKind;

/// What the name tiers read besides [`TypeFacts`]. Each method answers a
/// question about the declarations; the tiers below decide among them.
pub trait ResolveFacts: TypeFacts {
    /// `file`'s declarations of `name` in source order, with their kinds, as
    /// a name lookup sees them: no scoped declaration, no later overload.
    fn call_decls(&self, file: &str, name: &str) -> impl Iterator<Item = (u32, NodeKind)> + Clone;
    /// The first declaration of `name` in `file` of `target`'s kinds, in
    /// source order. A scoped one counts: [`prefer_owned`] puts the first
    /// unscoped one in its place.
    fn first_decl_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> Option<u32>;
}

/// What a call site adds to a name lookup: the index's and the overlay's
/// `CallScope`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallPick<'c> {
    /// The owner key of the type the calling member belongs to.
    pub owner: Option<&'c str>,
    /// The calling declaration: a Rust `self.m()` binds its own `impl`
    /// block first ([`prefer_owned`]).
    pub caller: Option<u32>,
    /// `o.m()` on a receiver the parser could not type: no `this`, so the
    /// caller's bases do not narrow the pick and the heritage tier skips.
    pub untyped: bool,
    /// A bare `f()` that never reaches a member: Tier 1 weighs no `Method`
    /// and the heritage tier skips. Each side's Tier 3 candidate stream
    /// drops the methods itself.
    pub free_only: bool,
}

/// Tier 1: `name` among `file`'s own declarations of `target`'s kinds, the
/// first in source order ([`pick_call`]). A call site narrows the pick by
/// owner ([`prefer_owned`], with `heritage` unless the receiver is
/// untyped), or to a free function when it reaches no member
/// ([`prefer_free`]). `Unknown` when the file declares no such name, or
/// holds several unrelated owners of it: the later tiers decide.
pub fn same_file<F: ResolveFacts>(
    facts: &F,
    file: &str,
    name: &str,
    target: ResolveTarget,
    call: Option<CallPick<'_>>,
    heritage: &[String],
) -> Resolution {
    let free_only = call.is_some_and(|call| call.free_only);
    let decls = facts
        .call_decls(file, name)
        .filter(move |&(_, kind)| !free_only || kind != NodeKind::Method);
    let Some(pick) = pick_call(decls, target) else {
        return Resolution::Unknown;
    };
    let hit = match call {
        Some(call) if free_only => prefer_free(facts, file, name, call.owner, target),
        Some(call) => prefer_owned(
            facts,
            file,
            pick,
            name,
            call.owner,
            if call.untyped { &[] } else { heritage },
            target,
            PickScope {
                caller: call.caller,
                typed_receiver: false,
                untyped_receiver: call.untyped,
                qualifier_is_type: false,
            },
        ),
        None => Some(pick),
    };
    match hit.filter(|&id| !free_only || facts.kind(id) != NodeKind::Method) {
        Some(id) => Resolution::bound(id, Tier::SameFile),
        None => Resolution::Unknown,
    }
}

/// Tier 2.75: a bare `name` in a type with declared `bases`. Each base, in
/// declared order (the parser's source order, which mirrors MRO
/// precedence), is an implicit qualifier: `base_file` names the file that
/// declares it, and that file's member owned by the base binds. The first
/// base that binds decides, and its name comes back with the edge. Only
/// the listed bases are probed, each once, so a supertype cycle ends.
/// `Unknown` for an untyped receiver or a call that reaches no member.
pub fn heritage<'b, F: ResolveFacts, P: AsRef<str>>(
    facts: &F,
    bases: &'b [String],
    name: &str,
    target: ResolveTarget,
    call: Option<CallPick<'_>>,
    mut base_file: impl FnMut(&str) -> Option<P>,
) -> (Resolution, Option<&'b str>) {
    if call.is_some_and(|call| call.untyped || call.free_only) {
        return (Resolution::Unknown, None);
    }
    for base in bases {
        let Some(declaring) = base_file(base.as_str()) else {
            continue;
        };
        let file = declaring.as_ref();
        let hit = facts
            .first_decl_in_file(file, name, target)
            .and_then(|pick| {
                prefer_owned(
                    facts,
                    file,
                    pick,
                    name,
                    Some(base.as_str()),
                    &[],
                    target,
                    PickScope {
                        typed_receiver: true,
                        ..PickScope::default()
                    },
                )
            });
        if let Some(id) = hit {
            return (
                Resolution::bound(id, Tier::HeritageScoped),
                Some(base.as_str()),
            );
        }
    }
    (Resolution::Unknown, None)
}

/// The Tier 3 verdict over `candidates`, same-named declarations with
/// their files' metas: [`pick_global`]'s barriers over the unscoped ones
/// (I5), then a unique pick outside `target`'s kinds is `NoMatch`. Method
/// eligibility is this post-filter, not a candidate filter: dropping free
/// functions before the uniqueness check would invent a winner.
pub fn global_pick<F: PickFacts + ?Sized>(
    facts: &F,
    caller: FileMeta,
    target: ResolveTarget,
    candidates: impl IntoIterator<Item = (u32, FileMeta)>,
) -> GlobalPick {
    let unscoped = candidates
        .into_iter()
        .filter(|&(id, _)| !facts.is_lexically_scoped(id));
    match pick_global(caller, unscoped) {
        GlobalPick::Unique(id) if !target.kind_predicate()(facts.kind(id)) => GlobalPick::NoMatch,
        other => other,
    }
}

/// The copy nearest `caller_file` when every candidate `caller` admits is
/// a copy of the first ([`TypeFacts::is_copy`]): duplicated source trees
/// (`android/` beside the main tree, vendored copies) hold such copies, and
/// the build compiles the caller against its own tree's. The walk stops at
/// the first candidate that is no copy, so a common name with many
/// unrelated declarations costs two comparisons. `None` for a tie or for
/// fewer than two copies ([`TypeFacts::nearest_copy`]).
pub fn nearest_copy<F: TypeFacts + ?Sized>(
    facts: &F,
    caller_file: &str,
    caller: FileMeta,
    candidates: impl IntoIterator<Item = (u32, FileMeta)>,
) -> Option<u32> {
    let mut copies: Vec<u32> = Vec::new();
    for (id, meta) in candidates {
        if !caller.admits(meta) || facts.is_lexically_scoped(id) {
            continue;
        }
        if copies
            .first()
            .is_some_and(|&first| !facts.is_copy(first, id))
        {
            return None;
        }
        copies.push(id);
    }
    facts.nearest_copy(caller_file, &copies)
}

/// Tier 3: one `name` across the project. `candidates` yields the
/// same-named declarations of the field the call may reach, with their
/// files' metas; it is read again for the nearest copy and the count.
/// A unique pick ([`global_pick`]) binds unless `hidden` (a TS / JS import
/// naming the pick's private ES module) refuses it. Several candidates bind
/// nothing, an invented edge being worse than a missing one, unless
/// `nearest` (a call site) and all are copies of one declaration: then the
/// copy nearest the caller binds ([`nearest_copy`]). A refused or absent
/// copy leaves `NoEdge::Ambiguous` with the number of candidates that fit.
pub fn global<F: TypeFacts + ?Sized, I: Iterator<Item = (u32, FileMeta)>>(
    facts: &F,
    caller_file: &str,
    caller: FileMeta,
    target: ResolveTarget,
    nearest: bool,
    candidates: impl Fn() -> I,
    hidden: impl Fn(u32) -> bool,
) -> Resolution {
    match global_pick(facts, caller, target, candidates()) {
        GlobalPick::Unique(id) if hidden(id) => Resolution::NoEdge(NoEdge::Hidden),
        GlobalPick::Unique(id) => Resolution::bound(id, Tier::Global),
        GlobalPick::Ambiguous => {
            let copy = nearest
                .then(|| nearest_copy(facts, caller_file, caller, candidates()))
                .flatten()
                .filter(|&id| target.kind_predicate()(facts.kind(id)) && !hidden(id));
            match copy {
                Some(id) => Resolution::bound(id, Tier::NearestCopy),
                None => {
                    let fit = candidates()
                        .filter(|&(id, meta)| caller.admits(meta) && !facts.is_lexically_scoped(id))
                        .count();
                    Resolution::NoEdge(NoEdge::Ambiguous(u32::try_from(fit).unwrap_or(u32::MAX)))
                }
            }
        }
        GlobalPick::NoMatch => Resolution::NoEdge(NoEdge::Unresolved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::outcome::Hit;
    use crate::analyzer::same_file_pick::ImplMember;
    use std::borrow::Cow;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    /// How a fake stores its declarations: the index's per-file name table,
    /// or the overlay's flat scan over its nodes.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Layout {
        Index,
        Overlay,
    }

    /// One declaration: `(file, name, kind, owner)`, id = position.
    type Decl = (&'static str, &'static str, NodeKind, Option<&'static str>);

    struct Fake {
        layout: Layout,
        decls: Vec<Decl>,
        /// `(file, name)` -> ids in source order, for [`Layout::Index`].
        by_file: HashMap<(&'static str, &'static str), Vec<u32>>,
        scoped: Vec<u32>,
        /// `(type id, resolved bases)`.
        supertypes: Vec<(u32, Vec<u32>)>,
        /// Every `first_decl_in_file` probe, as `(file, name)`.
        probes: RefCell<Vec<(String, String)>>,
    }

    impl Fake {
        fn new(layout: Layout, decls: &[Decl]) -> Self {
            let mut by_file: HashMap<_, Vec<u32>> = HashMap::new();
            for (id, &(file, name, _, _)) in decls.iter().enumerate() {
                by_file.entry((file, name)).or_default().push(id as u32);
            }
            Self {
                layout,
                decls: decls.to_vec(),
                by_file,
                scoped: Vec::new(),
                supertypes: Vec::new(),
                probes: RefCell::default(),
            }
        }

        fn ids(&self) -> impl Iterator<Item = u32> + '_ {
            0..self.decls.len() as u32
        }

        /// `file`'s declarations of `name`, scoped ones included.
        fn in_file(&self, file: &str, name: &str) -> Vec<u32> {
            match self.layout {
                Layout::Index => self
                    .by_file
                    .iter()
                    .find(|(key, _)| key.0 == file && key.1 == name)
                    .map_or_else(Vec::new, |(_, ids)| ids.clone()),
                Layout::Overlay => self
                    .ids()
                    .filter(|&id| {
                        let (f, n, _, _) = self.decls[id as usize];
                        f == file && n == name
                    })
                    .collect(),
            }
        }

        fn meta(&self, id: u32) -> FileMeta {
            FileMeta::from_path(self.decls[id as usize].0)
        }

        /// Every declaration of `name`, with its file's meta: the Tier 3
        /// candidate stream.
        fn named(&self, name: &str) -> Vec<(u32, FileMeta)> {
            self.ids()
                .filter(|&id| self.decls[id as usize].1 == name)
                .map(|id| (id, self.meta(id)))
                .collect()
        }
    }

    impl PickFacts for Fake {
        fn pick_candidates(&self, file: &str, name: &str) -> Cow<'_, [u32]> {
            let mut ids = self.in_file(file, name);
            ids.retain(|id| !self.scoped.contains(id));
            Cow::Owned(ids)
        }
        fn kind(&self, id: u32) -> NodeKind {
            self.decls[id as usize].2
        }
        fn owner(&self, id: u32) -> Option<&str> {
            self.decls[id as usize].3
        }
        fn impl_member(&self, _id: u32) -> Option<ImplMember<'_>> {
            None
        }
        fn is_namespace_owner(&self, _file: &str, _owner: &str) -> bool {
            false
        }
        fn file_of(&self, id: u32) -> Option<&str> {
            self.decls.get(id as usize).map(|decl| decl.0)
        }
        fn types_named(&self, owner: &str) -> Vec<u32> {
            self.ids()
                .filter(|&id| self.decls[id as usize].1 == owner && self.kind(id).is_type())
                .collect()
        }
        fn is_lexically_scoped(&self, id: u32) -> bool {
            self.scoped.contains(&id)
        }
        fn ancestors(&self, ty: u32) -> Vec<u32> {
            self.supertypes
                .iter()
                .find(|(t, _)| *t == ty)
                .map_or_else(Vec::new, |(_, bases)| bases.clone())
        }
    }

    impl TypeFacts for Fake {
        fn supertypes(&self, ty: u32) -> Option<(Cow<'_, [u32]>, Option<u32>)> {
            self.supertypes
                .iter()
                .find(|(t, _)| *t == ty)
                .map(|(_, bases)| (Cow::Borrowed(bases.as_slice()), None))
        }
        fn name_of(&self, id: u32) -> Option<&str> {
            self.decls.get(id as usize).map(|decl| decl.1)
        }
        fn owned_in_file(
            &self,
            file: &str,
            member: &str,
            owner: &str,
            target: ResolveTarget,
        ) -> Vec<u32> {
            let wanted = target.kind_predicate();
            self.in_file(file, member)
                .into_iter()
                .filter(|&id| wanted(self.kind(id)) && self.owner(id) == Some(owner))
                .collect()
        }
        fn owned_global(
            &self,
            member: &str,
            owner: &str,
            target: ResolveTarget,
            _scope_file: &str,
        ) -> Vec<u32> {
            let wanted = target.kind_predicate();
            self.ids()
                .filter(|&id| {
                    self.decls[id as usize].1 == member
                        && wanted(self.kind(id))
                        && self.owner(id) == Some(owner)
                })
                .collect()
        }
        fn count_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> usize {
            let wanted = target.kind_predicate();
            self.in_file(file, name)
                .into_iter()
                .filter(|&id| wanted(self.kind(id)))
                .count()
        }
        fn global_candidates(
            &self,
            name: &str,
            target: ResolveTarget,
            caller: FileMeta,
        ) -> Vec<u32> {
            let wanted = target.kind_predicate();
            self.named(name)
                .into_iter()
                .filter(|&(id, meta)| {
                    wanted(self.kind(id)) && caller.admits(meta) && !self.is_lexically_scoped(id)
                })
                .map(|(id, _)| id)
                .collect()
        }
        /// Same kind and owner, in another file of the same file name.
        fn is_copy(&self, a: u32, b: u32) -> bool {
            let (a_decl, b_decl) = (self.decls[a as usize], self.decls[b as usize]);
            let leaf = |path: &'static str| path.rsplit('/').next();
            a_decl.0 != b_decl.0
                && a_decl.2 == b_decl.2
                && a_decl.3 == b_decl.3
                && leaf(a_decl.0) == leaf(b_decl.0)
        }
        /// The copy whose path shares the longest prefix with the caller's.
        fn nearest_copy(&self, caller_file: &str, candidates: &[u32]) -> Option<u32> {
            if candidates.len() < 2 {
                return None;
            }
            let shared = |id: u32| {
                caller_file
                    .bytes()
                    .zip(self.decls[id as usize].0.bytes())
                    .take_while(|(a, b)| a == b)
                    .count()
            };
            let best = candidates.iter().copied().max_by_key(|&id| shared(id))?;
            let ties = candidates
                .iter()
                .filter(|&&id| shared(id) == shared(best))
                .count();
            (ties == 1).then_some(best)
        }
        fn return_type_of(&self, _callee: u32) -> Option<&str> {
            None
        }
    }

    impl ResolveFacts for Fake {
        fn call_decls(
            &self,
            file: &str,
            name: &str,
        ) -> impl Iterator<Item = (u32, NodeKind)> + Clone {
            self.in_file(file, name)
                .into_iter()
                .filter(|id| !self.scoped.contains(id))
                .map(|id| (id, self.kind(id)))
                .collect::<Vec<_>>()
                .into_iter()
        }
        fn first_decl_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> Option<u32> {
            self.probes
                .borrow_mut()
                .push((file.to_owned(), name.to_owned()));
            let wanted = target.kind_predicate();
            self.in_file(file, name)
                .into_iter()
                .find(|&id| wanted(self.kind(id)))
        }
    }

    const F: NodeKind = NodeKind::Function;
    const M: NodeKind = NodeKind::Method;
    const C: NodeKind = NodeKind::Class;
    const CALLABLE: ResolveTarget = ResolveTarget::Callable;

    fn both(decls: &[Decl]) -> [Fake; 2] {
        [
            Fake::new(Layout::Index, decls),
            Fake::new(Layout::Overlay, decls),
        ]
    }

    fn bound(id: u32, tier: Tier) -> Resolution {
        Resolution::Bound(Hit {
            id,
            confidence: tier.confidence(),
            tier: Some(tier),
        })
    }

    /// Tier 1 reads the same facts through either store layout and gives
    /// one `Resolution` per case: what the index and the overlay emit for a
    /// clean caller and for the same caller once dirty.
    #[test]
    fn test_ladder_tier1_same_file_binds_both_facts() {
        let decls: [Decl; 6] = [
            ("a.py", "helper", F, None),
            ("a.py", "helper", M, Some("K")),
            ("a.py", "m", M, Some("A")),
            ("a.py", "m", M, Some("B")),
            ("a.py", "only", M, Some("K")),
            ("a.py", "solo", F, None),
        ];
        let owner_k = CallPick {
            owner: Some("K"),
            ..CallPick::default()
        };
        let free_only = CallPick {
            free_only: true,
            ..CallPick::default()
        };
        let untyped = CallPick {
            untyped: true,
            ..CallPick::default()
        };
        let cases: [(&str, Option<CallPick<'_>>, Resolution); 10] = [
            // No call site: the first callable in source order.
            ("helper", None, bound(0, Tier::SameFile)),
            // A free function reached without a receiver stays the pick.
            ("helper", Some(owner_k), bound(0, Tier::SameFile)),
            ("helper", Some(free_only), bound(0, Tier::SameFile)),
            // Two same-file candidates of unrelated owners: no pick.
            ("m", Some(CallPick::default()), Resolution::Unknown),
            // The caller's own type owns the member.
            (
                "m",
                Some(CallPick {
                    owner: Some("B"),
                    ..CallPick::default()
                }),
                bound(3, Tier::SameFile),
            ),
            // A bare call that reaches no member weighs no method.
            ("only", Some(free_only), Resolution::Unknown),
            // A candidate list of one; an untyped receiver never reaches a
            // free function.
            ("solo", Some(CallPick::default()), bound(5, Tier::SameFile)),
            ("solo", Some(untyped), Resolution::Unknown),
            // An empty candidate list, and a whitespace-only name.
            ("absent", None, Resolution::Unknown),
            ("  ", Some(owner_k), Resolution::Unknown),
        ];
        let [index, overlay] = both(&decls);
        for (name, call, want) in cases {
            let got =
                [&index, &overlay].map(|facts| same_file(facts, "a.py", name, CALLABLE, call, &[]));
            assert_eq!(got, [want, want], "{name:?} {call:?}");
        }
        assert_eq!(
            same_file(&index, "a.py", "helper", CALLABLE, None, &[]).edge(),
            Some((0, 1.0)),
            "Tier 1 keeps confidence 1.0"
        );
    }

    /// Tier 1 skips a scoped declaration (I5) and binds its unscoped twin.
    #[test]
    fn test_ladder_tier1_scoped_declaration_returns_unscoped_twin() {
        for mut facts in both(&[("b.py", "helper", F, None), ("b.py", "helper", F, None)]) {
            facts.scoped = vec![0];
            assert_eq!(
                same_file(&facts, "b.py", "helper", CALLABLE, None, &[]),
                bound(1, Tier::SameFile)
            );
        }
    }

    #[test]
    fn test_ladder_global_ambiguous_returns_noedge_with_count() {
        let decls: [Decl; 4] = [
            ("a/x.py", "helper", F, None),
            ("b/y.py", "helper", F, None),
            ("c/z.py", "helper", F, None),
            ("vendor/v/w.py", "helper", F, None),
        ];
        let caller = FileMeta::from_path("app.py");
        let [mut facts, _] = both(&decls);
        let candidates = facts.named("helper");
        let run = |facts: &Fake, nearest| {
            global(
                facts,
                "app.py",
                caller,
                CALLABLE,
                nearest,
                || candidates.iter().copied(),
                |_| false,
            )
        };
        // The vendor candidate does not fit a project caller: three do.
        assert_eq!(run(&facts, true), Resolution::NoEdge(NoEdge::Ambiguous(3)));
        assert_eq!(run(&facts, false), Resolution::NoEdge(NoEdge::Ambiguous(3)));
        // A scoped candidate fits no name tier (I5).
        facts.scoped = vec![2];
        assert_eq!(run(&facts, true), Resolution::NoEdge(NoEdge::Ambiguous(2)));
        assert!(run(&facts, true).call_fallbacks_allowed());
        assert_eq!(run(&facts, true).edge(), None);
    }

    /// The other Tier 3 verdicts: an empty candidate list, one candidate,
    /// a hidden pick, a pick outside the call's kinds, and copies.
    #[test]
    fn test_ladder_global_unique_hidden_kind_and_copies_returns_each_verdict() {
        let caller = FileMeta::from_path("core/pkg/app.py");
        let run = |facts: &Fake, target, nearest, hidden: &dyn Fn(u32) -> bool| {
            let candidates = facts.named("helper");
            global(
                facts,
                "core/pkg/app.py",
                caller,
                target,
                nearest,
                || candidates.iter().copied(),
                hidden,
            )
        };
        let none = |_: u32| false;

        let empty = Fake::new(Layout::Index, &[]);
        assert_eq!(
            run(&empty, CALLABLE, true, &none),
            Resolution::NoEdge(NoEdge::Unresolved)
        );

        let one = Fake::new(Layout::Index, &[("lib/util.py", "helper", F, None)]);
        assert_eq!(run(&one, CALLABLE, true, &none), bound(0, Tier::Global));
        assert_eq!(
            run(&one, CALLABLE, true, &none).edge(),
            Some((0, 0.7)),
            "Tier 3 keeps confidence 0.7"
        );
        assert_eq!(
            run(&one, CALLABLE, true, &|id: u32| id == 0),
            Resolution::NoEdge(NoEdge::Hidden)
        );
        assert_eq!(
            run(&one, ResolveTarget::Method, true, &none),
            Resolution::NoEdge(NoEdge::Unresolved),
            "Rust method syntax never binds a free fn"
        );

        let copies = Fake::new(
            Layout::Index,
            &[
                ("android/core/pkg/util.py", "helper", F, None),
                ("core/pkg/util.py", "helper", F, None),
            ],
        );
        assert_eq!(
            run(&copies, CALLABLE, true, &none),
            bound(1, Tier::NearestCopy)
        );
        assert_eq!(run(&copies, CALLABLE, true, &none).edge(), Some((1, 0.6)));
        assert_eq!(
            run(&copies, CALLABLE, false, &none),
            Resolution::NoEdge(NoEdge::Ambiguous(2)),
            "no call site: no nearest copy"
        );
        assert_eq!(
            run(&copies, CALLABLE, true, &|id: u32| id == 1),
            Resolution::NoEdge(NoEdge::Ambiguous(2)),
            "a hidden copy binds nothing"
        );
        assert_eq!(
            nearest_copy(&copies, "core/pkg/app.py", caller, copies.named("helper")),
            Some(1)
        );
    }

    /// The heritage tier probes each declared base once, in declared order,
    /// and stops at the first that owns the member. A supertype cycle and a
    /// base absent from the graph both end the walk.
    #[test]
    fn test_ladder_heritage_walks_supertype_once_per_level() {
        let decls: [Decl; 3] = [
            ("a.py", "A", C, None),
            ("a.py", "m", M, Some("A")),
            ("b.py", "B", C, None),
        ];
        let file_of_base = |base: &str| match base {
            "A" => Some("a.py"),
            "B" => Some("b.py"),
            _ => None,
        };
        let walk = |facts: &Fake, bases: &[String], call: Option<CallPick<'_>>| {
            let seen = RefCell::new(Vec::new());
            let (resolution, base) = heritage(facts, bases, "m", CALLABLE, call, |base| {
                seen.borrow_mut().push(base.to_owned());
                file_of_base(base)
            });
            (resolution, base.map(str::to_owned), seen.into_inner())
        };
        let names = |bases: &[&str]| bases.iter().map(|b| b.to_string()).collect::<Vec<_>>();

        for mut facts in both(&decls) {
            // `B`'s file declares no `m`: the walk goes on to `A`.
            let bases = names(&["B", "A"]);
            let (resolution, base, seen) = walk(&facts, bases.as_slice(), None);
            assert_eq!(resolution, bound(1, Tier::HeritageScoped));
            assert_eq!(resolution.edge(), Some((1, 0.8)));
            assert_eq!(base.as_deref(), Some("A"));
            assert_eq!(seen, names(&["B", "A"]));

            // The first base that owns the member decides.
            let bases = names(&["A", "B"]);
            let (_, base, seen) = walk(&facts, bases.as_slice(), None);
            assert_eq!((base.as_deref(), seen), (Some("A"), names(&["A"])));

            // A cycle (`A: B`, `B: A`): only the declared bases are probed.
            facts.supertypes = vec![(0, vec![2]), (2, vec![0])];
            let bases = names(&["B"]);
            let (resolution, base, seen) = walk(&facts, bases.as_slice(), None);
            assert_eq!((resolution, base), (Resolution::Unknown, None));
            assert_eq!(seen, names(&["B"]));

            // A base absent from the graph, and a whitespace-only one.
            let bases = names(&["Missing", "  ", "A"]);
            let (resolution, _, seen) = walk(&facts, bases.as_slice(), None);
            assert_eq!(resolution, bound(1, Tier::HeritageScoped));
            assert_eq!(seen, names(&["Missing", "  ", "A"]));

            // No bases; an untyped receiver; a call that reaches no member.
            let no_bases: Vec<String> = Vec::new();
            assert_eq!(
                walk(&facts, no_bases.as_slice(), None),
                (Resolution::Unknown, None, vec![])
            );
            let bases = names(&["A"]);
            for call in [
                CallPick {
                    untyped: true,
                    ..CallPick::default()
                },
                CallPick {
                    free_only: true,
                    ..CallPick::default()
                },
            ] {
                assert_eq!(
                    walk(&facts, bases.as_slice(), Some(call)),
                    (Resolution::Unknown, None, vec![]),
                    "{call:?}"
                );
            }
            let probes = facts.probes.borrow().len();
            assert_eq!(probes, 5, "one file probe per base that has a file");
        }
    }

    /// A base that no file declares costs one lookup and no file probe.
    #[test]
    fn test_ladder_heritage_unresolved_bases_returns_unknown_without_probe() {
        let facts = Fake::new(Layout::Overlay, &[("a.py", "m", M, Some("A"))]);
        let calls = Cell::new(0);
        let bases = vec!["X".to_string(), "Y".to_string()];
        let (resolution, base) = heritage(&facts, &bases, "m", CALLABLE, None, |_| {
            calls.set(calls.get() + 1);
            None::<&str>
        });
        assert_eq!((resolution, base), (Resolution::Unknown, None));
        assert_eq!(calls.get(), 2);
        assert!(facts.probes.borrow().is_empty());
    }
}
