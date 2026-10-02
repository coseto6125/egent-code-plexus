/// Heuristics for resolving symbols to global nodes.
/// Ports the exact ResolutionTier and confidence scoring from original GitNexus.

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FallbackReason {
    ImplicitSelf,
    VueComponent,
    Unknown,
}

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
    Fallback(FallbackReason),
}

impl ResolutionTier {
    /// Returns the base confidence score for the resolution tier.
    pub fn base_confidence(&self) -> f32 {
        match self {
            ResolutionTier::SameFile => 1.0,
            ResolutionTier::ImportScoped => 0.95,
            ResolutionTier::QualifierScoped => 0.85,
            ResolutionTier::HeritageScoped => 0.8,
            ResolutionTier::Global => 0.7,
            ResolutionTier::Fallback(reason) => match reason {
                FallbackReason::ImplicitSelf => 0.8,
                FallbackReason::VueComponent => 0.8,
                FallbackReason::Unknown => 0.4,
            },
        }
    }
}
