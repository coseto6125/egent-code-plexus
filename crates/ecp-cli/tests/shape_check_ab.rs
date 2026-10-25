//! A/B regression test for `ecp shape_check` against upstream-equivalent fixtures.
//!
//! Upstream gitnexus' fixtures use Next.js App Router (`app/api/.../route.ts`
//! filename convention) which egent-code-plexus does not currently parse as
//! routes — it detects Express-style `app.get(path, handler)` calls. We
//! mirror upstream's logical shape (3 consumer/route pairs + 1 synthetic
//! drifter) in Express form so the same drift semantics get exercised
//! end-to-end:
//! - listUsers → GET /api/users:     consumer reads {data, error}     vs handler emits {data, total}       → DRIFT on `error`
//! - listSearch → /api/search:      consumer reads {courses, articles} vs route emits {courses, articles}  → NO drift
//! - exportGdpr → /api/gdpr/export: consumer reads {url}             vs route emits {url}                  → NO drift
//! - drifter → /api/users:           consumer reads {nonexistent_field} → DRIFT
//!
//! The Express rewrite splits upstream's `/api/users` handler in two: only
//! `createUser` (POST) returns `{ error, details }`, and `listUsers` (GET),
//! the route `fetch('/api/users')` reaches, returns `{ data, total }`. A
//! route's shape is the shape of its own handler (flow F8), so UserList's
//! read of `error` is a real drift. Before P8e the whole file's union of
//! keys hid it.
//!
//! The synthetic drifter ensures the test discriminates — if the entire
//! pipeline silently no-op'd, the clean cases would pass vacuously.

use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn init_repo(repo: &Path) {
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/shape-check-ab-test.git",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
}

fn write(repo: &Path, rel: &str, content: &str) {
    let p = repo.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn git_commit(repo: &Path) {
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "fixtures",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
}

fn run(args: &[&str], repo: &Path, home: &Path) -> std::process::Output {
    Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("ecp spawn")
}

/// End-to-end: write upstream fixtures + a synthetic drifter, run
/// `ecp analyze` + `ecp shape_check`, assert the drift report matches
/// upstream's expected behavior.
#[test]
fn ab_upstream_fixtures_two_clean_two_drift() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    init_repo(repo.path());

    // ── Express route handlers (logical mirror of upstream's
    //    test/fixtures/lang-resolution/shape-check-integration/app/api/
    //    handlers, rewritten as Express to match egent-code-plexus's
    //    JS/TS route extractor surface) ──
    write(
        repo.path(),
        "server/routes.ts",
        r#"import express from 'express';
const app = express();

app.get('/api/users', function listUsers(req, res) {
  const users = getUsers();
  return res.json({ data: users, total: users.length });
});

app.post('/api/users', function createUser(req, res) {
  if (!req.body.name) {
    return res.status(400).json({ error: 'Name required', details: 'missing field' });
  }
  return res.json({ data: req.body, success: true });
});

app.get('/api/search', function listSearch(req, res) {
  const courses = getCourses();
  const articles = getArticles();
  return res.json({ courses: courses, articles: articles });
});

app.post('/api/gdpr/export', function exportGdpr(req, res) {
  const archive = buildExport();
  return res.json({ url: archive.url });
});
"#,
    );

    // ── Consumer files (3 from upstream + 1 synthetic drifter) ──
    write(
        repo.path(),
        "components/UserList.tsx",
        r#"export function UserList() {
  const res = fetch('/api/users').then(r => r.json());
  const items = res.data;
  const err = res.error;
  return null;
}
"#,
    );
    write(
        repo.path(),
        "components/SearchBar.tsx",
        r#"export function SearchBar() {
  const data = fetch('/api/search').then(r => r.json());
  console.log(data.courses);
  console.log(data.articles);
  return null;
}
"#,
    );
    write(
        repo.path(),
        "components/GdprExport.tsx",
        r#"export function GdprExport() {
  const data = fetch('/api/gdpr/export', { method: 'POST' }).then(r => r.json());
  const link = document.createElement('a');
  link.href = data.url;
  return null;
}
"#,
    );
    // Synthetic: reads a key the route does NOT emit.
    write(
        repo.path(),
        "components/Drifter.tsx",
        r#"export function Drifter() {
  const data = fetch('/api/users').then(r => r.json());
  console.log(data.nonexistent_field);
  return null;
}
"#,
    );

    git_commit(repo.path());

    // ── Analyze + shape_check ──
    let analyze = run(&["admin", "index", "--repo", "."], repo.path(), home.path());
    assert!(
        analyze.status.success(),
        "admin index failed: stdout={} stderr={}",
        String::from_utf8_lossy(&analyze.stdout),
        String::from_utf8_lossy(&analyze.stderr),
    );

    let shape = run(
        &["shape-check", "--repo", ".", "--format", "json"],
        repo.path(),
        home.path(),
    );
    assert!(
        shape.status.success(),
        "shape_check failed: stdout={} stderr={}",
        String::from_utf8_lossy(&shape.stdout),
        String::from_utf8_lossy(&shape.stderr),
    );

    let stdout = String::from_utf8(shape.stdout).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("non-JSON output: {e}\n{stdout}"));

    let total_fetches = json["total_fetches"].as_u64().unwrap_or(0);
    let drift_count = json["drift_count"].as_u64().unwrap_or(0);
    let drift = json["drift"].as_array().cloned().unwrap_or_default();

    // 4 consumers fetching paths that map to 4 routes (one of which —
    // /api/users — has both GET and POST handlers, so the URL → Route
    // match is many-to-one for that path). Expected ≥4 Fetches edges;
    // when /api/users matches both methods, that consumer contributes 2
    // edges, so the realistic floor is 5.
    assert!(
        total_fetches >= 4,
        "expected ≥4 Fetches edges; got {total_fetches}\n{stdout}",
    );

    // Drift must be ≥2: Drifter.tsx (nonexistent_field) and UserList.tsx
    // (error). Allow more because /api/users has GET+POST handlers — a
    // consumer contributes one drift row per handler its fetch matches.
    assert!(
        drift_count >= 2,
        "expected ≥2 drift entries (Drifter and UserList); got {drift_count}\n{stdout}",
    );

    // Every drift row comes from Drifter.tsx (`nonexistent_field`) or from
    // UserList.tsx (`error`, which only the POST handler returns). The clean
    // consumers (SearchBar / GdprExport) must NOT appear in the drift report.
    let mut drifting: Vec<&str> = Vec::new();
    for entry in &drift {
        let consumer_file = entry["consumer_file"].as_str().unwrap_or_default();
        let drift_keys = entry["drift_keys"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        match consumer_file {
            "components/Drifter.tsx" => assert!(
                drift_keys.contains(&"nonexistent_field"),
                "drift_keys should contain 'nonexistent_field'; got {drift_keys:?}\n{stdout}",
            ),
            "components/UserList.tsx" => assert_eq!(
                drift_keys,
                ["error"],
                "UserList reads `error`, which GET /api/users does not return\n{stdout}",
            ),
            other => panic!("clean consumer drift entry leaked through: {other:?}\nfull: {stdout}"),
        }
        if !drifting.contains(&consumer_file) {
            drifting.push(consumer_file);
        }
    }
    drifting.sort_unstable();
    assert_eq!(
        drifting,
        ["components/Drifter.tsx", "components/UserList.tsx"],
        "both drifting consumers are reported\n{stdout}",
    );
}
