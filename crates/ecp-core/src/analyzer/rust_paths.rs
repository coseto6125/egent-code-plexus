//! Rust module layout shared by index-time and session-overlay resolution.

/// A Rust source file, by its `.rs` extension in any case.
pub fn is_rust_source(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

/// A file that names its own directory's module: `mod.rs`, `lib.rs`,
/// `main.rs`, and every Cargo target root (`src/bin/<name>.rs`, top-level
/// `examples/`, `benches/`, `tests/` files, `build.rs`). Every other `.rs`
/// file is a module named after its stem. The top-level target rules require
/// no `src` ancestor, so `src/a/tests/x.rs` stays an ordinary module.
pub fn is_rust_module_root(source_file: &std::path::Path) -> bool {
    matches!(
        source_file.file_stem().and_then(|stem| stem.to_str()),
        Some("mod" | "lib" | "main")
    ) || is_rust_target_root(source_file)
}

/// Conventional Cargo target roots, excluding ordinary `mod.rs` files.
pub fn is_rust_target_root(source_file: &std::path::Path) -> bool {
    let Some(dir) = source_file.parent() else {
        return false;
    };
    let dir_name = dir.file_name().and_then(|n| n.to_str());
    let filename = source_file.file_name().and_then(|n| n.to_str());
    if matches!(filename, Some("lib.rs" | "main.rs")) && dir_name == Some("src") {
        return true;
    }
    if filename == Some("main.rs")
        && dir.parent().is_some_and(|parent| {
            parent.file_name().is_some_and(|name| name == "bin")
                && parent
                    .parent()
                    .is_some_and(|src| src.file_name().is_some_and(|name| name == "src"))
        })
    {
        return true;
    }
    let in_src = |d: &std::path::Path| d.components().any(|c| c.as_os_str() == "src");
    let is_build_rs = source_file.file_name().is_some_and(|n| n == "build.rs");
    match dir_name {
        Some("bin") => dir
            .parent()
            .and_then(|g| g.file_name())
            .is_some_and(|n| n == "src"),
        Some("examples" | "benches" | "tests") => !in_src(dir),
        _ => is_build_rs && !in_src(dir),
    }
}

/// Directory that holds the child modules of `source_file`'s module — the
/// base of `self::`. Rust 2018 puts the children of `a/b.rs` in `a/b/`.
pub fn rust_module_dir(source_file: &std::path::Path) -> Option<std::path::PathBuf> {
    let own_dir = source_file.parent()?;
    if is_rust_module_root(source_file) {
        Some(own_dir.to_path_buf())
    } else {
        Some(own_dir.join(source_file.file_stem()?))
    }
}

/// Expand a Rust `use`-path module specifier to the caller crate's
/// `src/<segments>` base so Tier-2 import resolution can pin the declaring
/// module. Returns `None` for non-Rust specifiers (TS/Python/etc. keep their
/// existing relative-resolution branches).
///
/// Both the full resolver and the session overlay use this layout helper.
/// External crate heads (`std::`, `serde::`) have no local expansion:
/// * `crate::output` from `crates/ecp-cli/src/commands/find.rs`
///   → `crates/ecp-cli/src/output`; from a repo-root crate's
///   `src/commands/find.rs` → `src/output`
/// * `self::a` → `a` under the caller module's child directory
///   (`a/b.rs` → `a/b/a`, `a/b/mod.rs` → `a/b/a`)
/// * `super::a` → `a` under the parent module's directory
///   (`a/b.rs` → `a/a`, `a/b/mod.rs` → `a/a`)
///
/// The trailing item name is NOT part of `import.source` (the parser splits
/// `use crate::output::{emit}` into source=`crate::output`, name=`emit`), so
/// every `::` segment here is a module path component.
pub fn rust_module_path_base(
    source_file: &std::path::Path,
    specifier: &str,
) -> Option<std::path::PathBuf> {
    if !is_rust_source(source_file) {
        return None;
    }
    let segs: Vec<&str> = specifier.split("::").filter(|s| !s.is_empty()).collect();
    let (anchor, rest) = match segs.split_first()? {
        (&"crate", rest) => {
            let path = source_file.to_string_lossy().replace('\\', "/");
            let src_root = match path.rsplit_once("/src/") {
                Some((root, _)) => format!("{root}/src"),
                // A crate at the repo root: its repo-relative paths start at
                // `src/`, with no `/src/` segment to split on.
                None if path.starts_with("src/") => "src".to_owned(),
                None => return None,
            };
            (std::path::PathBuf::from(src_root), rest)
        }
        (&"self", rest) => (rust_module_dir(source_file)?, rest),
        (&"super", mut rest) => {
            // `super` is the parent of the file's own module, so it is
            // `rust_module_dir`'s parent; each further `super` climbs one
            // more module.
            let mut anchor = rust_module_dir(source_file)?.parent()?.to_path_buf();
            while let Some((&"super", tail)) = rest.split_first() {
                anchor = anchor.parent()?.to_path_buf();
                rest = tail;
            }
            (anchor, rest)
        }
        _ => return None,
    };
    Some(rest.iter().fold(anchor, |p, seg| p.join(seg)))
}
