use cgn_cli::repo_selector::{self, Atom, Selector};
use cgn_core::registry::{GroupEntry, RegistryFile, RepoAlias};
use std::collections::BTreeMap;
use std::process::Command;

fn make_repo_alias(dir: &str, common_dir: &str, alias: &str) -> RepoAlias {
    RepoAlias {
        dir_name: dir.into(),
        common_dir: common_dir.into(),
        remote_url: None,
        aliases: vec![alias.into()],
        last_touched: "2026-05-17T10:00:00Z".into(),
        groups: vec![],
    }
}

#[test]
fn grammar_parse_cwd_default() {
    let sel = repo_selector::parse("").unwrap();
    assert_eq!(sel.0, vec![Atom::Cwd]);
}

#[test]
fn grammar_dot_is_path_cwd() {
    let sel = repo_selector::parse(".").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Path(".".into())]));
}

#[test]
fn grammar_absolute_path() {
    let sel = repo_selector::parse("/abs/path").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Path("/abs/path".into())]));
}

#[test]
fn grammar_registry_name() {
    let sel = repo_selector::parse("backend-svc").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Name("backend-svc".into())]));
}

#[test]
fn grammar_at_group() {
    let sel = repo_selector::parse("@backend").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Group("backend".into())]));
}

#[test]
fn grammar_at_all() {
    let sel = repo_selector::parse("@all").unwrap();
    assert_eq!(sel, Selector(vec![Atom::All]));
}

#[test]
fn grammar_csv_mix() {
    let sel = repo_selector::parse("alpha,@beta,/abs/path").unwrap();
    assert_eq!(
        sel,
        Selector(vec![
            Atom::Name("alpha".into()),
            Atom::Group("beta".into()),
            Atom::Path("/abs/path".into()),
        ])
    );
}

#[test]
fn grammar_parse_name_path_group_all() {
    let sel = repo_selector::parse("foo,./bar,@grp,@all").unwrap();
    assert_eq!(sel.0.len(), 4);
    assert!(matches!(sel.0[0], Atom::Name(ref n) if n == "foo"));
    assert!(matches!(sel.0[1], Atom::Path(_)));
    assert!(matches!(sel.0[2], Atom::Group(ref g) if g == "grp"));
    assert!(matches!(sel.0[3], Atom::All));
}

#[test]
fn grammar_at_all_alone_no_csv() {
    let sel = repo_selector::parse("@all,alpha").unwrap();
    assert_eq!(sel.0.len(), 2);
}

#[test]
fn grammar_rejects_empty_atom() {
    assert!(repo_selector::parse("a,,b").is_err());
}

#[test]
fn grammar_rejects_at_without_name() {
    assert!(repo_selector::parse("@").is_err());
}

// ── Resolver tests ───────────────────────────────────────────────────────────

use cgn_cli::repo_selector::ResolveError;

#[test]
fn resolve_by_name_matches_user_alias() {
    let mut repos = BTreeMap::new();
    repos.insert(
        "myrepo__abcd1234".into(),
        make_repo_alias("myrepo__abcd1234", "/work/myrepo/.git", "myrepo"),
    );
    let reg = RegistryFile {
        version: 2,
        repos,
        groups: vec![],
    };

    let sel = Selector(vec![Atom::Name("myrepo".into())]);
    let out = repo_selector::resolve(&sel, &reg, "/").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].dir_name, "myrepo__abcd1234");
}

#[test]
fn resolve_by_name_falls_back_to_dir_name() {
    let mut repos = BTreeMap::new();
    repos.insert(
        "myrepo__abcd1234".into(),
        make_repo_alias("myrepo__abcd1234", "/work/myrepo/.git", "myrepo"),
    );
    let reg = RegistryFile {
        version: 2,
        repos,
        groups: vec![],
    };

    let sel = Selector(vec![Atom::Name("myrepo__abcd1234".into())]);
    let out = repo_selector::resolve(&sel, &reg, "/").unwrap();
    assert_eq!(out.len(), 1);
}

#[test]
fn resolve_all_returns_dedup_sorted_by_dir_name() {
    let mut repos = BTreeMap::new();
    repos.insert("a__1".into(), make_repo_alias("a__1", "/a/.git", "a"));
    repos.insert("b__2".into(), make_repo_alias("b__2", "/b/.git", "b"));
    let reg = RegistryFile {
        version: 2,
        repos,
        groups: vec![],
    };

    let sel = Selector(vec![Atom::All]);
    let out = repo_selector::resolve(&sel, &reg, "/").unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].dir_name, "a__1");
    assert_eq!(out[1].dir_name, "b__2");
}

#[test]
fn resolve_group_lookup() {
    let mut repos = BTreeMap::new();
    repos.insert("a__1".into(), make_repo_alias("a__1", "/a/.git", "a"));
    repos.insert("b__2".into(), make_repo_alias("b__2", "/b/.git", "b"));
    let reg = RegistryFile {
        version: 2,
        repos,
        groups: vec![GroupEntry {
            name: "team".into(),
            members: vec!["a__1".into()],
        }],
    };

    let sel = Selector(vec![Atom::Group("team".into())]);
    let out = repo_selector::resolve(&sel, &reg, "/").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].dir_name, "a__1");
}

#[test]
fn resolve_unknown_name_errors() {
    let reg = RegistryFile::empty();
    let sel = Selector(vec![Atom::Name("nope".into())]);
    let err = repo_selector::resolve(&sel, &reg, "/").unwrap_err();
    assert!(matches!(err, ResolveError::NotFound(_)));
}

#[test]
fn find_by_path_matches_via_common_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    std::fs::create_dir(&primary).unwrap();
    Command::new("git")
        .arg("-C")
        .arg(&primary)
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();

    let common_dir = std::fs::canonicalize(primary.join(".git")).unwrap();
    let mut repos = BTreeMap::new();
    repos.insert(
        "primary__xxxx".into(),
        RepoAlias {
            dir_name: "primary__xxxx".into(),
            common_dir: common_dir.to_string_lossy().into(),
            remote_url: None,
            aliases: vec!["primary".into()],
            last_touched: "2026-05-17T10:00:00Z".into(),
            groups: vec![],
        },
    );
    let reg = RegistryFile {
        version: 2,
        repos,
        groups: vec![],
    };

    let resolved = repo_selector::find_by_path(&reg, primary.to_string_lossy().as_ref()).unwrap();
    assert_eq!(resolved.dir_name, "primary__xxxx");
}

#[test]
fn find_by_path_returns_none_outside_any_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = RegistryFile::empty();
    assert!(repo_selector::find_by_path(&reg, tmp.path().to_str().unwrap()).is_none());
}
