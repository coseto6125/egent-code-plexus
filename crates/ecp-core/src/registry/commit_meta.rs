use crate::registry::dirname::SourceType;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

/// String baked into every `CommitBuildMeta` at build time. The fast path
/// in `build_l2` only reuses a cached L2 entry when its persisted
/// fingerprint matches the running binary's — upgrading `ecp` (or bumping
/// the `+schema<N>` suffix below) invalidates older entries automatically,
/// preventing a stale parser from feeding mismatched bytes back to a new
/// reader.
///
/// Bump the `+schema<N>` literal whenever `graph.bin`, `CommitBuildMeta`,
/// or any persisted L2 artefact changes shape in a way pre-bump binaries
/// can't read back, or changes meaning under the same shape. schema2: the
/// test-file classifier changed, and both `File.category` and the parse
/// cache (per-parser test-only emission) carry its verdict. schema3:
/// `RawNode.calls` entries carry `CallSite` markers. schema4: Python
/// `RawImport` sources are repo paths and module imports use the `*`
/// binding, Python callees keep their module qualifier, and Rust closures
/// carry function metadata, all under the same parse-cache shape. schema5:
/// Kotlin and PHP `RawImport.imported_name` is the short name (PHP group
/// members are `*` imports), Kotlin `package` headers emit Namespace nodes,
/// and PHP `$obj->m()` / Java computed-receiver callees are untyped members.
/// schema6: TS / JS `RawNode.is_exported` also marks declarations exported
/// through a local `export { … }` clause or `export default <name>`, and the
/// import tier reads it. schema7: Python calls on a `from`-bound receiver are
/// `CallSite::FromReceiver`, Python `from x import *` emits an unaliased `*`
/// import, Kotlin class bodies closed on the last member's line reparse, the
/// tree-sitter-kotlin pin moved to 1852ea17 (a declaration with two or more
/// modifiers parses as a class), and a one-line `object O { … }` that the
/// grammar misparses with no ERROR node is recovered.
/// schema8: `LocalGraph.export_aliases` and `BindingKind::EsDefault` (TS / JS
/// export aliases and default imports). schema9: `graph.bin` v13 carries the
/// import / export facts of each file, and session fragments (format v3)
/// carry export aliases and record the builder that parsed them. schema10:
/// TS / JS `RawImport.binding_kind` marks exported bindings
/// (`BindingKind::EsReexport` / `EsDefaultReexport`), `export default
/// <expression>` records a `default` export alias with an empty local, and
/// the JS re-export query captures `export { default as X } from`.
/// schema11: the JS queries capture `default` in `import { default as d }`
/// and `export { x as default } from`, a file whose only export is `export
/// default <expression>` is an ES module, and each `graph.bin` TS / JS
/// re-export import row keeps the file its specifier resolved to
/// (`ImportFact::target_file`).
/// schema12: tree-sitter 0.27 recovers some files with syntax errors into
/// different trees than 0.26 did, so a parse of the same source can yield
/// different symbols (nlohmann `json.hpp` gains a friend `operator<`).
/// schema13: a later same-name Method / Constructor overload keeps its name
/// and owner under its own uid instead of an empty-name tombstone, gets its
/// `HasMethod` edge, and the first overload `References` each later one
/// (`overload_set`).
/// schema14: a call through a type binds that type's own member before a
/// same-file interface / trait / base declaration, and a call whose
/// candidates are copies in duplicated source trees binds the nearest copy
/// (`call:nearest-copy`, confidence 0.6).
/// schema15: a function `References` each enum variant it uses
/// (`enum_variant:use` / `enum_variant:pattern`).
/// schema16: `graph.bin` keeps the repo's path aliases and each type's
/// resolved supertypes (`alias_table`, `supertype_table`), so the session
/// overlay resolves aliased specifiers and walks heritage like the index.
/// schema17: a Laravel route carries the prefix, middleware and controller
/// namespace of its route groups, a group that loads a route file applies
/// them to that file's routes, and a `'Ctrl@method'` / `[Ctrl::class, 'm']`
/// handler gets a `HandlesRoute` edge from the controller method.
/// schema18: Laravel `resource` / `apiResource` / `match` / `any` routes, a
/// route file without a framework import that a provider loads, actions a
/// controller inherits, and an `EntryPoint` on each controller action.
/// schema19: Laravel `withoutMiddleware` / `excluded_middleware` remove
/// middleware from a route, a controller's own middleware (`$this->middleware`
/// with `only` / `except`, static `HasMiddleware::middleware()`) joins its
/// routes' list, and a group's controller reaches an included route file.
/// schema20: functions read named constants and reference callbacks; imports
/// persist for every language, and chained calls use callee return types.
/// schema21: a decorator that names several HTTP verbs gives one Route per
/// verb, read from the method argument; a docker-compose flow-mapping `build`
/// is read; the Cargo / pyproject package alias comes from the TOML table; a
/// Python transaction decorator binds the innermost function.
/// schema22: every path routes through the C5 `FileRouter`: one size cap
/// (1 MiB default) on the cold and incremental paths alike, a sniffed OpenAPI
/// YAML reaches the openapi provider, template paths and non-UTF-8 names are
/// never parsed.
/// schema23: the session overlay parses each dirty path with the providers
/// its route lists, as the cold index does (bash, lua, crystal, nim, zig,
/// move, solidity, vyper, cairo, verilog, sql, hcl, protobuf, dockerfile,
/// docker-compose and github-actions fragments now exist), and routes by the
/// repo-relative path rather than the basename.
/// schema24: OpenAPI schema and field nodes carry the line of their key
/// instead of line 1, past YAML anchors, tags, comments and directives.
/// schema25: a non-UTF-8 path that would route is a `path-not-utf8` omission
/// instead of vanishing; the walk applies the size cap to a symlink's target.
/// schema26: every uid site numbers a file through `assign_file`; a dirty
/// file's same-uid redefinition becomes the full build's tombstone, and
/// incremental mode numbers a later overload like the full build.
/// schema27: path-literal, SQL, schema-field and event-topic records carry a
/// bound endpoint; a module-top literal starts at the File node, and a
/// PathLiteral owner is the bound node's owner (`Walk` for `Walk<'s, 't>`).
/// schema28: an EventTopic node keeps its source spelling; `RawEventTopic`
/// carries the canonical pairing key apart (`topic_key`).
/// schema29: Fetches start at the File node; a module-level framework ref
/// binds the File node; Overrides edges follow the stored supertypes per
/// language rule.
/// schema30: Overrides bind the nearest declaration along each language's
/// lookup order and carry the base's resolver confidence; Swift records the
/// override modifier; Dart heritage lists with-mixins; Java interface
/// heritage lists its extends-list; a C# delegate carries its return type.
/// schema31: a C# type beside a same-name type of another generic arity
/// keeps its own node (an overload uid), and a C# generic base binds by
/// arity; the parts of a C# partial type in one known project share one
/// identity for heritage, Overrides and calls.
/// schema32: one Route node per C1 route key; a route handler is an endpoint
/// bound by scope or by its decorated declaration, a closure handler binds its
/// anonymous node, and an unbound or absent handler is a BlindSpot.
/// schema33: a C# delegate, or a member of a type beside a same-name type of
/// another generic arity, is its own node.
/// schema34: a C# call whose receiver type lacks the member retries the
/// extension methods in scope, or records a BlindSpot; a TypeScript method
/// route needs an `@Controller` class; a nested same-name type owns its own
/// members; a test file's route marks no entry point.
/// schema35: a C# call site records its arguments, so the extension retry
/// selects by applicability; a C# call never binds a private or static
/// candidate it cannot reach; a Spring mapping routes only on a controller
/// class. Overlay fragments of schema34 are ignored by their fingerprint.
/// schema36: a C# partial type has one representative part, which heritage
/// edges target; every other part links to it with `PartOf`.
/// schema37: a C# value receiver of unknown type is an untyped member that
/// binds only an applicable extension, at 0.5; a typed field or property
/// receiver keeps its type; a generic method call records the method name
/// without its type arguments. Overlay fragments of schema36 are ignored by
/// their fingerprint.
/// schema38: a C# partial attribute class groups with its parts; a chained
/// C# member call takes the access rule; a keyword-type receiver is a static
/// call; a Color-Color receiver admits static and instance members; an
/// inline route handler is recorded at its argument's span
/// (`Unbound::Inline`), so a middleware closure never serves the route; an
/// echo route's handler is the argument after the path; a Spring mapping is
/// dropped only on an HTTP client (`@FeignClient`, `@HttpExchange`); an
/// annotation type (C# attribute class, Java `@interface`, Kotlin
/// `annotation class`) owns its members.
/// schema39: Kotlin and Swift `FunctionMeta.decorators` drop parser markers;
/// a C# generic attribute class carries its arity in its uid.
/// schema40: a Rust nested fn or closure has no owner, a scoped uid and a
/// lexical-parent row; an HTTP verb matches a whole word of the call name;
/// NestJS, Spring, Gin, FastAPI and JS Express routes carry their router
/// prefix; a route's response shape comes from its handler's span.
/// schema41: a Python, JavaScript or TypeScript nested function, lambda or
/// callback node has no owner, a scoped uid and a lexical-parent row too. A
/// schema40 graph stores those owners, which the overlay's identity check
/// rejects.
/// schema42: an import resolves in the caller's workspace scope first (its
/// own aliases, package names and module root), so a monorepo package's
/// `@/*` binds inside that package.
/// schema43: the global tier admits a pair by the stored `ParsedLanguage`
/// (`admits_global`), so a `.proto` file no longer pairs with an unrouted
/// file; a Python import probes every base path an alias expands to.
/// schema44: `LocalGraph.parsed_language` (the parse reports its
/// `ParsedLanguage`; a Vue/Svelte/Astro file resolves as its script language)
/// and the `sfc-script-mismatch` blind spot for mixed TS/JS script blocks.
/// schema45: an L1 fragment carries `FragmentFile.parsed_language`; a
/// schema44 fragment has no such field and must not be read.
pub const BUILDER_FINGERPRINT: &str = concat!("v", env!("CARGO_PKG_VERSION"), "+schema45");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitBuildMeta {
    pub version: u32,
    pub sha: String,
    pub source_type: SourceType,
    pub source_id: Option<String>,
    pub built_from_worktree: String,
    pub built_at: String,
    pub parent_sha: Option<String>,
    pub node_count: u32,
    pub embedding_status: EmbeddingStatus,
    pub refs_at_build: Vec<RefRecord>,
    #[serde(default)]
    pub refs_seen_since: Vec<RefRecord>,
    /// Fingerprint of the binary that wrote this entry. `None` on meta
    /// files written by pre-fingerprint binaries — treated as a fast-path
    /// miss so the next run rewrites with the current fingerprint.
    #[serde(default)]
    pub builder_fingerprint: Option<String>,
    /// Short SHA of the ecp binary that wrote this entry. `None` on meta
    /// files written by pre-SHA binaries.
    #[serde(default)]
    pub binary_commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefRecord {
    pub ref_name: String,
    pub seen_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbeddingStatus {
    None,
    Skipped,
    Computed,
}

impl CommitBuildMeta {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        crate::registry::io::atomic_write_json(path, value)
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_meta() -> CommitBuildMeta {
        CommitBuildMeta {
            version: 1,
            sha: "abc123def4567890abc123def4567890abc123de".into(),
            source_type: SourceType::Branch,
            source_id: Some("main".into()),
            built_from_worktree: "/work/repo".into(),
            built_at: "2026-05-23T00:00:00Z".into(),
            parent_sha: None,
            node_count: 10,
            embedding_status: EmbeddingStatus::None,
            refs_at_build: vec![],
            refs_seen_since: vec![],
            builder_fingerprint: None,
            binary_commit_sha: None,
        }
    }

    #[test]
    fn binary_commit_sha_roundtrip() {
        let meta = CommitBuildMeta {
            binary_commit_sha: Some("abc1234".into()),
            ..base_meta()
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: CommitBuildMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back, meta);
        assert_eq!(back.binary_commit_sha.as_deref(), Some("abc1234"));
    }

    #[test]
    fn binary_commit_sha_backcompat_missing_field() {
        // JSON written by a pre-SHA binary omits the field entirely.
        let json = r#"{
            "version": 1,
            "sha": "abc123def4567890abc123def4567890abc123de",
            "source_type": "Branch",
            "source_id": "main",
            "built_from_worktree": "/work/repo",
            "built_at": "2026-05-23T00:00:00Z",
            "parent_sha": null,
            "node_count": 10,
            "embedding_status": "None",
            "refs_at_build": [],
            "refs_seen_since": []
        }"#;
        let meta: CommitBuildMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.binary_commit_sha, None);
    }
}
