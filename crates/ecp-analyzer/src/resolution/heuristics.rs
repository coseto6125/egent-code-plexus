//! Heuristics for resolving symbols to global nodes.
//! Ports the exact ResolutionTier and confidence scoring from original GitNexus.

use ecp_core::analyzer::outcome::Tier;
pub use ecp_core::analyzer::types::{NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResolutionTier {
    SameFile,
    ImportScoped,
    /// Tier 2.5 — callee carries a qualifier (`A::new`, `Cls.method`) that
    /// resolves uniquely as a Type via the regular tiers, and the member is
    /// found in the qualifier's defining file. Higher precision than the
    /// kind-filtered bare-name Tier 3 because the qualifier scopes lookup
    /// to one file rather than relying on global uniqueness.
    QualifierScoped,
    /// Tier 2.75 — bare-name lookup resolves via the caller's enclosing
    /// class heritage chain. When Tiers 1/2/2.5 miss but the caller sits
    /// inside a class with `extends`/`include`/mixin annotations, we treat
    /// each parent name as a qualifier and probe the parent's defining
    /// file. Plugs the cross-file mixin gap (Ruby `include Foo` + Forwardable
    /// `def_delegators`; Java/Kotlin/C# inherited methods reached via
    /// unqualified callsites).
    HeritageScoped,
    Global,
    /// Every candidate is one declaration repeated in duplicated source
    /// trees (`android/` beside the main tree, a vendored copy, two packages
    /// publishing one module), and the copy nearest the caller by path is
    /// taken. Path proximity is a heuristic, so the edge stays below the
    /// Global tier and carries its own reason ([`NEAREST_COPY_REASON`]).
    NearestCopy,
}

impl ResolutionTier {
    /// Returns the base confidence score for the resolution tier. A tier
    /// the overlay shares reads [`Tier::confidence`], so the index and the
    /// overlay hold each value once.
    pub fn base_confidence(&self) -> f32 {
        match self {
            ResolutionTier::SameFile => Tier::SameFile.confidence(),
            ResolutionTier::ImportScoped => Tier::ImportScoped.confidence(),
            ResolutionTier::QualifierScoped => Tier::QualifierScoped.confidence(),
            ResolutionTier::HeritageScoped => Tier::HeritageScoped.confidence(),
            ResolutionTier::Global => Tier::Global.confidence(),
            ResolutionTier::NearestCopy => Tier::NearestCopy.confidence(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each shared tier keeps the literal it held before it read [`Tier`].
    #[test]
    fn test_base_confidence_shared_tiers_returns_unchanged_values() {
        let unchanged = [
            (ResolutionTier::SameFile, 1.0),
            (ResolutionTier::ImportScoped, 0.95),
            (ResolutionTier::QualifierScoped, 0.85),
            (ResolutionTier::HeritageScoped, 0.8),
            (ResolutionTier::Global, 0.7),
            (ResolutionTier::NearestCopy, 0.6),
        ];
        for (tier, confidence) in unchanged {
            assert_eq!(tier.base_confidence(), confidence, "{tier:?}");
        }
    }
}
