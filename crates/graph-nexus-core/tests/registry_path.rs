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

