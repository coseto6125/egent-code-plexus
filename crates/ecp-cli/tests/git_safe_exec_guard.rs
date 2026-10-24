//! Every git invocation in ecp-cli goes through `git::safe_exec::git()`, which
//! neutralises the repo-supplied executables git would otherwise run
//! (`core.fsmonitor`, hooks, credential helpers). A bare
//! `Command::new("git")` in non-test code re-opens that door, and a hostile
//! repository can trip it from the first Edit hook. This test scans the
//! source tree so the next new module cannot regress it silently.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Source text before the inline test module: fixtures in there may drive
/// git directly. The cut only happens at a `#[cfg(test)]` that opens a `mod`,
/// so a `#[cfg(test)]` on a single item does not hide the code after it.
fn production_text(source: &str) -> &str {
    let marker = "#[cfg(test)]";
    let mut from = 0;
    while let Some(at) = source[from..].find(marker) {
        let end = from + at;
        let rest = without_visibility(source[end + marker.len()..].trim_start());
        if rest.starts_with("mod ") {
            return &source[..end];
        }
        from = end + 1;
    }
    source
}

/// `pub`, `pub(crate)`, `pub(super)` and `pub(in path)` all prefix a test module.
fn without_visibility(item: &str) -> &str {
    match item.strip_prefix("pub") {
        Some(rest) if rest.starts_with('(') => rest
            .find(')')
            .map_or(rest, |close| rest[close + 1..].trim_start()),
        Some(rest) => rest.trim_start(),
        None => item,
    }
}

#[test]
fn test_production_text_restricted_visibility_test_module_cut() {
    for module in [
        "mod t",
        "pub mod t",
        "pub(crate) mod t",
        "pub(in crate::x) mod t",
    ] {
        let source = format!("fn a() {{}}\n#[cfg(test)]\n{module} {{ git }}");
        assert_eq!(production_text(&source), "fn a() {}\n", "{module}");
    }
}

/// `Command::new("git")` however it is spaced, ignoring `//` comments.
fn calls_git_directly(text: &str) -> bool {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<String>()
        .split_whitespace()
        .collect::<String>()
        .contains("Command::new(\"git\")")
}

#[test]
fn test_ecp_cli_production_code_has_no_bare_git_command() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let offenders: Vec<String> = files
        .iter()
        .filter(|path| !path.ends_with("git/safe_exec.rs"))
        .filter(|path| {
            let text = std::fs::read_to_string(path).expect("read source");
            calls_git_directly(production_text(&text))
        })
        .map(|path| path.strip_prefix(&src).unwrap().display().to_string())
        .collect();
    assert!(
        offenders.is_empty(),
        "bare `Command::new(\"git\")` outside git::safe_exec: {offenders:?}"
    );
}
