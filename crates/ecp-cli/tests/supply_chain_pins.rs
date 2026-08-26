//! Pins the facts that `.cargo/audit.toml`'s advisory ignores rest on.
//!
//! An ignore entry suppresses a real advisory on the strength of an argument
//! about the dependency tree. The argument can expire without anyone touching
//! this repo — an upstream refactor is enough. These tests fail when the
//! argument stops holding, so the suppression cannot outlive its reason.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/ecp-cli sits two levels under the workspace root")
        .to_path_buf()
}

fn locked_version(crate_name: &str) -> String {
    let lock =
        std::fs::read_to_string(workspace_root().join("Cargo.lock")).expect("read Cargo.lock");
    let doc: toml::Table = toml::from_str(&lock).expect("parse Cargo.lock");
    let mut found: Vec<String> = doc["package"]
        .as_array()
        .expect("Cargo.lock [[package]] array")
        .iter()
        .filter(|p| p["name"].as_str() == Some(crate_name))
        .map(|p| p["version"].as_str().expect("package version").to_string())
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one locked {crate_name}, found {found:?} — \
         a second copy means the reasoning below covers only one of them"
    );
    found.pop().unwrap()
}

/// `<CARGO_HOME>/registry/src/<index>/<crate>-<version>`, or None when the
/// registry has not been populated (offline / vendored build).
fn registry_src(crate_name: &str, version: &str) -> Option<PathBuf> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))?;
    let indices = std::fs::read_dir(cargo_home.join("registry").join("src")).ok()?;
    for index in indices.flatten() {
        let candidate = index.path().join(format!("{crate_name}-{version}"));
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Key types of every `LruCache<K, V>` written in `src`, in file order.
fn lru_cache_key_types(src: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = src;
    while let Some(at) = rest.find("LruCache<") {
        rest = &rest[at + "LruCache<".len()..];
        let mut depth = 0usize;
        let mut key = String::new();
        for ch in rest.chars() {
            match ch {
                ',' | '>' if depth == 0 => break,
                '<' | '(' | '[' => {
                    depth += 1;
                    key.push(ch);
                }
                '>' | ')' | ']' => {
                    depth -= 1;
                    key.push(ch);
                }
                _ => key.push(ch),
            }
        }
        keys.push(key.trim().to_string());
    }
    keys
}

/// RUSTSEC-2026-0253 is ignored in `.cargo/audit.toml` because tantivy's only
/// `LruCache` keys on `usize`, whose `Drop` cannot panic — the advisory's
/// precondition. A tantivy release that caches under a key with a real `Drop`
/// revives the advisory, and nothing in this repo would otherwise say so.
#[test]
fn tantivy_lru_cache_still_keys_on_usize() {
    let version = locked_version("tantivy");
    let Some(src) = registry_src("tantivy", &version) else {
        panic!(
            "tantivy-{version} sources absent from the cargo registry; this test \
             reads them to re-check the RUSTSEC-2026-0253 ignore in .cargo/audit.toml"
        );
    };

    let mut files = Vec::new();
    rust_sources(&src.join("src"), &mut files);
    assert!(!files.is_empty(), "no .rs files under {}", src.display());

    let mut sites = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read tantivy source");
        for key in lru_cache_key_types(&text) {
            sites.push((file.strip_prefix(&src).unwrap_or(file).to_path_buf(), key));
        }
    }

    assert!(
        !sites.is_empty(),
        "tantivy-{version} declares no LruCache at all — it may have dropped the \
         lru dependency, which would make the .cargo/audit.toml ignore dead config"
    );
    for (file, key) in &sites {
        assert_eq!(
            key,
            "usize",
            "tantivy-{version} caches under key `{key}` at {} — RUSTSEC-2026-0253 \
             applies to keys whose Drop can panic, so re-read the ignore comment \
             in .cargo/audit.toml before keeping the suppression",
            file.display()
        );
    }
}

/// The weaker leg of the same argument, and it covers the prebuilt artifacts
/// only: `install.sh`'s `cargo install --git` fallback builds under the
/// default release profile and does unwind.
#[test]
fn release_dist_profile_still_aborts_on_panic() {
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml"))
        .expect("read workspace Cargo.toml");
    let doc: toml::Table = toml::from_str(&manifest).expect("parse workspace Cargo.toml");
    let panic_setting = doc
        .get("profile")
        .and_then(|p| p.get("release-dist"))
        .and_then(|p| p.get("panic"))
        .and_then(toml::Value::as_str);
    assert_eq!(
        panic_setting,
        Some("abort"),
        "[profile.release-dist] no longer aborts on panic; the RUSTSEC-2026-0253 \
         ignore in .cargo/audit.toml cites this for the released binaries"
    );
}

/// The guard above is only as good as this extraction: it has to read a
/// changed key type, not just confirm the string `usize` appears somewhere.
#[test]
fn lru_key_extraction_reads_the_first_type_parameter() {
    assert_eq!(
        lru_cache_key_types("cache: Option<Mutex<LruCache<usize, Block>>>,"),
        vec!["usize"]
    );
    // A key that owns a heap allocation — exactly the shape that revives
    // RUSTSEC-2026-0253, and the shape the guard must not read as `usize`.
    assert_eq!(
        lru_cache_key_types("LruCache<PathBuf, Vec<u8>>"),
        vec!["PathBuf"]
    );
    assert_eq!(
        lru_cache_key_types("LruCache<(u64, usize), Block>"),
        vec!["(u64, usize)"]
    );
    assert_eq!(
        lru_cache_key_types("let c = LruCache::new(n); // no type parameters"),
        Vec::<String>::new()
    );
}
