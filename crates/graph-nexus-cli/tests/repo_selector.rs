use graph_nexus_cli::repo_selector::{parse, Atom, Selector};

#[test]
fn empty_selector_is_cwd() {
    let sel = parse("").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Cwd]));
}

#[test]
fn dot_is_path_cwd() {
    let sel = parse(".").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Path(".".into())]));
}

#[test]
fn absolute_path() {
    let sel = parse("/abs/path").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Path("/abs/path".into())]));
}

#[test]
fn registry_name() {
    let sel = parse("backend-svc").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Name("backend-svc".into())]));
}

#[test]
fn at_group() {
    let sel = parse("@backend").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Group("backend".into())]));
}

#[test]
fn at_all() {
    let sel = parse("@all").unwrap();
    assert_eq!(sel, Selector(vec![Atom::All]));
}

#[test]
fn csv_mix() {
    let sel = parse("alpha,@beta,/abs/path").unwrap();
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
fn at_all_alone_no_csv() {
    let sel = parse("@all,alpha").unwrap();
    assert_eq!(sel.0.len(), 2);
}

#[test]
fn rejects_empty_atom() {
    assert!(parse("a,,b").is_err());
}

#[test]
fn rejects_at_without_name() {
    assert!(parse("@").is_err());
}

// ── Resolver tests (Task 0.4) ────────────────────────────────────────────────

use graph_nexus_cli::repo_selector::{resolve, ResolveError};
use graph_nexus_core::registry::{BranchEntry, GroupEntry, RegistryFile, RepoEntry};

fn make_registry() -> RegistryFile {
    RegistryFile {
        version: 1,
        repos: vec![
            RepoEntry {
                name: "alpha".into(),
                remote_url: "x".into(),
                worktree_path: "/tmp/alpha".into(),
                index_dir_root: "/tmp/idx/alpha".into(),
                branches: vec![BranchEntry {
                    name: "main".into(),
                    index_dir: "/tmp/idx/alpha/main".into(),
                    indexed_at: "2026-05-15".into(),
                    node_count: 100,
                    delta_size: 0,
                    embedding_status: "none".into(),
                }],
                groups: vec!["backend".into()],
            },
            RepoEntry {
                name: "beta".into(),
                remote_url: "y".into(),
                worktree_path: "/tmp/beta".into(),
                index_dir_root: "/tmp/idx/beta".into(),
                branches: vec![],
                groups: vec!["backend".into(), "auth".into()],
            },
        ],
        groups: vec![
            GroupEntry {
                name: "backend".into(),
                members: vec!["alpha".into(), "beta".into()],
            },
            GroupEntry {
                name: "auth".into(),
                members: vec!["beta".into()],
            },
        ],
    }
}

#[test]
fn resolve_name_finds_repo() {
    let sel = parse("alpha").unwrap();
    let resolved = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "alpha");
}

#[test]
fn resolve_at_group_expands_members() {
    let sel = parse("@backend").unwrap();
    let resolved = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap();
    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().any(|r| r.name == "alpha"));
    assert!(resolved.iter().any(|r| r.name == "beta"));
}

#[test]
fn resolve_at_all_returns_all() {
    let sel = parse("@all").unwrap();
    let resolved = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap();
    assert_eq!(resolved.len(), 2);
}

#[test]
fn resolve_multi_group_union_dedups() {
    // backend = {alpha, beta}, auth = {beta} → union = {alpha, beta}
    let sel = parse("@backend,@auth").unwrap();
    let resolved = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap();
    assert_eq!(resolved.len(), 2);
}

#[test]
fn resolve_unknown_name_errors() {
    let sel = parse("nonexistent").unwrap();
    let err = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap_err();
    assert!(matches!(err, ResolveError::NotFound(ref s) if s == "nonexistent"));
}

#[test]
fn resolve_unknown_group_errors() {
    let sel = parse("@ghost").unwrap();
    let err = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap_err();
    assert!(matches!(err, ResolveError::GroupNotFound(ref s) if s == "ghost"));
}
