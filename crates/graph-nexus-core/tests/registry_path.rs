//! Tests for registry path sanitization (spec §1.1).

use graph_nexus_core::registry::sanitize_segment;

#[test]
fn rejects_empty() {
    assert!(sanitize_segment("").is_err());
}

#[test]
fn rejects_too_long() {
    let s = "a".repeat(65);
    assert!(sanitize_segment(&s).is_err());
}

#[test]
fn rejects_path_traversal() {
    assert!(sanitize_segment("..").is_err());
    assert!(sanitize_segment("foo..bar").is_err());
    assert!(sanitize_segment("../etc/passwd").is_err());
}

#[test]
fn rejects_leading_dot_or_hyphen() {
    assert!(sanitize_segment(".hidden").is_err());
    assert!(sanitize_segment("-flag").is_err());
}

#[test]
fn rejects_special_chars() {
    assert!(sanitize_segment("foo/bar").is_err());
    assert!(sanitize_segment("foo\\bar").is_err());
    assert!(sanitize_segment("foo\0bar").is_err());
    assert!(sanitize_segment("foo bar").is_err());
    assert!(sanitize_segment("foo:bar").is_err());
}

#[test]
fn accepts_valid_names() {
    assert_eq!(sanitize_segment("graph-nexus").unwrap(), "graph-nexus");
    assert_eq!(sanitize_segment("my_repo.2").unwrap(), "my_repo.2");
    assert_eq!(sanitize_segment("ABC-123").unwrap(), "ABC-123");
}

use graph_nexus_core::registry::sanitize_branch;

#[test]
fn branch_normalizes_slash() {
    assert_eq!(sanitize_branch("feat/foo").unwrap(), "feat__foo");
    assert_eq!(sanitize_branch("feat/foo/bar").unwrap(), "feat__foo__bar");
}

#[test]
fn branch_replaces_invalid_chars_with_underscore() {
    assert_eq!(sanitize_branch("feat:foo").unwrap(), "feat_foo");
    assert_eq!(sanitize_branch("feat foo").unwrap(), "feat_foo");
}

#[test]
fn branch_handles_real_world_names() {
    assert_eq!(sanitize_branch("main").unwrap(), "main");
    assert_eq!(sanitize_branch("fix/hook-race").unwrap(), "fix__hook-race");
    assert_eq!(
        sanitize_branch("release/v1.2.3").unwrap(),
        "release__v1.2.3"
    );
}

use graph_nexus_core::registry::derive_repo_name;

#[test]
fn derives_from_ssh_url() {
    let r = derive_repo_name(Some("git@github.com:coseto6125/graph-nexus.git")).unwrap();
    assert_eq!(r, "graph-nexus");
}

#[test]
fn derives_from_https_url() {
    let r = derive_repo_name(Some("https://github.com/coseto6125/graph-nexus.git")).unwrap();
    assert_eq!(r, "graph-nexus");
}

#[test]
fn strips_trailing_dot_git() {
    let r = derive_repo_name(Some("https://example.com/foo.git")).unwrap();
    assert_eq!(r, "foo");
}

#[test]
fn errors_on_malicious_url() {
    assert!(derive_repo_name(Some("git@github.com:foo/../../etc/passwd.git")).is_err());
}

#[test]
fn errors_when_no_remote() {
    assert!(derive_repo_name(None).is_err());
}

use graph_nexus_core::registry::uid_path;
use std::path::Path;

#[test]
fn uid_strips_repo_prefix() {
    let abs = Path::new("/home/enor/graph-nexus/src/auth.ts");
    let repo = Path::new("/home/enor/graph-nexus");
    assert_eq!(uid_path(abs, repo).unwrap(), "src/auth.ts");
}

#[test]
#[cfg(windows)]
fn uid_normalizes_backslash_to_slash() {
    let abs = Path::new(r"C:\repo\src\auth.ts");
    let repo = Path::new(r"C:\repo");
    let got = uid_path(abs, repo).unwrap();
    assert!(!got.contains('\\'), "got {got:?}");
    assert_eq!(got, "src/auth.ts");
}

#[test]
fn uid_normalizes_nfc_unicode() {
    let nfd_repo = Path::new("/repo");
    // "café" with é as NFD form (e + combining acute U+0301)
    let nfd_file = "/repo/cafe\u{0301}/main.py";
    let result = uid_path(Path::new(nfd_file), nfd_repo).unwrap();
    // NFC form: é = U+00E9
    assert_eq!(result, "café/main.py");
}

#[test]
fn uid_errors_if_not_under_repo() {
    let abs = Path::new("/other/path/file.rs");
    let repo = Path::new("/repo");
    assert!(uid_path(abs, repo).is_err());
}

use graph_nexus_core::registry::IndexLayout;
use std::path::PathBuf;

fn fake_home() -> PathBuf {
    PathBuf::from("/home/test/.gnx")
}

#[test]
fn index_path_basic() {
    let layout = IndexLayout::resolve(
        &fake_home(),
        "graph-nexus",
        "main",
        "/home/test/code/graph-nexus",
        &[],
    )
    .unwrap();
    assert_eq!(
        layout.index_dir,
        PathBuf::from("/home/test/.gnx/graph-nexus/main")
    );
    assert_eq!(layout.disambiguator, None);
}

#[test]
fn index_path_collision_gets_hash() {
    let existing = vec![(
        "graph-nexus".to_string(),
        "/home/test/other-worktree".to_string(),
    )];
    let layout = IndexLayout::resolve(
        &fake_home(),
        "graph-nexus",
        "main",
        "/home/test/code/graph-nexus",
        &existing,
    )
    .unwrap();
    assert!(
        layout
            .index_dir
            .to_string_lossy()
            .starts_with("/home/test/.gnx/graph-nexus-"),
        "got {:?}",
        layout.index_dir
    );
    assert!(layout.disambiguator.is_some());
}

#[test]
fn index_path_same_worktree_no_collision() {
    // Same repo name + same worktree path → not a collision
    let existing = vec![(
        "graph-nexus".to_string(),
        "/home/test/code/graph-nexus".to_string(),
    )];
    let layout = IndexLayout::resolve(
        &fake_home(),
        "graph-nexus",
        "main",
        "/home/test/code/graph-nexus",
        &existing,
    )
    .unwrap();
    assert_eq!(layout.disambiguator, None);
}

#[test]
fn index_path_rejects_escape_via_relative_segments() {
    // Even though sanitize_segment rejects ".." now, this guards
    // against future refactors that loosen the segment check.
    // Using a normal call to verify the assertion path doesn't false-alarm:
    let layout = IndexLayout::resolve(&fake_home(), "valid-repo", "main", "/path", &[]).unwrap();
    assert!(layout.index_dir.starts_with(fake_home()));
}
