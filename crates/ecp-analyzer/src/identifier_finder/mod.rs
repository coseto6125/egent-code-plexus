//! Multi-language identifier-occurrence finder used by `ecp rename`'s
//! Stage 2 verification pass. The C5 route of the path picks a per-language
//! module that walks the tree-sitter AST and emits byte-ranges for every
//! identifier whose text matches the target symbol.
//!
//! Supported languages are those with `Capability::IdentifierFinder`: the
//! 14-row main matrix (Python, TypeScript/TSX, JavaScript, Rust, Java,
//! Kotlin, C#, Go, PHP, Ruby, Swift, C, C++, Dart) plus 12 extras with
//! renameable identifiers (Bash, Lua, Solidity, Crystal, Nim, Cairo, Move,
//! Zig, HCL, SQL, Verilog, Vyper). A file outside that set (markup, config,
//! Dockerfile, Vue / Svelte / Astro) returns `None`, and the caller reports
//! it as not scanned.

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

use ecp_core::analyzer::types::IdentifierRange;
use ecp_core::contracts::routing::{Dialect, FileRouter, IdentifierFinder, Language, RouteOutcome};

/// Identifier occurrences of `target_name` in `source`, or `None` when
/// `path` has no identifier finder. `None` and an empty vec differ: the
/// caller reports a `None` file as not scanned, never as clean.
///
/// The C5 route of `path` picks the finder (`FileRoute::identifier_finder`),
/// so extensions match case-sensitively, as the index routes them.
pub fn find_identifier_occurrences(
    path: &str,
    source: &[u8],
    target_name: &str,
) -> Option<Vec<IdentifierRange>> {
    let finder = match FileRouter::PATH_ONLY.route(path) {
        RouteOutcome::Routed(route) => route.identifier_finder(),
        RouteOutcome::Unrouted => return find_in_unrouted(path, source, target_name),
        RouteOutcome::Excluded(_) => IdentifierFinder::Unsupported,
    };
    let IdentifierFinder::Supported { language, dialect } = finder else {
        return None;
    };
    Some(match language {
        Language::Python => python::find_identifier_occurrences(source, target_name),
        Language::TypeScript => match dialect {
            Dialect::Tsx => typescript::find_identifier_occurrences_tsx(source, target_name),
            Dialect::Standard => typescript::find_identifier_occurrences(source, target_name),
        },
        Language::JavaScript => javascript::find_identifier_occurrences(source, target_name),
        Language::Rust => rust::find_identifier_occurrences(source, target_name),
        Language::Java => java::find_identifier_occurrences(source, target_name),
        Language::Kotlin => kotlin::find_identifier_occurrences(source, target_name),
        Language::CSharp => c_sharp::find_identifier_occurrences(source, target_name),
        Language::Go => go::find_identifier_occurrences(source, target_name),
        Language::Php => php::find_identifier_occurrences(source, target_name),
        Language::Ruby => ruby::find_identifier_occurrences(source, target_name),
        Language::Swift => swift::find_identifier_occurrences(source, target_name),
        Language::C => c::find_identifier_occurrences(source, target_name),
        Language::Cpp => cpp::find_identifier_occurrences(source, target_name),
        Language::Dart => dart::find_identifier_occurrences(source, target_name),
        Language::Bash => bash::find_identifier_occurrences(source, target_name),
        Language::Lua => lua::find_identifier_occurrences(source, target_name),
        Language::Solidity => solidity::find_identifier_occurrences(source, target_name),
        Language::Crystal => crystal::find_identifier_occurrences(source, target_name),
        Language::Nim => nim::find_identifier_occurrences(source, target_name),
        Language::Cairo => cairo::find_identifier_occurrences(source, target_name),
        Language::Move => move_lang::find_identifier_occurrences(source, target_name),
        Language::Zig => zig::find_identifier_occurrences(source, target_name),
        Language::Hcl => hcl::find_identifier_occurrences(source, target_name),
        Language::Sql => sql::find_identifier_occurrences(source, target_name),
        Language::Verilog => verilog::find_identifier_occurrences(source, target_name),
        Language::Vyper => vyper::find_identifier_occurrences(source, target_name),
        // No `Capability::IdentifierFinder`, so `identifier_finder()` never
        // names them.
        Language::Dockerfile
        | Language::Vue
        | Language::Astro
        | Language::Svelte
        | Language::Protobuf
        | Language::DockerCompose
        | Language::GitHubActions
        | Language::Kubernetes
        | Language::OpenApi
        | Language::Yaml => return None,
    })
}

