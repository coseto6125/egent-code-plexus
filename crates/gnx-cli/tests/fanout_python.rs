//! Integration test: Python getattr fan-out resolution (Phase 2).
use gnx_analyzer::python::PythonProvider;
use gnx_core::analyzer::provider::LanguageProvider;

#[test]
fn dispatch_via_getattr_emits_fanout_ref() {
    let src = include_str!("fixtures/dispatcher_getattr.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    let fanouts: Vec<_> = local
        .fanout_refs
        .iter()
        .filter(|r| r.reason == "reflection-getattr-fanout")
        .collect();

    // Expect 2 fanout refs: from `dispatch` and from `alt_dispatch`.
    assert_eq!(
        fanouts.len(),
        2,
        "expected 2 fanout refs, got {}: {:?}",
        fanouts.len(),
        local.fanout_refs
    );

    // Sources should be dispatch + alt_dispatch.
    let sources: Vec<&str> = fanouts.iter().map(|r| r.source_name.as_str()).collect();
    assert!(sources.contains(&"dispatch"));
    assert!(sources.contains(&"alt_dispatch"));

    // Each must have at least 3 candidates from same-class methods (handle_* + fallback),
    // and exclude dunder methods (__init__).
    for r in &fanouts {
        assert!(
            r.candidates.len() >= 3,
            "expected >= 3 candidates, got {} in {:?}",
            r.candidates.len(),
            r
        );
        // No dunder methods.
        assert!(
            !r.candidates.iter().any(|c| c.starts_with("__")),
            "dunder methods should be excluded: {:?}",
            r.candidates
        );
        // Source method itself NOT in candidates (no self-fanout).
        assert!(
            !r.candidates.contains(&r.source_name),
            "source method should not be in own candidates: src={} cands={:?}",
            r.source_name,
            r.candidates
        );
        // Common handlers present.
        assert!(r.candidates.iter().any(|c| c == "handle_create"));
        assert!(r.candidates.iter().any(|c| c == "handle_delete"));
        assert!(r.candidates.iter().any(|c| c == "handle_update"));
        // base_confidence as spec'd
        assert!((r.base_confidence - 0.5).abs() < 1e-6);
    }
}

#[test]
fn static_string_getattr_is_not_fanout() {
    // getattr(self, "FIXED")() — 字串字面值不該被當作 fan-out
    let src = include_str!("fixtures/dispatcher_getattr.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    // static_dispatch method 不該產生 fanout_ref
    assert!(
        !local
            .fanout_refs
            .iter()
            .any(|r| r.source_name == "static_dispatch"),
        "static_dispatch uses string literal — should not emit fanout: {:?}",
        local.fanout_refs
    );
}

#[test]
fn getattr_without_call_is_not_fanout() {
    // getattr(self, name) 沒有 () invocation 不該抓
    let src = include_str!("fixtures/dispatcher_getattr.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    assert!(
        !local
            .fanout_refs
            .iter()
            .any(|r| r.source_name == "get_method"),
        "get_method just retrieves attr, doesn't invoke — should not emit fanout: {:?}",
        local.fanout_refs
    );
}

#[test]
fn cross_object_getattr_is_not_fanout() {
    // getattr(other, name)() 不是 self → 不該抓（Phase 2 範圍外）
    let src = include_str!("fixtures/dispatcher_getattr.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    assert!(
        !local
            .fanout_refs
            .iter()
            .any(|r| r.source_name == "cross_obj"),
        "cross_obj uses different object — Phase 2 only handles self: {:?}",
        local.fanout_refs
    );
}
