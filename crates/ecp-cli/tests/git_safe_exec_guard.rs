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

/// Source text before the first `#[cfg(test)]` marker: fixtures in test
/// modules may drive git directly.
fn production_text(source: &str) -> &str {
    source
        .find("#[cfg(test)]")
        .map_or(source, |end| &source[..end])
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
            production_text(&text).contains("Command::new(\"git\")")
        })
        .map(|path| path.strip_prefix(&src).unwrap().display().to_string())
        .collect();
    assert!(
        offenders.is_empty(),
        "bare `Command::new(\"git\")` outside git::safe_exec: {offenders:?}"
    );
}
