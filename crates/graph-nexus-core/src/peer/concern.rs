//! Concern classification — decide whether a peer dirty event matters.
//!
//! HARD  iff PEER_SYMBOLS ∩ MY_DIRTY_SYMBOLS ≠ ∅
//! SOFT  iff PEER_SYMBOLS ∩ IMPACT(MY_DIRTY_SYMBOLS) ≠ ∅ AND not HARD
//! IGNORE otherwise

use crate::session::overlay::SymbolRef;
use rustc_hash::FxHashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcernKind {
    Hard,
    Soft,
}

#[derive(Debug, Clone)]
pub enum ConcernResult {
    Hit {
        kind: ConcernKind,
        symbol: SymbolRef,
        reason: String,
    },
    Ignore,
}

#[derive(Debug, Clone, Default)]
pub struct ImpactCache {
    impacted_names: FxHashSet<String>,
}

impl ImpactCache {
    pub fn from_set(s: FxHashSet<String>) -> Self {
        Self { impacted_names: s }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.impacted_names.contains(name)
    }

    pub fn invalidate(&mut self) {
        self.impacted_names.clear();
    }

    pub fn refresh(&mut self, names: impl IntoIterator<Item = String>) {
        self.impacted_names = names.into_iter().collect();
    }
}

pub fn classify(
    peer_symbols: &[SymbolRef],
    my_dirty_symbols: &[SymbolRef],
    impact_cache: &ImpactCache,
) -> ConcernResult {
    if my_dirty_symbols.is_empty() || peer_symbols.is_empty() {
        return ConcernResult::Ignore;
    }
    let my_names: FxHashSet<&str> = my_dirty_symbols.iter().map(|s| s.name.as_str()).collect();

    // HARD first — wins over SOFT.
    for p in peer_symbols {
        if my_names.contains(p.name.as_str()) {
            return ConcernResult::Hit {
                kind: ConcernKind::Hard,
                symbol: p.clone(),
                reason: format!("Both sessions modified `{}`", p.name),
            };
        }
    }
    for p in peer_symbols {
        if impact_cache.contains(&p.name) {
            return ConcernResult::Hit {
                kind: ConcernKind::Soft,
                symbol: p.clone(),
                reason: format!(
                    "Peer modified `{}` which is a graph neighbor of your dirty symbols",
                    p.name
                ),
            };
        }
    }
    ConcernResult::Ignore
}
