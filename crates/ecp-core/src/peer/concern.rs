//! Concern classification — decide whether a peer dirty event matters.
//!
//! HARD  iff PEER_SYMBOLS ∩ MY_DIRTY_SYMBOLS ≠ ∅
//! SOFT  iff PEER_SYMBOLS ∩ IMPACT(MY_DIRTY_SYMBOLS) ≠ ∅ AND not HARD
//! IGNORE otherwise
//!
//! Every intersection is on `(file, name)`, never the bare name. Both sessions
//! index the same repo, so their paths agree; a name alone does not identify a
//! symbol — this repo's own graph holds 66 definitions of `run` and 87 of
//! `ecp_bin`, so name-only matching would report "both sessions modified `run`"
//! for two sessions that never touched the same code.

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

/// A symbol's identity for matching purposes: repo-relative file plus name.
pub type SymbolKey = (String, String);

pub fn symbol_key(s: &SymbolRef) -> SymbolKey {
    (s.file.clone(), s.name.clone())
}

#[derive(Debug, Clone, Default)]
pub struct ImpactCache {
    impacted: FxHashSet<SymbolKey>,
}

impl ImpactCache {
    pub fn from_set(s: FxHashSet<SymbolKey>) -> Self {
        Self { impacted: s }
    }

    pub fn contains(&self, file: &str, name: &str) -> bool {
        // Borrowing a `(&str, &str)` out of a `(String, String)` key needs an
        // owned probe; the set is only read once per peer symbol per event.
        self.impacted
            .contains(&(file.to_string(), name.to_string()))
    }

    pub fn invalidate(&mut self) {
        self.impacted.clear();
    }

    pub fn refresh(&mut self, keys: impl IntoIterator<Item = SymbolKey>) {
        self.impacted = keys.into_iter().collect();
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
    let mine: FxHashSet<(&str, &str)> = my_dirty_symbols
        .iter()
        .map(|s| (s.file.as_str(), s.name.as_str()))
        .collect();

    // HARD first — wins over SOFT.
    for p in peer_symbols {
        if mine.contains(&(p.file.as_str(), p.name.as_str())) {
            return ConcernResult::Hit {
                kind: ConcernKind::Hard,
                symbol: p.clone(),
                reason: format!("Both sessions modified `{}` in {}", p.name, p.file),
            };
        }
    }
    for p in peer_symbols {
        if impact_cache.contains(&p.file, &p.name) {
            return ConcernResult::Hit {
                kind: ConcernKind::Soft,
                symbol: p.clone(),
                reason: format!(
                    "Peer modified `{}` in {} which is a graph neighbor of your dirty symbols",
                    p.name, p.file
                ),
            };
        }
    }
    ConcernResult::Ignore
}
