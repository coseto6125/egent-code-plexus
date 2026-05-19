use cgn_core::peer::concern::{classify, ConcernKind, ConcernResult, ImpactCache};
use cgn_core::session::overlay::{SymbolKind, SymbolRef};
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

#[test]
fn hard_when_same_symbol_modified() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("verify_token", "src/auth.rs")];
    let cache = ImpactCache::from_set(FxHashSet::default());
    let r = classify(&peer, &mine, &cache);
    assert!(matches!(
        r,
        ConcernResult::Hit {
            kind: ConcernKind::Hard,
            ..
        }
    ));
}

#[test]
fn soft_when_peer_is_one_hop_neighbor() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("login_handler", "src/handlers/login.rs")];
    let mut impacted = FxHashSet::default();
    impacted.insert("login_handler".to_string());
    let cache = ImpactCache::from_set(impacted);
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
    let cache = ImpactCache::from_set(FxHashSet::default());
    let r = classify(&peer, &mine, &cache);
    assert!(matches!(r, ConcernResult::Ignore));
}

#[test]
fn hard_takes_precedence_over_soft() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![
        sym("verify_token", "src/auth.rs"),
        sym("login_handler", "src/login.rs"),
    ];
    let mut impacted = FxHashSet::default();
    impacted.insert("login_handler".into());
    let cache = ImpactCache::from_set(impacted);
    let r = classify(&peer, &mine, &cache);
    match r {
        ConcernResult::Hit {
            kind: ConcernKind::Hard,
            symbol,
            ..
        } => {
            assert_eq!(symbol.name, "verify_token");
        }
        _ => panic!("expected Hard"),
    }
}

#[test]
fn empty_my_dirty_yields_ignore() {
    let mine = vec![];
    let peer = vec![sym("anything", "src/x.rs")];
    let cache = ImpactCache::from_set(FxHashSet::default());
    assert!(matches!(
        classify(&peer, &mine, &cache),
        ConcernResult::Ignore
    ));
}

#[test]
fn impact_cache_refresh_replaces_contents() {
    let mut c = ImpactCache::from_set(FxHashSet::default());
    c.refresh(["foo".to_string(), "bar".to_string()]);
    assert!(c.contains("foo"));
    assert!(c.contains("bar"));
    c.refresh(["baz".to_string()]);
    assert!(!c.contains("foo"));
    assert!(c.contains("baz"));
}

#[test]
fn impact_cache_invalidate_clears_contents() {
    let mut c = ImpactCache::default();
    c.refresh(["foo".to_string()]);
    c.invalidate();
    assert!(!c.contains("foo"));
}
