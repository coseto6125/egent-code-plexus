//! The outcome of one resolution rule, and the confidence of each tier.
//!
//! The index (`resolver.rs`) and the overlay (`session/view/resolution.rs`) run the same tier
//! ladder. Each of them held its own copy of the tier confidences and tested
//! "did the ladder end, and may the call-level fallbacks still run" by hand.
//! This module holds both facts once (B §3.1, §3.2). Both call entries
//! (`Resolver::resolve_call` and the overlay's call closure) carry one
//! [`Resolution`] through their call-level fallbacks, and the tiers inside
//! the ladders return it too.

use super::type_ladder::TypeMember;
use super::types::NEAREST_COPY_CONFIDENCE;

/// The tier that bound an edge. The tier alone decides the confidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tier {
    Lexical,
    SameFile,
    ImportScoped,
    QualifierScoped,
    ModuleTree,
    TypeOwned,
    TypeHeritage,
    TypeCandidates,
    HeritageScoped,
    /// A unique name over the whole index. Several candidates are
    /// [`NoEdge::Ambiguous`], never a lower-confidence `Global`.
    Global,
    NearestCopy,
}

impl Tier {
    /// The confidence of an edge this tier binds. The values equal the
    /// consts this replaced: `CONF_*` in `view.rs`, `MT_CONF` and the
    /// `LadderMember` maps in `resolver.rs`.
    pub const fn confidence(self) -> f32 {
        match self {
            Tier::Lexical | Tier::SameFile | Tier::ModuleTree => 1.0,
            Tier::ImportScoped => 0.95,
            Tier::QualifierScoped | Tier::TypeOwned => 0.85,
            Tier::TypeHeritage | Tier::HeritageScoped => 0.8,
            Tier::TypeCandidates | Tier::Global => 0.7,
            Tier::NearestCopy => NEAREST_COPY_CONFIDENCE,
        }
    }

    /// The target and tier of a receiver-typing ladder verdict. `None`:
    /// the verdict binds no edge.
    pub const fn of_member(member: TypeMember) -> Option<(u32, Tier)> {
        match member {
            TypeMember::Owned(id) => Some((id, Tier::TypeOwned)),
            TypeMember::Inherited(id) => Some((id, Tier::TypeHeritage)),
            TypeMember::Candidate(id) => Some((id, Tier::TypeCandidates)),
            TypeMember::NearestCopy(id) => Some((id, Tier::NearestCopy)),
            TypeMember::Ambiguous | TypeMember::NoEdge => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub id: u32,
    pub confidence: f32,
    /// `None`: a ladder tier that does not report its tier yet bound the
    /// edge ([`Resolution::from_ladder`]); `confidence` is its own.
    pub tier: Option<Tier>,
}

/// How far a [`NoEdge`] stops the resolution of one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The remaining ladder tiers stop. The call-level fallbacks still run.
    Ladder,
    /// The ladder and every call-level fallback stop.
    Call,
}

/// Why a rule settled on no edge. Each reason has a fixed [`Reach`] (B §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoEdge {
    Unresolved,
    /// Several candidates fit; the true count, as the C4 contract's
    /// `Evidence::Ambiguous { candidates }` carries it, so a later slice can
    /// tag the "global ambiguous" edge-less call.
    Ambiguous(u32),
    External,
    Hidden,
    Rejected,
    /// A Go selector names a mapped package that lacks the member.
    PackageMissing,
    /// Python `from m import a; a.f()` on the import-only path: `a` is a
    /// value `m` declares, not a submodule.
    DeclaredAttribute,
    /// A typed chain level found no member after its Kotlin retry.
    ChainBroken,
}

