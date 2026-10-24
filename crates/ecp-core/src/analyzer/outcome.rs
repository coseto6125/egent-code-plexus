//! The outcome of one resolution rule, and the confidence of each tier.
//!
//! The index (`resolver.rs`) and the overlay (`view.rs`) run the same tier
//! ladder. Each of them held its own copy of the tier confidences and tested
//! "did the ladder end, and may the call-level fallbacks still run" by hand.
//! This module holds both facts once (B §3.1, §3.2). The ladders adopt
//! [`Resolution`] in later slices; today only [`Tier::confidence`] and
//! [`Tier::of_member`] have readers.

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
    pub tier: Tier,
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
    /// Several candidates fit; the count (saturating) tells a reader how
    /// many, so a later slice can tag the "global ambiguous" edge-less call.
    Ambiguous(u16),
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

/// Whether the call-level fallbacks (Kotlin extension retry, constructor
/// fallback) may run after the ladder ended in `resolution`. The single
/// reader of [`NoEdge::reach`] at the `resolve_call` boundary.
///
/// `Bound` answers `false`: a bound call needs no fallback. The constructor
/// fallback that follows an emptied untyped post-filter is the caller's
/// separate decision (B §3.2, row `Bound`).
pub const fn call_fallbacks_allowed(resolution: &Resolution) -> bool {
    match resolution {
        Resolution::Bound(_) => false,
        Resolution::NoEdge(reason) => matches!(reason.reach(), Reach::Ladder),
        Resolution::Unknown | Resolution::Shadowed => true,
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

    /// `reach_call` here is the `reach()` call on the Ladder reasons: their
    /// edge-less call keeps the call-level fallbacks (Kotlin retry,
    /// constructor fallback), as the index does today.
    #[test]
    fn test_noedge_reach_call_allows_fallbacks_true() {
        for reason in LADDER_REASONS {
            assert_eq!(reason.reach(), Reach::Ladder, "{reason:?}");
            assert!(
                call_fallbacks_allowed(&Resolution::NoEdge(reason)),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn test_noedge_package_missing_allows_fallbacks_false() {
        assert_eq!(NoEdge::PackageMissing.reach(), Reach::Call);
        assert!(!call_fallbacks_allowed(&Resolution::NoEdge(
            NoEdge::PackageMissing
        )));
    }

    #[test]
    fn test_noedge_declared_attribute_allows_fallbacks_false() {
        assert_eq!(NoEdge::DeclaredAttribute.reach(), Reach::Call);
        assert!(!call_fallbacks_allowed(&Resolution::NoEdge(
            NoEdge::DeclaredAttribute
        )));
    }

    #[test]
    fn test_noedge_chain_broken_allows_fallbacks_false() {
        assert_eq!(NoEdge::ChainBroken.reach(), Reach::Call);
        assert!(!call_fallbacks_allowed(&Resolution::NoEdge(
            NoEdge::ChainBroken
        )));
    }

    #[test]
    fn test_call_fallbacks_allowed_unknown_and_shadowed_true_bound_false() {
        let hit = Hit {
            id: 1,
            confidence: Tier::Global.confidence(),
            tier: Tier::Global,
        };
        assert!(call_fallbacks_allowed(&Resolution::Unknown));
        assert!(call_fallbacks_allowed(&Resolution::Shadowed));
        assert!(!call_fallbacks_allowed(&Resolution::Bound(hit)));
    }

    #[test]
    fn test_noedge_ambiguous_keeps_its_candidate_count() {
        assert_ne!(NoEdge::Ambiguous(2), NoEdge::Ambiguous(3));
        assert_eq!(NoEdge::Ambiguous(2).reach(), Reach::Ladder);
    }
}
