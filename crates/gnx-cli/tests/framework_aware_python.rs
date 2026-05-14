//! Integration test: Python FastAPI framework refs (T2) and CLI
//! `--high-trust-only` filter behaviour (T5).
use gnx_analyzer::python::PythonProvider;
use gnx_core::analyzer::provider::LanguageProvider;
use std::path::Path;
use std::process::Command;

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

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn setup_fastapi_repo(repo: &Path, home: &Path) {
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/main.py"),
        include_str!("fixtures/fastapi_depends.py"),
    )
    .unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/fw-test.git",
        ],
    );
    run_git(repo, &["add", "-A"]);
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let out = Command::new(gnx_bin())
        .args(["analyze", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("analyze failed to spawn");
    assert!(
        out.status.success(),
        "analyze failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn high_trust_only_filters_framework_edges_in_impact() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_fastapi_repo(repo, home_tmp.path());

    // The only edges reaching get_db come from FastAPI `Depends(get_db)` —
    // emitted as framework refs with confidence 0.6 ("fastapi-depends").
    //
    //   read_user --(Depends, 0.6)--> get_current_user --(Depends, 0.6)--> get_db
    //
    // Default impact upstream from get_db must include at least one caller
    // (get_current_user) reached via the low-confidence edge.
    let target_uid = "Function:src/main.py:get_db";
    let default_out = Command::new(gnx_bin())
        .args([
            "impact",
            "--repo",
            ".",
            "--target",
            target_uid,
            "--direction",
            "upstream",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", home_tmp.path())
        .output()
        .expect("impact failed to spawn");
    assert!(
        default_out.status.success(),
        "default impact failed: {}",
        String::from_utf8_lossy(&default_out.stderr)
    );
    let default_json: serde_json::Value = serde_json::from_slice(&default_out.stdout).unwrap();
    let default_count = default_json["impact"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        default_count > 1,
        "default upstream from get_db must traverse Depends edges (got count={default_count}): {default_json}"
    );

    // --high-trust-only: framework edges (confidence 0.6) filtered → only
    // the target node itself remains in the BFS result.
    let strict_out = Command::new(gnx_bin())
        .args([
            "impact",
            "--repo",
            ".",
            "--target",
            target_uid,
            "--direction",
            "upstream",
            "--format",
            "json",
            "--high-trust-only",
        ])
        .current_dir(repo)
        .env("HOME", home_tmp.path())
        .output()
        .expect("strict impact failed to spawn");
    assert!(
        strict_out.status.success(),
        "strict impact failed: {}",
        String::from_utf8_lossy(&strict_out.stderr)
    );
    let strict_json: serde_json::Value = serde_json::from_slice(&strict_out.stdout).unwrap();
    let strict_count = strict_json["impact"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);

    assert!(
        strict_count < default_count,
        "--high-trust-only must produce a smaller affected set (got strict={strict_count}, default={default_count}): default={default_json} strict={strict_json}"
    );
}

#[test]
fn fastapi_route_decorators_create_framework_refs() {
    let src = include_str!("fixtures/fastapi_routes.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    // Expect 4 fastapi-route-* refs:
    //   app    --fastapi-route-get-->     read_user
    //   app    --fastapi-route-post-->    create_item
    //   router --fastapi-route-delete--> delete_item
    //   router --fastapi-route-patch-->  patch_item
    let route_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason.starts_with("fastapi-route-"))
        .collect();
    assert_eq!(
        route_refs.len(),
        4,
        "expected 4 fastapi-route-* refs, got {}: {:?}",
        route_refs.len(),
        local.framework_refs
    );

    let triples: Vec<(&str, &str, &str)> = route_refs
        .iter()
        .map(|r| {
            (
                r.source_name.as_str(),
                r.target_name.as_str(),
                r.reason.as_str(),
            )
        })
        .collect();
    for expected in [
        ("app", "read_user", "fastapi-route-get"),
        ("app", "create_item", "fastapi-route-post"),
        ("router", "delete_item", "fastapi-route-delete"),
        ("router", "patch_item", "fastapi-route-patch"),
    ] {
        assert!(
            triples.contains(&expected),
            "missing {:?} in {:?}",
            expected,
            triples
        );
    }

    // Negative: @app.middleware MUST NOT match (not an HTTP verb).
    assert!(
        !triples.iter().any(|(_, t, _)| *t == "middleware_fn"),
        "middleware_fn should not be captured: {:?}",
        triples
    );

    for r in &route_refs {
        assert!(r.confidence > 0.0 && r.confidence <= 1.0);
    }
}

#[test]
fn django_urlpatterns_create_framework_refs() {
    let src = include_str!("fixtures/django_urls.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("urls.py".as_ref(), src.as_bytes())
        .unwrap();

    let refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "django-url-path")
        .collect();
    assert_eq!(
        refs.len(),
        4,
        "expected 4 django-url-path refs, got {}: {:?}",
        refs.len(),
        local.framework_refs
    );

    let targets: Vec<&str> = refs.iter().map(|r| r.target_name.as_str()).collect();
    for expected in ["user_list", "user_detail", "login_view", "fallback_handler"] {
        assert!(
            targets.contains(&expected),
            "missing {} in {:?}",
            expected,
            targets
        );
    }

    // Negative: `os.path("/tmp")` outside urlpatterns must NOT be captured.
    // Asserting exactly 4 above already enforces this.

    for r in &refs {
        assert!(r.confidence > 0.0 && r.confidence <= 1.0);
    }
}

#[test]
fn celery_task_decorators_create_framework_refs() {
    let src = include_str!("fixtures/celery_tasks.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("tasks.py".as_ref(), src.as_bytes())
        .unwrap();

    let refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "celery-task")
        .collect();
    assert_eq!(
        refs.len(),
        3,
        "expected 3 celery-task refs, got {}: {:?}",
        refs.len(),
        local.framework_refs
    );

    let targets: Vec<&str> = refs.iter().map(|r| r.target_name.as_str()).collect();
    for expected in ["send_email", "process_data", "retry_job"] {
        assert!(
            targets.contains(&expected),
            "missing {} in {:?}",
            expected,
            targets
        );
    }

    // Negative: @cached_property MUST NOT match.
    assert!(
        !targets.contains(&"something"),
        "cached_property should not be captured: {:?}",
        targets
    );

    for r in &refs {
        assert!(r.confidence > 0.0 && r.confidence <= 1.0);
    }
}
