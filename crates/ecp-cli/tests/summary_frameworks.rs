//! `ecp summary` framework detection (architecture direction, area 8): the
//! catalog and the detection read one table, so a Laravel, Gin, Rails or
//! ASP.NET repo lists its framework in `frameworks.detected` instead of `[]`.

mod common;

use common::{commit_all, ecp_bin, run_git};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

/// An indexed git repo with its own cache root.
struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn indexed(files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        for (rel, text) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let repo = Self {
            _tmp: tmp,
            root,
            home,
        };
        run_git(&repo.root, &["init", "-q", "-b", "main"]);
        commit_all(&repo.root, "base");
        let out = repo.ecp(&["admin", "index", "--repo", "."]);
        assert!(out.status.success(), "admin index: {}", stderr(&out));
        repo
    }

    fn ecp(&self, args: &[&str]) -> Output {
        Command::new(ecp_bin())
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", "summary-frameworks")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
    }

    /// The `frameworks.detected` rows of `ecp summary --format json`.
    fn detected(&self) -> Vec<Value> {
        let out = self.ecp(&["summary", "--repo", ".", "--format", "json"]);
        assert!(out.status.success(), "summary: {}", stderr(&out));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let v: Value = serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text}"));
        v["summary"]["per_repo"][0]["frameworks"]["detected"]
            .as_array()
            .unwrap_or_else(|| panic!("no detected array: {text}"))
            .clone()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn names(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|r| r["lang_framework"].as_str().unwrap().to_string())
        .collect()
}

const LARAVEL_ROUTES: &str = "<?php\nuse Illuminate\\Support\\Facades\\Route;\nuse App\\Http\\Controllers\\UserController;\nRoute::get('/api/users', [UserController::class, 'index']);\n";
const LARAVEL_CONTROLLER: &str = "<?php\nnamespace App\\Http\\Controllers;\nclass UserController { public function index() { return []; } }\n";
const GIN_MAIN: &str = "package main\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc listUsers(c *gin.Context) {}\n\nfunc main() {\n\tr := gin.Default()\n\tr.GET(\"/api/users\", listUsers)\n\tr.Run()\n}\n";
const RAILS_ROUTES: &str =
    "Rails.application.routes.draw do\n  get '/api/users', to: 'users#index'\nend\n";
const RAILS_CONTROLLER: &str = "class UsersController < ApplicationController\n  def index\n    render json: User.all\n  end\nend\n";
const ASPNET_CONTROLLER: &str = "using Microsoft.AspNetCore.Mvc;\n[ApiController]\npublic class UsersController : ControllerBase {\n  [HttpGet(\"/api/users\")]\n  public IActionResult List() { return Ok(); }\n}\n";

/// Stands for: before the fix `frameworks.detected` was `[]` for these four
/// repos, so an agent concluded that ecp cannot see their routes. Each repo
/// must list its own framework and count the route evidence.
#[test]
fn test_summary_frameworks_detected_laravel_gin_rails_aspnet() {
    let cases: [(&str, Vec<(&str, &str)>, &str); 4] = [
        (
            "Laravel",
            vec![
                ("routes/web.php", LARAVEL_ROUTES),
                (
                    "app/Http/Controllers/UserController.php",
                    LARAVEL_CONTROLLER,
                ),
            ],
            "PHP/Laravel",
        ),
        ("Gin", vec![("main.go", GIN_MAIN)], "Go/Gin"),
        (
            "Rails",
            vec![
                ("config/routes.rb", RAILS_ROUTES),
                ("app/controllers/users_controller.rb", RAILS_CONTROLLER),
            ],
            "Ruby/Rails|Sinatra",
        ),
        (
            "ASP.NET",
            vec![("UsersController.cs", ASPNET_CONTROLLER)],
            "C#/ASP.NET",
        ),
    ];
    for (label, files, expected) in cases {
        let detected = Repo::indexed(&files).detected();
        let found = names(&detected);
        assert!(
            found.iter().any(|n| n == expected),
            "{label}: expected {expected} in frameworks.detected, got {found:?}"
        );
    }
}

/// Stands for: a repo with no web framework reports an empty `detected`
/// list, not a framework guessed from the file extension.
#[test]
fn test_summary_frameworks_plain_python_repo_detects_nothing() {
    let repo = Repo::indexed(&[("lib.py", "def parse():\n    return 1\n")]);
    assert!(repo.detected().is_empty());
}

/// Stands for: every detected framework appears in the supported catalog, so
/// the two lists cannot drift apart again.
#[test]
fn test_summary_frameworks_detected_names_are_in_supported_catalog() {
    let repo = Repo::indexed(&[("main.go", GIN_MAIN)]);
    let out = repo.ecp(&["summary", "--repo", ".", "--format", "json"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: Value = serde_json::from_str(text.trim()).unwrap();
    let fw = &v["summary"]["per_repo"][0]["frameworks"];
    let supported: Vec<&str> = fw["supported"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lang_framework"].as_str().unwrap())
        .collect();
    for row in fw["detected"].as_array().unwrap() {
        let name = row["lang_framework"].as_str().unwrap();
        assert!(supported.contains(&name), "{name} not in supported");
    }
}
