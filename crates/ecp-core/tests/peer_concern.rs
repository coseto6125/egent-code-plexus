use ecp_core::peer::concern::{classify, ConcernKind, ConcernResult, ImpactCache, SymbolKey};
use ecp_core::session::overlay::{SymbolKind, SymbolRef};
use rustc_hash::FxHashSet;

fn sym(name: &str, file: &str) -> SymbolRef {
    SymbolRef {
        name: name.into(),
        kind: SymbolKind::Function,
        file: file.into(),
        line_start: 1,
        line_end: 10,
    }
}

fn cache_of(keys: &[(&str, &str)]) -> ImpactCache {
    let set: FxHashSet<SymbolKey> = keys
        .iter()
        .map(|(f, n)| ((*f).to_string(), (*n).to_string()))
        .collect();
    ImpactCache::from_set(set)
}

fn empty_cache() -> ImpactCache {
    ImpactCache::from_set(FxHashSet::default())
}

#[test]
fn hard_when_same_symbol_modified() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("verify_token", "src/auth.rs")];
    let r = classify(&peer, &mine, &empty_cache());
    assert!(matches!(
        r,
        ConcernResult::Hit {
            kind: ConcernKind::Hard,
            ..
        }
    ));
}

/// The identity is `(file, name)`, never the name alone. This repo's own graph
/// holds 66 definitions of `run`; matching on the name would tell two sessions
/// that never touched the same code that they both modified it.
#[test]
fn same_name_in_different_files_is_not_a_hard_overlap() {
    let mine = vec![sym("run", "crates/ecp-cli/src/commands/watch.rs")];
    let peer = vec![sym("run", "crates/ecp-cli/src/commands/impact/mod.rs")];
    assert!(matches!(
        classify(&peer, &mine, &empty_cache()),
        ConcernResult::Ignore
    ));
}

/// Same for SOFT: a cached neighbour named `run` must not match every other
/// `run` in the repo.
#[test]
fn same_name_in_different_files_is_not_a_soft_overlap() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("run", "src/unrelated.rs")];
    let cache = cache_of(&[("src/server.rs", "run")]);
    assert!(matches!(
        classify(&peer, &mine, &cache),
        ConcernResult::Ignore
    ));
}

#[test]
fn soft_when_peer_is_one_hop_neighbor() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("login_handler", "src/handlers/login.rs")];
    let cache = cache_of(&[("src/handlers/login.rs", "login_handler")]);
    let r = classify(&peer, &mine, &cache);
    assert!(matches!(
        r,
        ConcernResult::Hit {
            kind: ConcernKind::Soft,
            ..
        }
    ));
}

#[test]
fn ignore_when_unrelated() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("format_money", "src/utils/money.rs")];
    let r = classify(&peer, &mine, &empty_cache());
    assert!(matches!(r, ConcernResult::Ignore));
}

#[test]
fn hard_takes_precedence_over_soft() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![
        sym("verify_token", "src/auth.rs"),
        sym("login_handler", "src/login.rs"),
    ];
    let cache = cache_of(&[("src/login.rs", "login_handler")]);
    match classify(&peer, &mine, &cache) {
        ConcernResult::Hit {
            kind: ConcernKind::Hard,
            symbol,
            ..
        } => {
            assert_eq!(symbol.name, "verify_token");
        }
        other => panic!("expected Hard, got {other:?}"),
    }
}

/// The reason string is read by an LLM, so it has to name the file it matched
/// on — "both sessions modified `run`" is unactionable when the repo has 66.
#[test]
fn hard_reason_names_the_file_it_matched_on() {
    let mine = vec![sym("run", "src/a.rs")];
    let peer = vec![sym("run", "src/a.rs")];
    match classify(&peer, &mine, &empty_cache()) {
        ConcernResult::Hit { reason, .. } => assert!(
            reason.contains("src/a.rs"),
            "reason must carry the file: {reason}"
        ),
        other => panic!("expected a hit, got {other:?}"),
    }
}

#[test]
fn empty_my_dirty_yields_ignore() {
    let mine = vec![];
    let peer = vec![sym("anything", "src/x.rs")];
    assert!(matches!(
        classify(&peer, &mine, &empty_cache()),
        ConcernResult::Ignore
    ));
}

#[test]
fn impact_cache_refresh_replaces_contents() {
    let mut c = empty_cache();
    c.refresh([
        ("src/a.rs".to_string(), "foo".to_string()),
        ("src/b.rs".to_string(), "bar".to_string()),
    ]);
    assert!(c.contains("src/a.rs", "foo"));
    assert!(c.contains("src/b.rs", "bar"));
    c.refresh([("src/c.rs".to_string(), "baz".to_string())]);
    assert!(!c.contains("src/a.rs", "foo"));
    assert!(c.contains("src/c.rs", "baz"));
}

#[test]
fn impact_cache_matches_on_file_and_name_together() {
    let c = cache_of(&[("src/a.rs", "foo")]);
    assert!(c.contains("src/a.rs", "foo"));
    assert!(!c.contains("src/b.rs", "foo"), "name alone must not match");
    assert!(!c.contains("src/a.rs", "bar"), "file alone must not match");
}

#[test]
fn impact_cache_invalidate_clears_contents() {
    let mut c = ImpactCache::default();
    c.refresh([("src/a.rs".to_string(), "foo".to_string())]);
    c.invalidate();
    assert!(!c.contains("src/a.rs", "foo"));
}
