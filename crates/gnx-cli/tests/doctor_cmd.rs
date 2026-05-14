use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn doctor_lists_framework_coverage() {
    let out = Command::new(gnx_bin())
        .args(["doctor"])
        .output()
        .expect("doctor failed to spawn");

    assert!(
        out.status.success(),
        "doctor exit code: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Framework coverage section present
    assert!(
        stdout.contains("framework_coverage"),
        "missing framework_coverage section\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("fastapi-depends"),
        "missing fastapi-depends entry"
    );
    assert!(
        stdout.contains("django-signal-receiver"),
        "missing django-signal-receiver entry"
    );
    assert!(
        stdout.contains("axum-route-handler"),
        "missing axum-route-handler entry"
    );
    assert!(
        stdout.contains("spring-autowired"),
        "missing spring-autowired entry"
    );

    // Blind-spot catalog
    assert!(
        stdout.contains("blind_spot_catalog"),
        "missing blind_spot_catalog section"
    );
    assert!(stdout.contains("python-eval"), "missing python-eval entry");
    assert!(
        stdout.contains("python-cross-getattr"),
        "missing python-cross-getattr entry"
    );

    // Confidence thresholds
    assert!(
        stdout.contains("high_trust_only"),
        "missing threshold info"
    );
}

#[test]
fn doctor_json_format() {
    let out = Command::new(gnx_bin())
        .args(["doctor", "--format", "json"])
        .output()
        .expect("doctor failed to spawn");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Should be valid JSON
    let _: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("doctor --format json output not valid JSON: {e}\nstdout: {stdout}")
    });

    // Contains expected keys
    assert!(stdout.contains("framework_coverage"));
    assert!(stdout.contains("blind_spot_catalog"));
}
