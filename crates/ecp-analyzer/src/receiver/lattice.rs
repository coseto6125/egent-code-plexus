//! The receiver lattice: what one cell holds at one program point.

use super::ir::TypeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abstract {
    /// No path reaches this point.
    Unreachable,
    /// A local that no reaching path has assigned yet.
    Unbound,
    Known(TypeId),
    Unknown(UnknownCause),
}

/// Ordered by severity; `join` keeps the greater.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnknownCause {
    /// No path gave the binding a type (untyped param, untyped call result).
    NoEvidence,
    /// Reachable paths disagree: two types, or a type on some paths only.
    Conflict,
    /// The lowering met a construct it does not model (`eval`, `extract`, `$$v`, `with`).
    Unsupported,
    /// A by-ref capture or alias may write the cell.
    Escape,
    /// The function exceeded the analysis budget.
    Budget,
}

impl UnknownCause {
    /// Every cause, in severity order.
    pub const ALL: [UnknownCause; 5] = [
        UnknownCause::NoEvidence,
        UnknownCause::Conflict,
        UnknownCause::Unsupported,
        UnknownCause::Escape,
        UnknownCause::Budget,
    ];
}

impl Abstract {
    /// Whether a later assignment leaves the cell as it is: a by-ref escape
    /// may write it at any time, and the budget answer covers the function.
    pub fn is_sticky(self) -> bool {
        matches!(
            self,
            Abstract::Unknown(UnknownCause::Escape | UnknownCause::Budget)
        )
    }
}

/// The least upper bound of two states. Commutative, associative and
/// idempotent; a join only moves up, so an annotation on another path never
/// restores `Known`.
pub fn join(a: Abstract, b: Abstract) -> Abstract {
    use Abstract::{Known, Unbound, Unknown, Unreachable};
    match (a, b) {
        (Unreachable, other) | (other, Unreachable) => other,
        (Unknown(c), Unknown(d)) => Unknown(c.max(d)),
        (Unbound, Unbound) => Unbound,
        (Unbound, Unknown(c)) | (Unknown(c), Unbound) => Unknown(c),
        (Known(x), Known(y)) if x == y => Known(x),
        (Known(_), Known(_)) | (Known(_), Unbound) | (Unbound, Known(_)) => {
            Unknown(UnknownCause::Conflict)
        }
        (Known(_), Unknown(c)) | (Unknown(c), Known(_)) => Unknown(c.max(UnknownCause::Conflict)),
    }
}
