//! End-to-end over the axum router with stand-ins for `ecp`, `git` and
//! `curl`: every spawn is logged to a file the assertions read back, so the
//! tests pin the argv and cwd the real binaries would receive.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use clap::CommandFactory;
use ecp_cli::cli::Cli;
use ecp_demo::app::{router, AppState};
use ecp_demo::ratelimit::RateLimiter;
use ecp_demo::repos::{Programs, RepoStore, StoreConfig};
use ecp_demo::runner::Runner;
use ecp_demo::tools::demo_tools;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

struct Harness {
    _dir: tempfile::TempDir,
    root: PathBuf,
    log: PathBuf,
    app: Router,
}

#[derive(Default, Clone, Copy)]
struct Stubs {
    /// GitHub API answer: `Some(size_kb)` → 200 with that size; `None` → 403.
    /// A repo whose owner is `big` always reports 5000 KB.
    api_size_kb: Option<u64>,
    api_404: bool,
    /// Bytes the fake clone writes into the checkout.
    checkout_bytes: usize,
    /// Seconds the fake `ecp admin index` sleeps.
    index_delay_secs: u32,
}

struct Limits {
    max_repo_kb: u64,
    max_repos: usize,
    queue_limit: usize,
    concurrency: usize,
    run_timeout: Duration,
    queue_wait: Duration,
    max_output: usize,
    run_rate_per_min: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_repo_kb: 1024,
            max_repos: 4,
            queue_limit: 3,
            concurrency: 2,
            run_timeout: Duration::from_secs(10),
            queue_wait: Duration::from_secs(10),
            max_output: 1 << 20,
            run_rate_per_min: 1000,
        }
    }
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn harness(stubs: Stubs, limits: Limits) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let log = root.join("spawn.log");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log_s = log.display();

    let ecp = script(
        &bin,
        "ecp",
        &format!(
            r#"echo "cwd=$PWD argv=$*" >> "{log_s}"
case "$1" in
  admin) [ "$2" = index ] && sleep {delay}; exit 0 ;;
  summary) echo '{{"summary":{{"per_repo":[{{"metrics":{{"nodes":3}}}}]}}}}' ;;
  find)
    case "$*" in *--slow*) sleep 5 ;; esac
    case "$*" in *--big*) head -c 600000 /dev/zero | tr '\0' x ;; esac
    echo '{{"found":true}}' ;;
  *) echo '{{}}' ;;
esac"#,
            delay = stubs.index_delay_secs
        ),
    );
    let git = script(
        &bin,
        "git",
        &format!(
            r#"echo "cwd=$PWD argv=git $*" >> "{log_s}"
case "$1" in
  clone) for last; do :; done; mkdir -p "$last"; head -c {bytes} /dev/zero > "$last/blob"; exit 0 ;;
  rev-parse) echo abc1234 ;;
esac"#,
            bytes = stubs.checkout_bytes
        ),
    );
    let api = match (stubs.api_404, stubs.api_size_kb) {
        (true, _) => "printf '{}\\n404'".to_string(),
        (false, Some(kb)) => format!(
            r#"case "$*" in *repos/big/*) printf '{{"size":5000}}\n200' ;; *) printf '{{"size":{kb}}}\n200' ;; esac"#
        ),
        (false, None) => "printf '{}\\n403'".to_string(),
    };
    let curl = script(
        &bin,
        "curl",
        &format!("echo \"argv=curl $*\" >> \"{log_s}\"\n{api}"),
    );

    let repos_dir = root.join("repos");
    fs::create_dir_all(&repos_dir).unwrap();
    let store = RepoStore::new(StoreConfig {
        dir: repos_dir,
        programs: Programs {
            ecp: ecp.clone(),
            git,
            curl,
        },
        max_repo_kb: limits.max_repo_kb,
        max_repos: limits.max_repos,
        queue_limit: limits.queue_limit,
        clone_timeout: Duration::from_secs(10),
        index_timeout: Duration::from_secs(10),
        github_token: None,
    });
    let runner = Runner::new(
        ecp,
        limits.run_timeout,
        limits.queue_wait,
        limits.max_output,
        limits.concurrency,
    );
    let state = AppState::new(
        demo_tools(&Cli::command()),
        Arc::new(store),
        runner,
        RateLimiter::per_minute(limits.run_rate_per_min),
        RateLimiter::new(1000, Duration::from_secs(3600)),
        1,
    );
    Harness {
        app: router(Arc::new(state)),
        _dir: dir,
        root,
        log,
    }
}

