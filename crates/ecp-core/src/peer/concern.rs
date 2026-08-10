//! Concern classification — decide whether a peer dirty event matters.
//!
//! HARD  iff the peer has an overlay entry for a file I also have an entry for
//! SOFT  iff a declaration in the peer's dirty file is in IMPACT(my dirty
//!       symbols), AND not HARD
//! IGNORE otherwise
//!
//! **`dirty_symbols` is a file's declaration list, not a change set.** The
//! overlay writer re-parses the whole file and records every declaration in it;
//! nothing compares against the base graph, so it cannot say which declarations
//! the session actually edited. HARD is therefore stated at the granularity the
//! evidence supports — both sessions hold an overlay entry for this file — and
//! never as "both sessions modified `foo`", which is false whenever two
//! sessions edit different functions of one file.
//!
//! **An overlay entry means "differs from the published graph".** That is
//! usually an uncommitted edit, but it also covers a clean worktree whose index
//! has not caught up with HEAD, so HARD can fire between two sessions that have
//! merely committed and not reindexed. The reason string says so rather than
//! asserting a merge conflict — see FU-2026-08-09 (peers overlay-vs-worktree).
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
        /// The peer's dirty file. Always known — it is what the manifest
        /// records.
        file: String,
        /// The declaration the classification actually rests on. `None` for
        /// HARD, whose evidence is the shared file and nothing finer: the
        /// manifest lists every declaration in a re-parsed file and compares
        /// none of them against the base graph, so naming one would assert an
        /// edit no one observed.
        symbol: Option<SymbolRef>,
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

/// `peer_file` is the peer manifest's own entry key, and `my_dirty_files` the
/// keys of mine. HARD is decided on those, never on the declaration lists:
/// a file the parser finds no declarations in — imports-only, or one whose
/// parse failed — is still a file both sessions are editing, and deriving the
/// path from `dirty_symbols` would drop it while `peers status --pairs`, which
/// reads the keys directly, still reported it.
pub fn classify(
    peer_file: &str,
    peer_symbols: &[SymbolRef],
    my_dirty_files: &[String],
    my_dirty_symbols: &[SymbolRef],
    impact_cache: &ImpactCache,
) -> ConcernResult {
    // HARD first — wins over SOFT.
    if my_dirty_files.iter().any(|f| f == peer_file) {
        let reason = format!(
            "Both sessions have {peer_file} in their overlay. Neither manifest records WHICH \
             declarations changed — and an overlay entry means \"differs from the published \
             graph\", which is uncommitted edits OR commits the index has not caught up with. \
             Review the file."
        );
        return ConcernResult::Hit {
            kind: ConcernKind::Hard,
            file: peer_file.to_string(),
            symbol: None,
            reason,
        };
    }
    if my_dirty_symbols.is_empty() || peer_symbols.is_empty() {
        return ConcernResult::Ignore;
    }
    for p in peer_symbols {
        if impact_cache.contains(&p.file, &p.name) {
            return ConcernResult::Hit {
                kind: ConcernKind::Soft,
                file: p.file.clone(),
                symbol: Some(p.clone()),
                reason: format!(
                    "Peer has {} dirty; its `{}` is a graph neighbor of your dirty symbols",
                    p.file, p.name
                ),
            };
        }
    }
    ConcernResult::Ignore
}