/// `nims` and `zon` had a finder before the route existed, and no rule
/// routes them yet (C5 open point 4). They keep the finder until that point
/// is decided, so no extension loses one in the move to the route.
fn find_in_unrouted(path: &str, source: &[u8], target_name: &str) -> Option<Vec<IdentifierRange>> {
    match path.rsplit_once('.')?.1 {
        "nims" => Some(nim::find_identifier_occurrences(source, target_name)),
        "zon" => Some(zig::find_identifier_occurrences(source, target_name)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(path: &str, source: &[u8], target: &str) -> Vec<IdentifierRange> {
        find_identifier_occurrences(path, source, target)
            .unwrap_or_else(|| panic!("{path} should have a finder"))
    }

    fn spans(found: &[IdentifierRange]) -> Vec<(usize, usize)> {
        found.iter().map(|r| (r.start_byte, r.end_byte)).collect()
    }

    #[test]
    fn dispatch_unknown_extension_returns_none() {
        assert!(find_identifier_occurrences("README.md", b"# foo\n", "foo").is_none());
    }

    #[test]
    fn dispatch_no_extension_returns_none() {
        assert!(find_identifier_occurrences("Makefile", b"foo:\n", "foo").is_none());
    }

    #[test]
    fn dispatch_python_routes_to_python_finder() {
        assert_eq!(hits("a.py", b"def foo(): pass\nfoo()\n", "foo").len(), 2);
    }

    #[test]
    fn dispatch_rust_routes_to_rust_finder() {
        let found = hits("a.rs", b"fn foo() {}\nfn main() { foo(); }\n", "foo");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn dispatch_extras_bash_routes_via_sh_extension() {
        // Guards against `.sh` ever being routed to the wrong module.
        assert_eq!(hits("a.sh", b"foo() { :; }\nfoo\n", "foo").len(), 2);
    }

    #[test]
    fn dispatch_extras_sql_routes_via_sql_extension() {
        let found = hits(
            "a.sql",
            b"CREATE TABLE users (id INT);\nSELECT * FROM users;\n",
            "users",
        );
        assert!(found.len() >= 2, "{:?}", found);
    }

    #[test]
    fn test_find_identifier_occurrences_vue_returns_no_finder() {
        // The `.vue` file names the target; the `.py` file does not. Only the
        // second one is "scanned, nothing found".
        let vue = b"<script>\nexport function foo() {}\nfoo()\n</script>\n";
        assert!(find_identifier_occurrences("src/App.vue", vue, "foo").is_none());
        let py = find_identifier_occurrences("src/a.py", b"def bar(): pass\n", "foo");
        assert!(py.is_some_and(|found| found.is_empty()));
    }

    #[test]
    fn test_find_identifier_occurrences_upper_case_extension_routes_like_index() {
        // The index leaves `A.RS` unrouted (C5 open point 3), so the finder
        // has nothing to scan there either; `a.rs` with the same bytes does.
        let source = b"fn foo() {}\nfn main() { foo(); }\n";
        assert!(find_identifier_occurrences("src/A.RS", source, "foo").is_none());
        assert_eq!(hits("src/a.rs", source, "foo").len(), 2);
    }

    #[test]
    fn test_find_identifier_occurrences_tsx_dialect_picks_tsx_grammar() {
        // The route's `Tsx` dialect picks the TSX grammar, which reads
        // `<Foo />` as a JSX tag; `.ts` keeps the plain TypeScript grammar.
        let source = b"function Foo() { return null; }\nconst x = <Foo />;\n";
        let tsx = spans(&hits("ui/App.tsx", source, "Foo"));
        assert_eq!(
            tsx,
            spans(&typescript::find_identifier_occurrences_tsx(source, "Foo"))
        );
        assert_eq!(tsx.len(), 2, "{tsx:?}");
        let plain = b"function Foo() {}\nFoo();\n";
        assert_eq!(
            spans(&hits("ui/app.ts", plain, "Foo")),
            spans(&typescript::find_identifier_occurrences(plain, "Foo"))
        );
    }

    #[test]
    fn test_find_identifier_occurrences_every_capable_extension_has_finder() {
        // Each extension the route gives `Capability::IdentifierFinder` gets
        // `Some`, and each other default extension gets `None`.
        use ecp_core::contracts::routing::Capability;
        for &language in Language::ALL {
            let capable = language
                .capabilities()
                .contains(Capability::IdentifierFinder);
            for ext in language.extensions() {
                let path = format!("src/x.{ext}");
                assert_eq!(
                    find_identifier_occurrences(&path, b"", "foo").is_some(),
                    capable,
                    "{path}"
                );
            }
        }
    }

    #[test]
    fn test_find_identifier_occurrences_unrouted_nims_zon_keep_finder() {
        // C5 open point 4 is open, so the two pre-route extensions keep one.
        assert!(find_identifier_occurrences("build.nims", b"", "foo").is_some());
        assert!(find_identifier_occurrences("build.zig.zon", b"", "foo").is_some());
    }

    #[test]
    fn test_find_identifier_occurrences_template_dir_returns_none() {
        let source = b"def foo(): pass\n";
        assert!(find_identifier_occurrences("__brick__/lib/a.py", source, "foo").is_none());
    }
}