impl NoEdge {
    pub const fn reach(self) -> Reach {
        match self {
            NoEdge::PackageMissing | NoEdge::DeclaredAttribute | NoEdge::ChainBroken => Reach::Call,
            _ => Reach::Ladder,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use]
pub enum Resolution {
    Bound(Hit),
    NoEdge(NoEdge),
    /// This rule does not apply. The next tier runs.
    Unknown,
    /// A lexical scope of the file declares the name, but none in reach binds
    /// it (`LexicalFunctions::targets` == `Some(vec![])`). The name tiers
    /// skip; the import tiers and the call-level fallbacks still run.
    Shadowed,
}

// The B §3.1 bound. If rustc lays `Resolution` out larger, flatten the
// `NoEdge` payload into fieldless variants; this assert fails first.
const _: () = assert!(std::mem::size_of::<Resolution>() <= 12);

impl Resolution {
    /// An edge `tier` binds, at the tier's confidence.
    pub const fn bound(id: u32, tier: Tier) -> Self {
        Resolution::Bound(Hit {
            id,
            confidence: tier.confidence(),
            tier: Some(tier),
        })
    }

    /// The verdict of a ladder whose tiers still answer with an edge or none
    /// (the index's `Vec`, the overlay's `Option`). They report neither the
    /// tier nor the reason, so a miss is `NoEdge(Unresolved)`: its Ladder
    /// reach keeps the call-level fallbacks, as an empty answer did.
    pub const fn from_ladder(edge: Option<(u32, f32)>) -> Self {
        match edge {
            Some((id, confidence)) => Resolution::Bound(Hit {
                id,
                confidence,
                tier: None,
            }),
            None => Resolution::NoEdge(NoEdge::Unresolved),
        }
    }

    /// The `(target, confidence)` this resolution emits, once every
    /// call-level step has read it.
    pub const fn edge(&self) -> Option<(u32, f32)> {
        match *self {
            Resolution::Bound(hit) => Some((hit.id, hit.confidence)),
            _ => None,
        }
    }

    /// Whether the call-level fallbacks (Kotlin extension retry, constructor
    /// fallback) may run after the ladder ended here. The single reader of
    /// [`NoEdge::reach`] at the call boundary, on both sides.
    ///
    /// `Bound` answers `false`: a bound call needs no fallback. A bound edge
    /// the untyped post-filter refuses becomes `Unknown` ([`Self::admit`]),
    /// so the constructor fallback still runs after it (B §3.2, row `Bound`).
    pub const fn call_fallbacks_allowed(&self) -> bool {
        match self {
            Resolution::Bound(_) => false,
            Resolution::NoEdge(reason) => matches!(reason.reach(), Reach::Ladder),
            Resolution::Unknown | Resolution::Shadowed => true,
        }
    }

    /// The untyped-member post-filter (B §3.2 `admit`): a bound target that
    /// `admits` refuses leaves the call `Unknown`, as an emptied result did.
    pub fn admit(self, admits: impl FnOnce(u32) -> bool) -> Self {
        self.admit_or(|id| admits(id).then_some(id))
    }

    /// [`Self::admit`] with a substitute: `pick` returns the bound target,
    /// another target that takes its edge at the same tier, or `None` to
    /// leave the call `Unknown`.
    pub fn admit_or(self, pick: impl FnOnce(u32) -> Option<u32>) -> Self {
        match self {
            Resolution::Bound(hit) => pick(hit.id).map_or(Resolution::Unknown, |id| {
                Resolution::Bound(Hit { id, ..hit })
            }),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LADDER_REASONS: [NoEdge; 5] = [
        NoEdge::Unresolved,
        NoEdge::Ambiguous(2),
        NoEdge::External,
        NoEdge::Hidden,
        NoEdge::Rejected,
    ];

    /// Each expected value is the literal the replaced const held, so a
    /// drift in `Tier::confidence` shows here and not only in an edge diff.
    #[test]
    fn test_tier_confidence_matches_each_replaced_const() {
        let replaced = [
            (Tier::SameFile, 1.0),                        // CONF_SAME_FILE
            (Tier::ImportScoped, 0.95),                   // CONF_IMPORT_SCOPED
            (Tier::QualifierScoped, 0.85),                // CONF_QUALIFIER_SCOPED
            (Tier::HeritageScoped, 0.8),                  // CONF_HERITAGE
            (Tier::Global, 0.7),                          // CONF_GLOBAL_UNIQUE
            (Tier::ModuleTree, 1.0),                      // CONF_MODULE_TREE, MT_CONF
            (Tier::Lexical, 1.0),                         // LexicalFunctions::targets
            (Tier::TypeOwned, 0.85),                      // LadderMember::Owned
            (Tier::TypeHeritage, 0.8),                    // LadderMember::Inherited
            (Tier::TypeCandidates, 0.7),                  // LadderMember::Candidate
            (Tier::NearestCopy, NEAREST_COPY_CONFIDENCE), // LadderMember::NearestCopy
        ];
        for (tier, confidence) in replaced {
            assert_eq!(tier.confidence(), confidence, "{tier:?}");
        }
        assert_eq!(NEAREST_COPY_CONFIDENCE, 0.6);
    }

    #[test]
    fn test_tier_of_member_maps_each_verdict_to_the_old_confidence() {
        let bound = [
            (TypeMember::Owned(7), 0.85),
            (TypeMember::Inherited(7), 0.8),
            (TypeMember::Candidate(7), 0.7),
            (TypeMember::NearestCopy(7), 0.6),
        ];
        for (member, confidence) in bound {
            let (id, tier) = Tier::of_member(member).expect("a bound verdict");
            assert_eq!((id, tier.confidence()), (7, confidence), "{member:?}");
        }
        assert_eq!(Tier::of_member(TypeMember::Ambiguous), None);
        assert_eq!(Tier::of_member(TypeMember::NoEdge), None);
    }

    /// A Ladder-reach reason's edge-less call keeps the call-level fallbacks
    /// (Kotlin retry, constructor fallback), as the index does today.
    #[test]
    fn test_call_fallbacks_allowed_ladder_reach_reasons_true() {
        for reason in LADDER_REASONS {
            assert_eq!(reason.reach(), Reach::Ladder, "{reason:?}");
            assert!(
                Resolution::NoEdge(reason).call_fallbacks_allowed(),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn test_noedge_package_missing_allows_fallbacks_false() {
        assert_eq!(NoEdge::PackageMissing.reach(), Reach::Call);
        assert!(!Resolution::NoEdge(NoEdge::PackageMissing).call_fallbacks_allowed());
    }

    #[test]
    fn test_noedge_declared_attribute_allows_fallbacks_false() {
        assert_eq!(NoEdge::DeclaredAttribute.reach(), Reach::Call);
        assert!(!Resolution::NoEdge(NoEdge::DeclaredAttribute).call_fallbacks_allowed());
    }

    #[test]
    fn test_noedge_chain_broken_allows_fallbacks_false() {
        assert_eq!(NoEdge::ChainBroken.reach(), Reach::Call);
        assert!(!Resolution::NoEdge(NoEdge::ChainBroken).call_fallbacks_allowed());
    }

    #[test]
    fn test_call_fallbacks_allowed_unknown_and_shadowed_true_bound_false() {
        assert!(Resolution::Unknown.call_fallbacks_allowed());
        assert!(Resolution::Shadowed.call_fallbacks_allowed());
        assert!(!Resolution::bound(1, Tier::Global).call_fallbacks_allowed());
        assert!(!Resolution::from_ladder(Some((1, 0.7))).call_fallbacks_allowed());
    }

    /// An empty ladder answer reads as a Ladder-reach miss: the fallbacks
    /// still run after it, as they ran after an empty `Vec` / `None`.
    #[test]
    fn test_from_ladder_none_allows_fallbacks_true() {
        let missed = Resolution::from_ladder(None);
        assert_eq!(missed, Resolution::NoEdge(NoEdge::Unresolved));
        assert!(missed.call_fallbacks_allowed());
        assert_eq!(missed.edge(), None);
    }

    /// The edge a ladder bound comes out unchanged, without a tier it did
    /// not report.
    #[test]
    fn test_from_ladder_some_edge_returns_same_target_and_confidence() {
        let bound = Resolution::from_ladder(Some((7, 0.85)));
        assert_eq!(bound.edge(), Some((7, 0.85)));
        assert!(matches!(bound, Resolution::Bound(Hit { tier: None, .. })));
    }

    #[test]
    fn test_bound_tier_returns_tier_confidence() {
        let bound = Resolution::bound(3, Tier::ImportScoped);
        assert_eq!(bound.edge(), Some((3, 0.95)));
        assert!(matches!(
            bound,
            Resolution::Bound(Hit {
                tier: Some(Tier::ImportScoped),
                ..
            })
        ));
    }

    /// A refused bound target leaves the call open to the constructor
    /// fallback; an admitted one, or a call with no edge, is unchanged.
    #[test]
    fn test_admit_refused_bound_returns_unknown_admitted_unchanged() {
        let bound = Resolution::from_ladder(Some((5, 0.7)));
        let refused = bound.admit(|_| false);
        assert_eq!(refused, Resolution::Unknown);
        assert!(refused.call_fallbacks_allowed());
        assert_eq!(bound.admit(|id| id == 5), bound);
        let package_missing = Resolution::NoEdge(NoEdge::PackageMissing);
        assert_eq!(package_missing.admit(|_| false), package_missing);
        assert!(!package_missing.admit(|_| false).call_fallbacks_allowed());
    }

    #[test]
    fn test_noedge_ambiguous_keeps_its_candidate_count() {
        assert_ne!(NoEdge::Ambiguous(2), NoEdge::Ambiguous(3));
        assert_eq!(NoEdge::Ambiguous(2).reach(), Reach::Ladder);
    }
}