async fn call_app(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    forwarded_for: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(xff) = forwarded_for {
        req = req.header("x-forwarded-for", xff);
    }
    let req = req
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, value)
}

fn run_body(tool: &str, repo: &str, args: Value) -> Value {
    json!({ "tool": tool, "repo": repo, "args": args })
}

impl Harness {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        call_app(self.app.clone(), method, uri, body, None).await
    }

    async fn add(&self, url: &str) -> (StatusCode, Value) {
        self.call("POST", "/api/repos", Some(json!({ "url": url })))
            .await
    }

    async fn run(&self, tool: &str, repo: &str, args: Value) -> (StatusCode, Value) {
        self.call("POST", "/api/run", Some(run_body(tool, repo, args)))
            .await
    }

    async fn run_from(&self, forwarded_for: &str, repo: &str) -> StatusCode {
        call_app(
            self.app.clone(),
            "POST",
            "/api/run",
            Some(run_body("find", repo, json!({ "pattern": "x" }))),
            Some(forwarded_for),
        )
        .await
        .0
    }

    /// Poll until `name` leaves the in-progress states; returns its entry.
    /// Every poll comes from a fresh address, so a test that pins the
    /// per-minute limit to 1 can still wait for a build.
    async fn settled(&self, name: &str) -> Value {
        for i in 0..200u32 {
            let poller = format!("10.9.{}.{}", i / 256, i % 256);
            let (_, list) =
                call_app(self.app.clone(), "GET", "/api/repos", None, Some(&poller)).await;
            if let Some(entry) = list["repos"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == name)
            {
                if matches!(entry["status"].as_str(), Some("ready" | "failed")) {
                    return entry.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{name} never settled");
    }

    async fn names(&self) -> Vec<String> {
        let (_, list) = self.call("GET", "/api/repos", None).await;
        list["repos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn checkout(&self, owner: &str, repo: &str) -> PathBuf {
        self.root.join("repos").join(format!("{owner}__{repo}"))
    }
}

fn ok_stubs() -> Stubs {
    Stubs {
        api_size_kb: Some(10),
        checkout_bytes: 100,
        ..Default::default()
    }
}

#[tokio::test]
async fn meta_lists_only_readonly_tools_and_the_limits() {
    let h = harness(ok_stubs(), Limits::default());
    let (status, meta) = h.call("GET", "/api/meta", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = meta["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["subcommand"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"find") && names.contains(&"impact"));
    assert!(!names.contains(&"rename") && !names.contains(&"uninstall"));
    assert_eq!(meta["limits"]["max_repo_mb"], 1);
    assert_eq!(meta["limits"]["max_repos"], 4);
}

#[tokio::test]
async fn add_then_run_spawns_ecp_inside_the_checkout_with_repo_injected() {
    let h = harness(ok_stubs(), Limits::default());
    let (status, body) = h.add("https://github.com/octo/cat").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["repo"]["status"], "queued");

    let entry = h.settled("octo/cat").await;
    assert_eq!(entry["status"], "ready", "{entry}");
    assert_eq!(entry["commit"], "abc1234");
    assert_eq!(
        entry["summary"]["summary"]["per_repo"][0]["metrics"]["nodes"],
        3
    );

    let checkout = h.checkout("octo", "cat");
    let log = h.log();
    assert!(
        log.contains(&format!(
            "argv=git clone --quiet --depth 1 --single-branch https://github.com/octo/cat.git {}",
            checkout.display()
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!("argv=admin index --repo {}", checkout.display())),
        "{log}"
    );

    let (status, out) = h
        .run(
            "find",
            "octo/cat",
            json!({ "pattern": "handler", "mode": "fuzzy", "all": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        out["argv"],
        json!([
            "ecp",
            "find",
            "--repo",
            checkout.display().to_string(),
            "handler",
            "--all",
            "--mode",
            "fuzzy"
        ])
    );
    assert_eq!(out["exit_code"], 0);
    assert_eq!(out["stdout"].as_str().unwrap().trim(), r#"{"found":true}"#);
    assert!(
        h.log()
            .contains(&format!("cwd={} argv=find", checkout.display())),
        "ecp must run inside the checkout"
    );
}

#[tokio::test]
async fn re_adding_a_ready_repo_returns_it_without_a_second_build() {
    let h = harness(ok_stubs(), Limits::default());
    h.add("octo/cat").await;
    h.settled("octo/cat").await;
    let clones_before = h.log().matches("argv=git clone").count();
    let (status, body) = h.add("https://github.com/octo/cat.git").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["repo"]["status"], "ready");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.log().matches("argv=git clone").count(), clones_before);
}

#[tokio::test]
async fn run_refuses_a_repo_that_is_still_indexing() {
    let h = harness(
        Stubs {
            index_delay_secs: 2,
            ..ok_stubs()
        },
        Limits::default(),
    );
    h.add("octo/slow").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (status, body) = h.run("find", "octo/slow", json!({ "pattern": "x" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("still being indexed"));
}

#[tokio::test]
async fn run_refuses_an_unknown_repo() {
    let h = harness(ok_stubs(), Limits::default());
    let (status, body) = h
        .run("find", "nobody/nothing", json!({ "pattern": "x" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn add_rejects_a_repo_the_api_reports_above_the_ceiling_before_cloning() {
    let h = harness(ok_stubs(), Limits::default());
    h.add("big/one").await;
    let entry = h.settled("big/one").await;
    assert_eq!(entry["status"], "failed");
    assert!(
        entry["error"]
            .as_str()
            .unwrap()
            .contains("GitHub reports 4 MB"),
        "{entry}"
    );
    assert!(
        !h.log().contains("argv=git clone"),
        "no bandwidth spent on a refused repo"
    );
    assert!(!h.checkout("big", "one").exists());
}

#[tokio::test]
async fn add_rejects_a_checkout_above_the_ceiling_when_the_api_was_unavailable() {
    let h = harness(
        Stubs {
            api_size_kb: None,
            checkout_bytes: 3 << 20,
            ..Default::default()
        },
        Limits::default(),
    );
    h.add("big/two").await;
    let entry = h.settled("big/two").await;
    assert_eq!(entry["status"], "failed");
    assert!(
        entry["error"]
            .as_str()
            .unwrap()
            .contains("checkout is 3 MB"),
        "{entry}"
    );
    assert!(
        !h.checkout("big", "two").exists(),
        "a refused checkout is removed"
    );
}

#[tokio::test]
async fn add_reports_a_missing_or_private_repo() {
    let h = harness(
        Stubs {
            api_404: true,
            ..ok_stubs()
        },
        Limits::default(),
    );
    h.add("no/such").await;
    let entry = h.settled("no/such").await;
    assert_eq!(entry["status"], "failed");
    assert!(
        entry["error"].as_str().unwrap().contains("not found"),
        "{entry}"
    );
}

#[tokio::test]
async fn add_rejects_anything_that_is_not_a_github_repo() {
    let h = harness(ok_stubs(), Limits::default());
    for url in ["https://gitlab.com/a/b", "../../etc", "onlyowner"] {
        let (status, body) = h.add(url).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}: {body}");
    }
}

#[tokio::test]
async fn failed_entries_never_fill_the_build_queue() {
    let h = harness(
        Stubs {
            api_404: true,
            ..ok_stubs()
        },
        Limits {
            queue_limit: 1,
            ..Limits::default()
        },
    );
    h.add("no/one").await;
    assert_eq!(h.settled("no/one").await["status"], "failed");
    let (status, body) = h.add("no/two").await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a failed entry is not pending: {body}"
    );
    assert_eq!(h.settled("no/two").await["status"], "failed");
    h.add("no/three").await;
    h.settled("no/three").await;
    let names = h.names().await;
    assert!(
        names.len() <= 2 && names.contains(&"no/three".to_string()),
        "old failures are pruned, the newest stays: {names:?}"
    );
}

#[tokio::test]
async fn adding_past_max_repos_evicts_the_least_recently_used_one() {
    let h = harness(
        ok_stubs(),
        Limits {
            max_repos: 1,
            ..Limits::default()
        },
    );
    h.add("octo/first").await;
    h.settled("octo/first").await;
    h.add("octo/second").await;
    let second = h.settled("octo/second").await;
    assert_eq!(second["status"], "ready");

    assert_eq!(h.names().await, ["octo/second"]);
    let first = h.checkout("octo", "first");
    assert!(
        h.log()
            .contains(&format!("argv=admin drop --repo {}", first.display())),
        "{}",
        h.log()
    );
    assert!(!first.exists());
}

#[tokio::test]
async fn a_refused_add_evicts_nothing() {
    let h = harness(
        ok_stubs(),
        Limits {
            max_repos: 1,
            ..Limits::default()
        },
    );
    h.add("octo/first").await;
    h.settled("octo/first").await;
    h.add("big/one").await;
    assert_eq!(h.settled("big/one").await["status"], "failed");
    let names = h.names().await;
    assert!(names.contains(&"octo/first".to_string()), "{names:?}");
    assert!(h.checkout("octo", "first").exists());
    assert!(!h.log().contains("argv=admin drop"), "{}", h.log());
}

#[tokio::test]
async fn eviction_skips_a_repo_with_a_run_in_flight() {
    let h = harness(
        ok_stubs(),
        Limits {
            max_repos: 1,
            run_timeout: Duration::from_secs(1),
            ..Limits::default()
        },
    );
    h.add("octo/first").await;
    h.settled("octo/first").await;

    let app = h.app.clone();
    let in_flight = tokio::spawn(async move {
        call_app(
            app,
            "POST",
            "/api/run",
            Some(run_body(
                "find",
                "octo/first",
                json!({ "pattern": "x", "slow": true }),
            )),
            None,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    h.add("octo/second").await;
    assert_eq!(h.settled("octo/second").await["status"], "ready");
    let names = h.names().await;
    assert!(
        names.contains(&"octo/first".to_string()),
        "a repo being read survives eviction: {names:?}"
    );
    assert!(h.checkout("octo", "first").exists());

    let (status, _) = in_flight.await.unwrap();
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);

    h.add("octo/third").await;
    h.settled("octo/third").await;
    assert!(
        !h.names().await.contains(&"octo/first".to_string()),
        "once the run is over the repo is an ordinary victim"
    );
}

#[tokio::test]
async fn run_rejects_server_owned_flags_however_they_are_spelled() {
    let h = harness(ok_stubs(), Limits::default());
    h.add("octo/cat").await;
    h.settled("octo/cat").await;
    for (args, flag) in [
        (json!({ "pattern": "x", "graph": "/etc/passwd" }), "graph"),
        (json!({ "pattern": "x", "Graph": "/etc/passwd" }), "graph"),
        (json!({ "pattern": "--graph=/etc/shadow" }), "graph"),
        (json!({ "pattern": "x", "repo": "/" }), "repo"),
        (json!({ "pattern": "x", "batch": true }), "batch"),
    ] {
        let (status, body) = h.run("find", "octo/cat", args.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{args}: {body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains(&format!("`{flag}`")),
            "{args}: {body}"
        );
    }
    assert!(
        !h.log().contains("argv=find"),
        "none of those reached ecp: {}",
        h.log()
    );
}

#[tokio::test]
async fn run_kills_ecp_at_the_timeout_and_says_so() {
    let h = harness(
        ok_stubs(),
        Limits {
            run_timeout: Duration::from_millis(300),
            ..Limits::default()
        },
    );
    h.add("octo/cat").await;
    h.settled("octo/cat").await;
    let (status, body) = h
        .run("find", "octo/cat", json!({ "pattern": "x", "slow": true }))
        .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert_eq!(body["timed_out"], true);
    assert!(
        body["elapsed_ms"].as_u64().unwrap() < 2000,
        "the child was killed, not awaited"
    );
}

#[tokio::test]
async fn run_answers_503_when_no_slot_frees_up_within_the_queue_wait() {
    let h = harness(
        ok_stubs(),
        Limits {
            concurrency: 1,
            queue_wait: Duration::from_millis(200),
            run_timeout: Duration::from_secs(2),
            ..Limits::default()
        },
    );
    h.add("octo/cat").await;
    h.settled("octo/cat").await;
    let app = h.app.clone();
    let slow = tokio::spawn(async move {
        call_app(
            app,
            "POST",
            "/api/run",
            Some(run_body(
                "find",
                "octo/cat",
                json!({ "pattern": "x", "slow": true }),
            )),
            None,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, body) = h.run("find", "octo/cat", json!({ "pattern": "x" })).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body["error"].as_str().unwrap().starts_with("busy"));
    slow.await.unwrap();
}

#[tokio::test]
async fn run_truncates_stdout_above_the_cap() {
    let h = harness(
        ok_stubs(),
        Limits {
            max_output: 1000,
            ..Limits::default()
        },
    );
    h.add("octo/cat").await;
    h.settled("octo/cat").await;
    let (status, body) = h
        .run("find", "octo/cat", json!({ "pattern": "x", "big": true }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["truncated"], true);
    assert_eq!(body["stdout"].as_str().unwrap().len(), 1000);
}

#[tokio::test]
async fn rate_limit_buckets_follow_the_proxy_appended_hop() {
    let h = harness(
        ok_stubs(),
        Limits {
            run_rate_per_min: 1,
            ..Limits::default()
        },
    );
    h.add("octo/cat").await;
    h.settled("octo/cat").await;
    // The proxy appended 203.0.113.5; the client chose the leftmost value.
    assert_eq!(
        h.run_from("1.1.1.1, 203.0.113.5", "octo/cat").await,
        StatusCode::OK
    );
    assert_eq!(
        h.run_from("2.2.2.2, 203.0.113.5", "octo/cat").await,
        StatusCode::TOO_MANY_REQUESTS,
        "changing the client-supplied hop must not open a fresh bucket"
    );
    assert_eq!(
        h.run_from("2.2.2.2, 198.51.100.7", "octo/cat").await,
        StatusCode::OK,
        "a different proxy-observed address is a different bucket"
    );
}

#[tokio::test]
async fn the_repo_list_shares_the_run_rate_limit() {
    let h = harness(
        ok_stubs(),
        Limits {
            run_rate_per_min: 1,
            ..Limits::default()
        },
    );
    assert_eq!(h.call("GET", "/api/repos", None).await.0, StatusCode::OK);
    assert_eq!(
        h.call("GET", "/api/repos", None).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn the_ui_is_served_from_the_binary() {
    let h = harness(ok_stubs(), Limits::default());
    let (status, body) = h.call("GET", "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body
        .as_str()
        .unwrap()
        .contains("<title>ecp live demo</title>"));
    let (status, _) = h.call("GET", "/app.js", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.call("GET", "/nope.txt", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
