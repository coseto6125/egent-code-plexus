use graph_nexus_cli::commands::group::extractors::{
    registry, ExtractorKind,
};

#[test]
fn registry_lists_first_wave_languages() {
    let entries = registry();
    let go_http = entries
        .iter()
        .find(|e| e.lang == "go" && e.kind == ExtractorKind::Http);
    assert!(go_http.is_some(), "missing go/http extractor");
    assert!(entries.len() >= 10, "got {} extractors, expected ≥10", entries.len());
}

#[test]
fn extractor_kinds_distinct() {
    let entries = registry();
    let http_count = entries.iter().filter(|e| e.kind == ExtractorKind::Http).count();
    let grpc_count = entries.iter().filter(|e| e.kind == ExtractorKind::Grpc).count();
    assert_eq!(http_count, 5);
    assert_eq!(grpc_count, 5);
}
