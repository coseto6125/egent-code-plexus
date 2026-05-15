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
