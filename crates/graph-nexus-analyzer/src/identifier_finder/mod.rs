//! Multi-language identifier-occurrence finder used by `gnx rename`'s
//! Stage 2 verification pass. Dispatches by file extension to a per-
//! language module that walks the tree-sitter AST and emits byte-ranges
//! for every identifier whose text matches the target symbol.
//!
//! Supported languages cover the 14-row main matrix (Python, TypeScript/
//! TSX, JavaScript, Rust, Java, Kotlin, C#, Go, PHP, Ruby, Swift, C, C++,
//! Dart) plus 12 extras with renameable identifiers (Bash, Lua, Solidity,
//! Crystal, Nim, Cairo, Move, Zig, HCL, SQL, Verilog, Vyper). Files
//! outside the supported set (markup / configs / Dockerfile) return an
//! empty vec and the caller treats that as "skip this file".

pub mod bash;
pub mod c;
pub mod c_sharp;
pub mod cairo;
pub mod cpp;
pub mod crystal;
pub mod dart;
mod generic;
pub mod go;
pub mod hcl;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod lua;
pub mod move_lang;
pub mod nim;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod solidity;
pub mod sql;
pub mod swift;
pub mod typescript;
pub mod verilog;
pub mod vyper;
pub mod zig;

use graph_nexus_core::analyzer::types::IdentifierRange;

/// Dispatch identifier-occurrence scan to the matching per-language
/// implementation based on `path`'s file extension. Returns an empty
/// vec for unsupported languages — callers treat that as "skip file".
pub fn find_identifier_occurrences(
    path: &str,
    source: &[u8],
    target_name: &str,
) -> Vec<IdentifierRange> {
    let ext = ext_of(path);
    match ext.as_str() {
        "py" | "pyi" => python::find_identifier_occurrences(source, target_name),
        "ts" => typescript::find_identifier_occurrences(source, target_name),
        "tsx" => typescript::find_identifier_occurrences_tsx(source, target_name),
        "js" | "jsx" | "mjs" | "cjs" => javascript::find_identifier_occurrences(source, target_name),
        "rs" => rust::find_identifier_occurrences(source, target_name),
        "java" => java::find_identifier_occurrences(source, target_name),
        "kt" | "kts" => kotlin::find_identifier_occurrences(source, target_name),
        "cs" => c_sharp::find_identifier_occurrences(source, target_name),
        "go" => go::find_identifier_occurrences(source, target_name),
        "php" => php::find_identifier_occurrences(source, target_name),
        "rb" => ruby::find_identifier_occurrences(source, target_name),
        "swift" => swift::find_identifier_occurrences(source, target_name),
        "c" | "h" => c::find_identifier_occurrences(source, target_name),
        "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" => {
            cpp::find_identifier_occurrences(source, target_name)
        }
        "dart" => dart::find_identifier_occurrences(source, target_name),
        // ── Extras with renameable identifiers ──
        "sh" | "bash" => bash::find_identifier_occurrences(source, target_name),
        "lua" => lua::find_identifier_occurrences(source, target_name),
        "sol" => solidity::find_identifier_occurrences(source, target_name),
        "cr" => crystal::find_identifier_occurrences(source, target_name),
        "nim" | "nims" => nim::find_identifier_occurrences(source, target_name),
        "cairo" => cairo::find_identifier_occurrences(source, target_name),
        "move" => move_lang::find_identifier_occurrences(source, target_name),
        "zig" | "zon" => zig::find_identifier_occurrences(source, target_name),
        "hcl" | "tf" | "tfvars" => hcl::find_identifier_occurrences(source, target_name),
        "sql" => sql::find_identifier_occurrences(source, target_name),
        "v" | "sv" | "vh" | "svh" => verilog::find_identifier_occurrences(source, target_name),
        "vy" => vyper::find_identifier_occurrences(source, target_name),
        _ => Vec::new(),
    }
}

/// Lowercase extension extracted from the final `.`-segment of `path`.
/// Empty for paths with no extension. We use this instead of
/// `std::path::Path::extension()` because the input is a string path
/// (relative to repo root) and we want exact case-insensitive matching
/// across OSes.
fn ext_of(path: &str) -> String {
    path.rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_unknown_extension_returns_empty() {
        let hits = find_identifier_occurrences("README.md", b"# foo\n", "foo");
        assert!(hits.is_empty());
    }

    #[test]
    fn dispatch_no_extension_returns_empty() {
        let hits = find_identifier_occurrences("Makefile", b"foo:\n", "foo");
        assert!(hits.is_empty());
    }

    #[test]
    fn dispatch_python_routes_to_python_finder() {
        let hits = find_identifier_occurrences("a.py", b"def foo(): pass\nfoo()\n", "foo");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn dispatch_rust_routes_to_rust_finder() {
        let hits = find_identifier_occurrences(
            "a.rs",
            b"fn foo() {}\nfn main() { foo(); }\n",
            "foo",
        );
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn dispatch_extension_is_case_insensitive() {
        let hits = find_identifier_occurrences("A.PY", b"def foo(): pass\nfoo()\n", "foo");
        assert_eq!(hits.len(), 2);
    }
}
