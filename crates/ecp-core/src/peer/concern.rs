//! Concern classification — decide whether a peer dirty event matters.
//!
//! HARD  iff the peer has a file dirty that I also have dirty
//! SOFT  iff a declaration in the peer's dirty file is in IMPACT(my dirty
//!       symbols), AND not HARD
//! IGNORE otherwise
//!
//! **`dirty_symbols` is a file's declaration list, not a change set.** The
//! overlay writer re-parses the whole dirty file and records every declaration
//! in it; nothing compares against the base graph, so it cannot say which
//! declarations the session actually edited. HARD is therefore stated at the
//! granularity the evidence supports — "both sessions have this file dirty" —
//! and never as "both sessions modified `foo`", which would be false whenever
//! two sessions edit different functions of the same file. That framing is also
//! the one that matters: a shared dirty file is exactly what becomes a merge
//! conflict.
//!
//! SOFT matches on `(file, name)`, never the bare name. This repo's own graph
//! holds 66 definitions of `run` and 87 of `ecp_bin`, so a name-only match
//! would fire between sessions that never touched related code. Two same-named
//! declarations in the SAME file (`impl Foo { fn run }` / `impl Bar { fn run }`)
//! still collapse together — `SymbolRef` carries no owner — which can only
//! widen SOFT within one file, never across files.

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
    let my_files: FxHashSet<&str> = my_dirty_symbols.iter().map(|s| s.file.as_str()).collect();

    // HARD first — wins over SOFT.
    for p in peer_symbols {
        if my_files.contains(p.file.as_str()) {
            return ConcernResult::Hit {
                kind: ConcernKind::Hard,
                symbol: p.clone(),
                reason: format!(
                    "Both sessions have {} dirty (it declares `{}`; neither manifest records \
                     WHICH declarations changed, so review the file, not just this symbol)",
                    p.file, p.name
                ),
            };
        }
    }
    for p in peer_symbols {
        if impact_cache.contains(&p.file, &p.name) {
            return ConcernResult::Hit {
                kind: ConcernKind::Soft,
                symbol: p.clone(),
                reason: format!(
                    "Peer has {} dirty; its `{}` is a graph neighbor of your dirty symbols",
                    p.file, p.name
                ),
            };
        }
    }
    ConcernResult::Ignore
}
