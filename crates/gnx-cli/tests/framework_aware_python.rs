//! Integration test: Python FastAPI framework refs (T2).
use gnx_analyzer::python::PythonProvider;
use gnx_core::analyzer::provider::LanguageProvider;

#[test]
fn fastapi_depends_creates_low_confidence_framework_refs() {
    let src = include_str!("fixtures/fastapi_depends.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    // Expect 2 framework_refs from Depends():
    //   get_current_user  --fastapi-depends-->  get_db
    //   read_user         --fastapi-depends-->  get_current_user
    let depends_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "fastapi-depends")
        .collect();
    assert_eq!(
        depends_refs.len(),
        2,
        "expected 2 fastapi-depends refs, got {}: {:?}",
        depends_refs.len(),
        local.framework_refs
    );

    let pairs: Vec<(&str, &str)> = depends_refs
        .iter()
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        pairs.contains(&("get_current_user", "get_db")),
        "missing get_current_user→get_db: {:?}",
        pairs
    );
    assert!(
        pairs.contains(&("read_user", "get_current_user")),
        "missing read_user→get_current_user: {:?}",
        pairs
    );

    // Confidence must be < 1.0 and reason tagged.
    for r in &depends_refs {
        assert!(
            r.confidence > 0.0 && r.confidence < 1.0,
            "confidence out of range: {}",
            r.confidence
        );
    }
}
