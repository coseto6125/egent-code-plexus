use graph_nexus_core::config::Config;

#[test]
fn group_section_defaults_when_absent() {
    let toml = r#"
[output]
default_format = "toon"
"#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!((cfg.group.bm25_threshold - 0.6).abs() < f32::EPSILON);
    assert_eq!(cfg.group.max_candidates_per_step, 16);
    assert!(cfg.group.exclude_links_paths.is_empty());
    assert!(!cfg.group.exclude_links_param_only_paths);
    assert_eq!(cfg.group.cross_depth, 1);
    assert_eq!(cfg.group.local_impact_timeout_ms, 5000);
}

#[test]
fn group_section_honours_overrides() {
    let toml = r#"
[group]
bm25_threshold = 0.75
max_candidates_per_step = 32
exclude_links_paths = ["/health", "/metrics"]
exclude_links_param_only_paths = true
cross_depth = 2
local_impact_timeout_ms = 8000
"#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!((cfg.group.bm25_threshold - 0.75).abs() < f32::EPSILON);
    assert_eq!(cfg.group.max_candidates_per_step, 32);
    assert_eq!(
        cfg.group.exclude_links_paths,
        vec!["/health".to_string(), "/metrics".to_string()]
    );
    assert!(cfg.group.exclude_links_param_only_paths);
    assert_eq!(cfg.group.cross_depth, 2);
    assert_eq!(cfg.group.local_impact_timeout_ms, 8000);
}
