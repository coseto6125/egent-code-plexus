use super::receiver_types::{collect_local_types, extract_python_calls_and_path_literals};
use super::spec::PythonSpec;
use crate::framework_confidence;
use crate::framework_helpers::{
    enclosing_class, enclosing_function_name, enumerate_class_methods, has_import_from, node_span,
    point_in_span, Span, MODULE_LEVEL_SOURCE,
};
use crate::indirect_dispatch::{collect_python_param_names, detect_python_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::algorithms::process_trace::is_test_path;
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    BlindSpot, FrameworkId, LocalGraph, RawFanoutRef, RawFrameworkRef, RawImport, RawNode,
    RawRoute, RawTxScope,
};
use ecp_core::graph::{FileCategory, NodeKind};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor};

// Framework-presence gates: only claim "this is a FastAPI/Django/Celery ref"
// when the file actually imports the framework. Reflection fan-out and
// blind_spots are intentionally not gated — they are language-level patterns.
const FASTAPI_REQUIRED: &[&str] = &["fastapi"];
const DJANGO_REQUIRED: &[&str] = &["django"];
const CELERY_REQUIRED: &[&str] = &["celery"];

/// Module prefixes that gate generic Route emission. A file that doesn't
/// import any of these can still produce framework-specific refs
/// (FastAPI Depends, Django signals, Celery tasks) but its `obj.get(...)`
/// / `obj.post(...)` calls are NOT considered routes — they're almost
/// always `dict.get` / response-key access FPs. Spec:
/// `docs/superpowers/specs/2026-05-17-route-precision-design.md`.
const HTTP_FRAMEWORK_MODULES: &[&str] = &[
    "fastapi",
    "flask",
    "django",
    "starlette",
    "aiohttp",
    "tornado",
    "sanic",
    "bottle",
    "falcon",
    "pyramid",
    "quart",
    "litestar",
];

/// Blind-spot kind/hint pairs. Order matches the capture-index lookup in
/// `parse_file` (eval / exec / compile / dynamic-import / builtin-import /
/// cross-getattr) so the dispatch reads as a flat table.
const BLIND_SPEC: &[(&str, &str)] = &[
    (
        "python-eval",
        "eval(arg) — runtime Python code execution; called function is not statically determinable",
    ),
    (
        "python-exec",
        "exec(arg) — runtime statement execution; executed code is not statically determinable",
    ),
    (
        "python-compile",
        "compile(arg) — runtime bytecode compile; produced code object is not statically determinable",
    ),
    (
        "python-dynamic-import",
        "importlib.import_module(...) — dynamic module loading; imported module name depends on runtime value",
    ),
    (
        "python-builtin-import",
        "__import__(...) — dynamic builtin import; module name depends on runtime value",
    ),
    (
        "python-cross-getattr",
        "getattr(<obj>, name)() with obj != self — cross-object reflection; target class not enumerated by ecp Phase 2",
    ),
];

/// Test-client chain markers — segments that appear in test-suite client
/// patterns but not in production route registration. `app.test_client.get(...)`
/// (Flask / Sanic / Tornado), `app.asgi_client.get(...)` (Sanic async),
/// `app.sync_client.get(...)` (some custom test harnesses). Each is
/// overwhelmingly a testing convention; user code that names a production
/// attribute the same way would be false-negative.
const TEST_CLIENT_CHAIN_MARKERS: &[&str] = &[".test_client.", ".asgi_client.", ".sync_client."];

/// Route-registration method names that don't encode an HTTP verb in the
/// name. They default to GET when no `methods=[...]` kwarg is supplied;
/// otherwise the kwarg specifies the verb(s). Used to gate (a) the
/// framework-presence relaxation, (b) bare-path normalization, (c) the
/// methods-kwarg parse branch. Single source of truth — keeps the three
/// emission-time checks aligned when a new registration method is added.
const REGISTRATION_METHOD_NAMES: &[&str] = &["route", "add_route", "add_url_rule", "add_api_route"];

/// Direct receiver names that — only inside test files (path/filename
/// classified as Test) — indicate a test-client variable injected via
/// pytest fixture or similar. Common conventions: `http_client` (Sanic
/// inspector tests), `client` / `api_client` / `async_client` (Flask /
/// FastAPI). In production files these names could legitimately be a
/// Blueprint or app variable, so we only treat them as test-client when
/// the source file is itself a test file.
///
/// Caller must combine with `is_test_path(file_path)`. Empirical impact
/// on sanic-org/sanic `tests/worker/test_inspector.py`: removes 3 final
/// `--include-tests` FPs (`/reload`, `/shutdown`, `/scale`) where
/// `http_client` is a pytest fixture function.
const TEST_FILE_DIRECT_RECEIVERS: &[&str] = &[
    "http_client",
    "client",
    "api_client",
    "async_client",
    "asgi_client",
    "test_client",
    "sync_client",
];

/// True when `call_node` sits inside a `(decorator ...)` parent. Flask
/// Blueprint shorthand `@bp.get("/path")` / `.post(...)` etc. is a
/// decorator-only call — the `.get`/`.post`/`.put` method names overlap
/// with `dict.get(key)` / `requests.get(url)` semantically, so we can't
/// gate on the method name alone; but `@expr(...)` decorators always wrap
/// callables that are about to register a route handler, never dict access.
/// This lets the route emitter recognize Blueprint shorthand even when
/// the file lacks an explicit `from flask import` (transitive imports via
/// conftest fixtures, blueprint re-exports, etc).
fn is_in_decorator(call_node: Node) -> bool {
    let mut anc = call_node.parent();
    while let Some(a) = anc {
        match a.kind() {
            "decorator" => return true,
            // Don't ascend out of a decorator's scope by accident.
            "function_definition" | "class_definition" | "module" => return false,
            _ => {}
        }
        anc = a.parent();
    }
    false
}

/// True when `call_node`'s function chain's immediate receiver is a bare
/// identifier matching one of `TEST_FILE_DIRECT_RECEIVERS`. Used only in
/// combination with a test-file path check (caller responsibility).
fn is_test_file_direct_receiver_call(call_node: Node, source: &[u8]) -> bool {
    let Some(fn_node) = call_node.child_by_field_name("function") else {
        return false;
    };
    // function is an attribute node like `http_client.post`; its object
    // field is the receiver. Only check bare identifiers — chained
    // accesses like `self.http_client.X` are out of scope for this filter
    // (the `.test_client.` chain filter handles those).
    let Some(obj_node) = fn_node.child_by_field_name("object") else {
        return false;
    };
    if obj_node.kind() != "identifier" {
        return false;
    }
    let Ok(name) = std::str::from_utf8(&source[obj_node.start_byte()..obj_node.end_byte()]) else {
        return false;
    };
    TEST_FILE_DIRECT_RECEIVERS.contains(&name)
}

/// True when the call's function chain contains any test-client marker,
/// identifying patterns like `app.test_client.get('/x')` /
/// `self.app.asgi_client.post(...)`. These are test-client REQUESTS, not
/// route DEFINITIONS — tree-sitter sees the same call shape for both, so
/// the parser-side filter keeps them out of `routes` regardless of file
/// category. Empirical impact on sanic-org/sanic: cuts ~88% of the
/// `--include-tests` "extra paths" vs gitnexus.
fn is_test_client_chained_call(call_node: Node, source: &[u8]) -> bool {
    let Some(fn_node) = call_node.child_by_field_name("function") else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&source[fn_node.start_byte()..fn_node.end_byte()]) else {
        return false;
    };
    TEST_CLIENT_CHAIN_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

/// Walk a route-registration call node (`@app.route(...)`, `app.add_url_rule(...)`,
/// `app.add_api_route(...)`) for the optional `methods=[...]` kwarg.
///
/// Return value semantics (matches caller's three-state handling):
/// - `None` — no `methods=` kwarg at all → caller defaults to `["GET"]`
///   per Flask / Sanic / FastAPI semantics.
/// - `Some(empty)` — kwarg present but value isn't a list of string literals
///   we can parse → caller skips emit (don't fabricate a method).
/// - `Some(non-empty)` — parsed methods, caller emits one Route per method.
///
/// P1 review fix for PR #50: previously `@app.route("/x", methods=["POST"])`
/// silently translated to `GET /x`, producing fake GET routes and missing
/// real POSTs.
fn extract_methods_kwarg(call_node: Node, source: &[u8]) -> Option<Vec<String>> {
    let args = call_node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.kind() != "keyword_argument" {
            continue;
        }
        // Use `continue` not `?` for inner-loop skips — `?` would abort
        // the entire function on a single malformed kwarg and silently
        // fall back to the default `GET`, masking later valid `methods=`
        // arguments.
        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };
        let Ok(name) = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
        else {
            continue;
        };
        if name != "methods" {
            continue;
        }
        let Some(raw_value) = child.child_by_field_name("value") else {
            continue;
        };
        // Unwrap `frozenset({...})` / `set([...])` / `tuple([...])` wrappers
        // common in Sanic (`methods=frozenset({"PUT","POST"})`). The literal
        // collection (set/list/tuple) lives as the first arg of the call.
        let value = match raw_value.kind() {
            "call" => {
                let fn_node = raw_value.child_by_field_name("function")?;
                let fn_name =
                    std::str::from_utf8(&source[fn_node.start_byte()..fn_node.end_byte()]).ok()?;
                if !matches!(fn_name, "frozenset" | "set" | "tuple" | "list") {
                    return Some(Vec::new()); // unrecognized wrapper
                }
                let args = raw_value.child_by_field_name("arguments")?;
                let mut arg_cursor = args.walk();
                let inner = args
                    .children(&mut arg_cursor)
                    .find(|c| matches!(c.kind(), "set" | "list" | "tuple"));
                inner.unwrap_or(raw_value)
            }
            _ => raw_value,
        };
        if !matches!(value.kind(), "list" | "set" | "tuple") {
            return Some(Vec::new()); // present but unparseable
        }
        let mut methods = Vec::new();
        let mut list_cursor = value.walk();
        for el in value.children(&mut list_cursor) {
            if el.kind() != "string" {
                continue;
            }
            if let Ok(text) = std::str::from_utf8(&source[el.start_byte()..el.end_byte()]) {
                let cleaned = text
                    .trim_matches(|c: char| c == '\'' || c == '"' || c == '`')
                    .to_ascii_uppercase();
                if !cleaned.is_empty() {
                    methods.push(cleaned);
                }
            }
        }
        return Some(methods);
    }
    None
}

/// When `call_node` is the call inside a route decorator
/// (`@router.post("/x")` style), resolve the decorated function/class name
/// so it can be carried on `RawRoute.handler`. Returns `None` if the call
/// isn't decorator-wrapped — the caller treats that as the imperative
/// `app.add_url_rule(path, handler)` shape (handler resolution happens
/// later in the builder).
///
/// Walk: `call → decorator → decorated_definition →
/// child_by_field_name("definition") → child_by_field_name("name")`.
/// `decorated_definition.definition` is either a `function_definition` or
/// a `class_definition` per tree-sitter-python grammar — both expose a
/// `name` field, so a single lookup suffices.
fn resolve_decorator_handler(call_node: Node, source: &[u8]) -> Option<String> {
    let dec = call_node.parent()?;
    if dec.kind() != "decorator" {
        return None;
    }
    let decorated = dec.parent()?;
    if decorated.kind() != "decorated_definition" {
        return None;
    }
    let definition = decorated.child_by_field_name("definition")?;
    let name = definition.child_by_field_name("name")?;
    std::str::from_utf8(&source[name.start_byte()..name.end_byte()])
        .ok()
        .map(str::to_string)
}

/// Push a Django signal RawFrameworkRef when both `sig_node` (signal name) and
/// `handler_node` (handler identifier) decode as UTF-8. Shared by `@receiver`
/// decorator and `signal.connect(handler)` capture sites — only `reason` differs.
fn push_django_signal_ref(
    sig_node: Node,
    handler_node: Node,
    source: &[u8],
    reason: &str,
    dest: &mut Vec<RawFrameworkRef>,
) {
    if let (Ok(sig), Ok(handler)) = (
        std::str::from_utf8(&source[sig_node.start_byte()..sig_node.end_byte()]),
        std::str::from_utf8(&source[handler_node.start_byte()..handler_node.end_byte()]),
    ) {
        dest.push(RawFrameworkRef {
            source_name: sig.to_string(),
            target_name: handler.to_string(),
            confidence: framework_confidence::DJANGO_SIGNAL,
            reason: reason.to_string(),
            span: node_span(&handler_node),
        });
    }
}

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_python::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct PythonProvider {
    query: Query,
    indices: PythonCaptureIndices,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `PythonSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index — equivalent perf to the previous
    /// if-chain, but the source of truth lives in `spec.rs`.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

struct PythonCaptureIndices {
    property: Option<u32>,
    variable: Option<u32>,
    type_ann: Option<u32>,
    heritage: Option<u32>,
    export: Option<u32>,
    import_name: Option<u32>,
    import_source: Option<u32>,
    import_alias: Option<u32>,
    decorator: Option<u32>,
    function: Option<u32>,
    class: Option<u32>,
    route_method: Option<u32>,
    route_path: Option<u32>,
    route_call: Option<u32>,
    fastapi_depends_target: Option<u32>,
    fastapi_route_app: Option<u32>,
    fastapi_route_method: Option<u32>,
    fastapi_route_handler: Option<u32>,
    django_url_handler: Option<u32>,
    django_signal_receiver_name: Option<u32>,
    django_signal_receiver_handler: Option<u32>,
    django_signal_connect_name: Option<u32>,
    django_signal_connect_handler: Option<u32>,
    celery_task_handler: Option<u32>,
    tx_atomic_handler: Option<u32>,
    tx_db_session_handler: Option<u32>,
    reflection_getattr_site: Option<u32>,
    blind_eval: Option<u32>,
    blind_exec: Option<u32>,
    blind_compile: Option<u32>,
    blind_dynamic_import: Option<u32>,
    blind_builtin_import: Option<u32>,
    blind_cross_getattr: Option<u32>,
}

/// True when every entry in `bases` is a Protocol-marker — meaning the class
/// declares a structural interface contract rather than concrete inheritance.
///
/// Spec §4.5b: when ALL bases are Protocol-markers, promote NodeKind::Class →
/// NodeKind::Interface so the kind-based Implements dispatch treats inheritors
/// as "implements" rather than "extends". Mixed bases keep NodeKind::Class to
/// preserve concrete inheritance semantics for the non-marker base(s).
///
/// Marker set: `Protocol`, `ABC`, `ABCMeta` (bare or dotted with `typing`,
/// `typing_extensions`, `abc` prefixes), and their subscript forms like
/// `Protocol[T]`. `Generic` and `Generic[T]` are NOT markers — they express
/// parameterization, not interface semantics.
fn is_protocol_marker_only(bases: &[String]) -> bool {
    if bases.is_empty() {
        return false;
    }
    bases.iter().all(|base| {
        // Strip subscript suffix: `Protocol[T]` → `Protocol`, `typing.Protocol[T_co]` → `typing.Protocol`
        let stripped = base
            .find('[')
            .map(|i| &base[..i])
            .unwrap_or(base.as_str())
            .trim();
        matches!(
            stripped,
            "Protocol"
                | "ABC"
                | "ABCMeta"
                | "typing.Protocol"
                | "typing.ABC"
                | "typing.ABCMeta"
                | "typing_extensions.Protocol"
                | "typing_extensions.ABC"
                | "typing_extensions.ABCMeta"
                | "abc.ABC"
                | "abc.ABCMeta"
        )
    })
}

/// True when `func_def` is a `def`/`async def` defined directly inside a
/// `class` body (vs free-standing or nested inside another function).
/// Walks: function_definition → [decorated_definition] → block → class_definition.
fn is_class_method(func_def: Node) -> bool {
    let outer = match func_def.parent() {
        Some(p) if p.kind() == "decorated_definition" => p.parent(),
        other => other,
    };
    let Some(block) = outer else { return false };
    if block.kind() != "block" {
        return false;
    }
    block
        .parent()
        .is_some_and(|p| p.kind() == "class_definition")
}

impl PythonProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_python::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| PythonSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        let indices = PythonCaptureIndices {
            property: query.capture_index_for_name("property"),
            variable: query.capture_index_for_name("variable"),
            type_ann: query.capture_index_for_name("type"),
            heritage: query.capture_index_for_name("heritage"),
            export: query.capture_index_for_name("export"),
            import_name: query.capture_index_for_name("import.name"),
            import_source: query.capture_index_for_name("import.source"),
            import_alias: query.capture_index_for_name("import.alias"),
            decorator: query.capture_index_for_name("decorator"),
            function: query.capture_index_for_name("function"),
            class: query.capture_index_for_name("class"),
            route_method: query.capture_index_for_name("route.method"),
            route_path: query.capture_index_for_name("route.path"),
            route_call: query.capture_index_for_name("route.call"),
            fastapi_depends_target: query.capture_index_for_name("fastapi.depends.target"),
            fastapi_route_app: query.capture_index_for_name("fastapi.route.app"),
            fastapi_route_method: query.capture_index_for_name("fastapi.route.method"),
            fastapi_route_handler: query.capture_index_for_name("fastapi.route.handler"),
            django_url_handler: query.capture_index_for_name("django.url.handler"),
            django_signal_receiver_name: query
                .capture_index_for_name("django.signal.receiver_name"),
            django_signal_receiver_handler: query
                .capture_index_for_name("django.signal.receiver_handler"),
            django_signal_connect_name: query.capture_index_for_name("django.signal.connect_name"),
            django_signal_connect_handler: query
                .capture_index_for_name("django.signal.connect_handler"),
            celery_task_handler: query.capture_index_for_name("celery.task.handler"),
            tx_atomic_handler: query.capture_index_for_name("tx.atomic.handler"),
            tx_db_session_handler: query.capture_index_for_name("tx.db_session.handler"),
            reflection_getattr_site: query.capture_index_for_name("reflection.getattr.site"),
            blind_eval: query.capture_index_for_name("blind.eval"),
            blind_exec: query.capture_index_for_name("blind.exec"),
            blind_compile: query.capture_index_for_name("blind.compile"),
            blind_dynamic_import: query.capture_index_for_name("blind.dynamic_import"),
            blind_builtin_import: query.capture_index_for_name("blind.builtin_import"),
            blind_cross_getattr: query.capture_index_for_name("blind.cross_getattr"),
        };
        Ok(Self {
            query,
            indices,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for PythonProvider {
    fn name(&self) -> &'static str {
        "python"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let idx = &self.indices;

        // Pre-scan pass: walk the same query once just to populate `imports`.
        // We then compute framework-presence flags up-front so the main pass
        // can short-circuit framework-specific captures the moment they fire
        // (skip ref construction + push entirely, not just the final extend).
        let mut imports: Vec<RawImport> = Vec::new();
        {
            let mut pre_cursor = QueryCursor::new();
            let mut pre_matches = pre_cursor.matches(&self.query, tree.root_node(), source);
            while let Some(m) = pre_matches.next() {
                let mut import_name_node = None;
                let mut import_src_node = None;
                let mut import_alias_node = None;
                for cap in m.captures {
                    let cap_idx = Some(cap.index);
                    if cap_idx == idx.import_name {
                        import_name_node = Some(cap.node);
                    } else if cap_idx == idx.import_source {
                        import_src_node = Some(cap.node);
                    } else if cap_idx == idx.import_alias {
                        import_alias_node = Some(cap.node);
                    }
                }
                if let Some(i_name) = import_name_node {
                    if let Ok(name_str) =
                        std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()])
                    {
                        let src_str = if let Some(i_src) = import_src_node {
                            std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                                .unwrap_or("")
                                .to_string()
                        } else {
                            "".to_string()
                        };
                        let alias = import_alias_node.and_then(|a| {
                            std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        });
                        imports.push(RawImport {
                            alias,
                            imported_name: name_str.to_string(),
                            source: src_str,
                            binding_kind: None,
                        });
                    }
                }
            }
        }

        let has_fastapi = has_import_from(&imports, FASTAPI_REQUIRED);
        let has_django = has_import_from(&imports, DJANGO_REQUIRED);
        let has_celery = has_import_from(&imports, CELERY_REQUIRED);
        let has_any_http_framework = has_import_from(&imports, HTTP_FRAMEWORK_MODULES);
        // Path-conditional flag for the test-file-only direct-receiver
        // filter. `is_test_path` matches `tests/` / `test_*.py` / `conftest.`
        // / `*_test.py` / `_spec.` and the rest of the FileCategory::Test
        // patterns. Production files keep their permissive emission rules.
        let file_is_test = is_test_path(&path.to_string_lossy());

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut routes: Vec<RawRoute> = Vec::new();
        let mut blind_spots: Vec<BlindSpot> = Vec::new();

        // Collect (target_name, span) for FastAPI Depends() refs; resolve
        // the enclosing function via span containment after nodes are built.
        let mut pending_depends: Vec<(String, (u32, u32, u32, u32))> = Vec::new();

        // Per-framework pending refs. Emitted only if the file imports the
        // matching framework — see gates below.
        let mut pending_fastapi_refs: Vec<RawFrameworkRef> = Vec::new();
        let mut pending_django_refs: Vec<RawFrameworkRef> = Vec::new();
        let mut pending_celery_refs: Vec<RawFrameworkRef> = Vec::new();

        // Reflection fan-out sites: outer `getattr(self, name)()` call spans.
        // Resolved after `nodes` is populated (need enclosing class + sibling methods).
        let mut pending_getattr_sites: Vec<Span> = Vec::new();

        // Pending transaction-scope decorator hits. Each entry is
        // `(handler_name_start_row, handler_name_start_col, framework)`.
        // Resolved against `nodes` after the match loop closes — we need
        // to find the Function/Method/Constructor whose body span contains
        // this position to record the `node_idx` in `RawTxScope::new`.
        let mut pending_tx_scopes: Vec<(u32, u32, FrameworkId)> = Vec::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut type_annotation_node = None;
            let mut heritage = Vec::new();
            let mut is_exported_explicit = false;
            let mut decorators = Vec::new();

            let mut route_method = None;
            let mut route_path = None;
            let mut route_call_node: Option<Node> = None;
            let mut is_route = false;

            let mut fa_route_app_node = None;
            let mut fa_route_method_node = None;
            let mut fa_route_handler_node = None;

            let mut dj_recv_name_node = None;
            let mut dj_recv_handler_node = None;
            let mut dj_connect_name_node = None;
            let mut dj_connect_handler_node = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap.index as usize)
                    .copied()
                    .flatten()
                {
                    // Single spec-driven dispatch replaces the four explicit
                    // Function/Class/Property/Variable name-capture arms.
                    // Source of truth: PythonSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if cap_idx == idx.type_ann {
                    type_annotation_node = Some(cap.node);
                } else if cap_idx == idx.heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if cap_idx == idx.export {
                    is_exported_explicit = true;
                } else if cap_idx == idx.decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if cap_idx == idx.import_name
                    || cap_idx == idx.import_source
                    || cap_idx == idx.import_alias
                {
                    // Already collected in the pre-scan pass; nothing to do here.
                } else if cap_idx == idx.route_method {
                    route_method = Some(cap.node);
                } else if cap_idx == idx.route_path {
                    route_path = Some(cap.node);
                } else if cap_idx == idx.route_call {
                    is_route = true;
                    root_span_node = Some(cap.node);
                    route_call_node = Some(cap.node);
                } else if cap_idx == idx.function
                    || cap_idx == idx.class
                    || cap_idx == idx.property
                    || cap_idx == idx.variable
                {
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx.fastapi_depends_target {
                    if !has_fastapi {
                        continue;
                    }
                    if let Ok(target_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        pending_depends.push((target_name.to_string(), node_span(&cap.node)));
                    }
                } else if cap_idx == idx.fastapi_route_app {
                    if !has_fastapi {
                        continue;
                    }
                    fa_route_app_node = Some(cap.node);
                } else if cap_idx == idx.fastapi_route_method {
                    if !has_fastapi {
                        continue;
                    }
                    fa_route_method_node = Some(cap.node);
                } else if cap_idx == idx.fastapi_route_handler {
                    if !has_fastapi {
                        continue;
                    }
                    fa_route_handler_node = Some(cap.node);
                } else if cap_idx == idx.django_url_handler {
                    if !has_django {
                        continue;
                    }
                    if let Ok(target_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        pending_django_refs.push(RawFrameworkRef {
                            source_name: MODULE_LEVEL_SOURCE.to_string(),
                            target_name: target_name.to_string(),
                            confidence: framework_confidence::DJANGO_URL,
                            reason: "django-url-path".to_string(),
                            span: node_span(&cap.node),
                        });
                    }
                } else if cap_idx == idx.django_signal_receiver_name {
                    if !has_django {
                        continue;
                    }
                    dj_recv_name_node = Some(cap.node);
                } else if cap_idx == idx.django_signal_receiver_handler {
                    if !has_django {
                        continue;
                    }
                    dj_recv_handler_node = Some(cap.node);
                } else if cap_idx == idx.django_signal_connect_name {
                    if !has_django {
                        continue;
                    }
                    dj_connect_name_node = Some(cap.node);
                } else if cap_idx == idx.django_signal_connect_handler {
                    if !has_django {
                        continue;
                    }
                    dj_connect_handler_node = Some(cap.node);
                } else if cap_idx == idx.celery_task_handler {
                    if !has_celery {
                        continue;
                    }
                    if let Ok(target_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        pending_celery_refs.push(RawFrameworkRef {
                            source_name: MODULE_LEVEL_SOURCE.to_string(),
                            target_name: target_name.to_string(),
                            confidence: framework_confidence::CELERY_TASK,
                            reason: "celery-task".to_string(),
                            span: node_span(&cap.node),
                        });
                    }
                } else if cap_idx == idx.tx_atomic_handler || cap_idx == idx.tx_db_session_handler {
                    let framework = if cap_idx == idx.tx_atomic_handler {
                        FrameworkId::DjangoAtomic
                    } else {
                        FrameworkId::PonyDbSession
                    };
                    let pos = cap.node.start_position();
                    pending_tx_scopes.push((pos.row as u32, pos.column as u32, framework));
                } else if cap_idx == idx.reflection_getattr_site {
                    pending_getattr_sites.push(node_span(&cap.node));
                } else {
                    // Blind-spot dispatch: one row per kind, no per-arm boilerplate.
                    let blind_match: Option<(&str, &str)> = if cap_idx == idx.blind_eval {
                        Some(BLIND_SPEC[0])
                    } else if cap_idx == idx.blind_exec {
                        Some(BLIND_SPEC[1])
                    } else if cap_idx == idx.blind_compile {
                        Some(BLIND_SPEC[2])
                    } else if cap_idx == idx.blind_dynamic_import {
                        Some(BLIND_SPEC[3])
                    } else if cap_idx == idx.blind_builtin_import {
                        Some(BLIND_SPEC[4])
                    } else if cap_idx == idx.blind_cross_getattr {
                        Some(BLIND_SPEC[5])
                    } else {
                        None
                    };
                    if let Some((kind, hint)) = blind_match {
                        blind_spots.push(BlindSpot {
                            kind: kind.to_string(),
                            file_path: path.to_path_buf(),
                            span: node_span(&cap.node),
                            hint: hint.to_string(),
                        });
                    }
                }
            }

            if let (Some(app_n), Some(method_n), Some(handler_n)) = (
                fa_route_app_node,
                fa_route_method_node,
                fa_route_handler_node,
            ) {
                if let (Ok(app_str), Ok(method_str), Ok(handler_str)) = (
                    std::str::from_utf8(&source[app_n.start_byte()..app_n.end_byte()]),
                    std::str::from_utf8(&source[method_n.start_byte()..method_n.end_byte()]),
                    std::str::from_utf8(&source[handler_n.start_byte()..handler_n.end_byte()]),
                ) {
                    pending_fastapi_refs.push(RawFrameworkRef {
                        source_name: app_str.to_string(),
                        target_name: handler_str.to_string(),
                        confidence: framework_confidence::FASTAPI_ROUTE,
                        reason: format!("fastapi-route-{}", method_str),
                        span: node_span(&handler_n),
                    });
                }
            }

            if let (Some(sig_n), Some(handler_n)) = (dj_recv_name_node, dj_recv_handler_node) {
                push_django_signal_ref(
                    sig_n,
                    handler_n,
                    source,
                    "django-signal-receiver",
                    &mut pending_django_refs,
                );
            }

            if let (Some(sig_n), Some(handler_n)) = (dj_connect_name_node, dj_connect_handler_node)
            {
                push_django_signal_ref(
                    sig_n,
                    handler_n,
                    source,
                    "django-signal-connect",
                    &mut pending_django_refs,
                );
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                // Module-level guard: Variable captures fire for every
                // expression_statement in any scope. Walk the parent chain
                // from the expression_statement node; only emit when the
                // immediate parent is `module`. Any intervening
                // `class_definition`, `function_definition`, `if_statement`,
                // `with_statement`, `try_statement`, `for_statement`,
                // `while_statement` — or any other block-introducing node —
                // means this is NOT a module-level binding.
                // No ancestor filter: emit every assignment that matches the
                // query. Lets downstream consumers decide what's a true
                // module-level vs class-attr vs local — we don't pre-classify.
                // (Round 2 already split class-attrs into Property via a
                // distinct query, so this Variable capture mostly fires for
                // non-class scopes.)
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let span = node_span(&root);

                    let type_str = type_annotation_node.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });

                    let final_kind = if k == NodeKind::Function && is_class_method(root) {
                        NodeKind::Method
                    } else {
                        k
                    };
                    // `__init__` is Python's constructor convention; the spec
                    // table maps class methods to Method, so promote here.
                    let final_kind = if final_kind == NodeKind::Method && name_str == "__init__" {
                        NodeKind::Constructor
                    } else {
                        final_kind
                    };
                    if let Some(existing) = nodes.iter_mut().find(|node| node.span == span) {
                        if existing.kind == NodeKind::Function && final_kind == NodeKind::Method {
                            existing.kind = NodeKind::Method;
                        }
                        if final_kind == NodeKind::Constructor {
                            existing.kind = NodeKind::Constructor;
                        }
                        for h in heritage {
                            if !existing.heritage.contains(&h) {
                                existing.heritage.push(h);
                            }
                        }
                        if existing.type_annotation.is_none() && type_str.is_some() {
                            existing.type_annotation = type_str;
                        }
                        if !decorators.is_empty() {
                            for d in &decorators {
                                if !existing.decorators.contains(d) {
                                    existing.decorators.push(d.clone());
                                }
                            }
                        }
                    } else {
                        nodes.push(RawNode {
                            decorators: decorators.clone(),
                            is_exported: is_exported_explicit || !name_str.starts_with('_'),
                            heritage,
                            type_annotation: type_str,
                            name: name_str.to_string(),
                            kind: final_kind,
                            span,
                            calls: Vec::new(),
                            owner_class: None,
                            content_hash: ecp_core::uid::xxh3_64_bytes(
                                &source[root.start_byte()..root.end_byte()],
                            ),
                        });
                    }
                }
            }

            // Imports were collected up-front in the pre-scan pass — see top of
            // parse_file. Skipping a duplicate populate here keeps `imports` as
            // the single source of truth and lets the framework gates run before
            // any pending_*_refs push.

            // Framework-presence gate, with a relaxation for route-registration
            // methods (`route`, `add_route`, `add_url_rule`, `add_api_route`).
            // These method names are sufficiently framework-specific that a
            // bare `@bp.route(...)` in a file that does `from app.api import bp`
            // (transitive Flask Blueprint — common pattern in real Flask apps,
            // e.g. miguelgrinberg/microblog `app/api/tokens.py`) still gets
            // emitted even though ecp can't statically follow the chain to
            // confirm `bp` is a `Blueprint`.
            //
            // Defense-in-depth: the relaxation also requires the file to have
            // AT LEAST ONE import. A self-contained script that defines its
            // own `class CustomRouter { def route(...) }` and calls it inline
            // has zero imports — that's almost certainly not a web framework
            // and the relaxation would FP. Files using a Blueprint always
            // import the blueprint identifier, so the recall path is
            // preserved.
            let route_method_is_framework_specific = route_method
                .and_then(|n| std::str::from_utf8(&source[n.start_byte()..n.end_byte()]).ok())
                .map(|s| REGISTRATION_METHOD_NAMES.contains(&s.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
                && !imports.is_empty();
            // Decorator context: `@bp.get("/path")` / `@router.post(...)` —
            // Flask Blueprint shorthand and FastAPI's APIRouter shorthand both
            // expose HTTP-verb methods directly on the route-registration
            // object. The method names (`get`/`post`/`put`/...) collide with
            // `dict.get` and `requests.post` so we can't gate by method alone;
            // but a `@expr(...)` decorator never wraps a dict access, so this
            // is a safe disambiguator. Required for test files like
            // `tests/test_apps/blueprintapp/apps/admin/__init__.py` that
            // import the blueprint via `from . import bp` without a direct
            // `from flask import` — `has_any_http_framework` returns false
            // there even though the decorator is unambiguously a route.
            let route_is_decorator_call = route_call_node.map(is_in_decorator).unwrap_or(false);
            // Drop `<receiver>.test_client.X(...)` patterns up-front: these
            // are test-client REQUESTS, not route definitions. Tree-sitter
            // can't distinguish them by call shape; the `.test_client.`
            // substring in the chained function name is the cheapest reliable
            // signal. Empirical: removes ~88% of the Sanic `--include-tests`
            // FPs.
            if let Some(call_node) = route_call_node {
                if is_test_client_chained_call(call_node, source) {
                    continue;
                }
                // Path-conditional filter: in test files, also drop calls
                // whose immediate receiver is a known test-client fixture
                // name (`http_client`, `client`, `api_client`, ...). These
                // are pytest fixtures that bind to HTTP clients — not
                // route registrations. Production files keep emitting on
                // these names because there `client = Blueprint(...)`
                // / `client = APIRouter()` is legitimate.
                if file_is_test && is_test_file_direct_receiver_call(call_node, source) {
                    continue;
                }
            }
            if is_route
                && (has_any_http_framework
                    || route_method_is_framework_specific
                    || route_is_decorator_call)
            {
                if let (Some(r_method), Some(r_path), Some(root)) =
                    (route_method, route_path, root_span_node)
                {
                    if let (Ok(method_str), Ok(path_str)) = (
                        std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                        std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
                    ) {
                        // For route-registration methods (see
                        // `REGISTRATION_METHOD_NAMES`), framework convention
                        // (Sanic, Flask) accepts both `'path'` and `'/path'` —
                        // semantically `/path`. Normalize bare paths so the
                        // builder's `looks_like_path` filter doesn't drop them
                        // (mirrors PHP/Laravel bare-path handling).
                        let method_lower = method_str.to_ascii_lowercase();
                        let is_registration_method =
                            REGISTRATION_METHOD_NAMES.contains(&method_lower.as_str());
                        let resolved_path = if is_registration_method {
                            crate::route_detector::clean_route_path_lax(path_str)
                        } else {
                            crate::route_detector::clean_route_path(path_str)
                        };
                        if let Some(clean_path) = resolved_path {
                            // Registration methods don't encode the HTTP verb;
                            // default to GET when no `methods=[...]` kwarg is
                            // supplied, otherwise parse the kwarg. P1 review
                            // fix for PR #50: previously translated unconditionally
                            // to GET, fabricating data when `methods=["POST"]`
                            // was present.
                            let methods_to_emit: Vec<String> = if is_registration_method {
                                match route_call_node.and_then(|n| extract_methods_kwarg(n, source))
                                {
                                    None => vec!["GET".to_string()],
                                    Some(methods) if !methods.is_empty() => methods,
                                    Some(_) => {
                                        // kwarg present but unparseable — skip
                                        // rather than fabricate a method.
                                        continue;
                                    }
                                }
                            } else {
                                vec![method_str.to_string()]
                            };
                            // Decorator-style routes (`@router.post("/x")`)
                            // carry the handler one parent up — the decorated
                            // function/class name. Imperative-style routes
                            // (`app.add_url_rule(path, handler)`) leave this
                            // as `None`; builder-side symbol-table lookup
                            // takes over there if/when the parser captures
                            // the handler arg.
                            let decorator_handler =
                                route_call_node.and_then(|n| resolve_decorator_handler(n, source));
                            for method in methods_to_emit {
                                routes.push(RawRoute {
                                    method,
                                    path: clean_path.clone(),
                                    handler: decorator_handler.clone(),
                                    span: node_span(&root),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Extract call sites with receiver-type binding. Replaces the shared
        // `extract_calls` for Python so `x.method()` can be rewritten to
        // `Type.method` when `x`'s type is known from a local annotation
        // (typed param or annotated assignment) — fed back into the resolver's
        // Tier 2.5 qualifier-scoped lookup. Falls back to bare member name
        // when no annotation is in scope.
        let local_types = collect_local_types(tree.root_node(), source);
        let raw_path_literals = extract_python_calls_and_path_literals(
            tree.root_node(),
            source,
            &mut nodes,
            &local_types,
        );

        let param_names = collect_python_param_names(tree.root_node(), source);
        let call_metas = detect_python_indirect(tree.root_node(), source, &nodes, &param_names);

        // Resolve FastAPI Depends() refs: find the innermost enclosing
        // Function/Method node whose span contains the capture span. The site
        // capture itself was gated by `has_fastapi` in the main loop, so no
        // additional gate is needed here.
        for (target_name, span) in pending_depends {
            if let Some(source_name) = enclosing_function_name(&nodes, span) {
                pending_fastapi_refs.push(RawFrameworkRef {
                    source_name,
                    target_name,
                    confidence: framework_confidence::FASTAPI_DEPENDS,
                    reason: "fastapi-depends".to_string(),
                    span,
                });
            }
        }

        // pending_*_refs were only populated when the matching framework gate
        // was satisfied, so we can merge unconditionally here.
        let mut framework_refs: Vec<RawFrameworkRef> = Vec::new();
        framework_refs.extend(pending_fastapi_refs);
        framework_refs.extend(pending_django_refs);
        framework_refs.extend(pending_celery_refs);

        // Resolve reflection-getattr fan-out sites: enclosing method (source)
        // dispatches to any sibling method on the same class. Skip sites with
        // no enclosing class (module-level) or no enclosing fn (defensive).
        let mut fanout_refs: Vec<RawFanoutRef> = Vec::new();
        for span in pending_getattr_sites {
            let Some(source_name) = enclosing_function_name(&nodes, span) else {
                continue;
            };
            let Some((_class_name, class_span)) = enclosing_class(&nodes, span) else {
                continue;
            };
            let candidates = enumerate_class_methods(&nodes, class_span, &source_name);
            if candidates.is_empty() {
                continue;
            }
            fanout_refs.push(RawFanoutRef {
                source_name,
                candidates,
                base_confidence: framework_confidence::FANOUT_BASE,
                reason: "reflection-getattr-fanout".to_string(),
                span,
            });
        }

        // Dedupe routes by (method, path, span). The generic tree-sitter query
        // `arguments: (argument_list (string) @route.path)` matches EVERY string
        // child of the argument list — so `app.add_url_rule("/path", "endpoint",
        // handler)` fires twice (once for "/path", once for "endpoint"). The
        // endpoint name normally fails the strict `clean_route_path` filter
        // (no leading `/`), but for registration methods we use the lax variant
        // which prepends `/` and would emit `/endpoint` as a duplicate of
        // `/path` after both normalize. Dedupe is the simplest fix without
        // changing the query (which would break Sanic's
        // `add_route(handler, "/path")` arg-order variant).
        routes.sort_by(|a, b| {
            a.method
                .cmp(&b.method)
                .then(a.path.cmp(&b.path))
                .then(a.span.cmp(&b.span))
        });
        routes.dedup_by(|a, b| a.method == b.method && a.path == b.path && a.span == b.span);

        // Python pytest convention: files named `test_*.py` or `*_test.py` are test files.
        // `is_test_path` requires a `/test` dir prefix; supplement for bare filenames.
        let basename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let is_py_test_file = file_is_test
            || basename.starts_with("test_")
            || basename.ends_with("_test.py")
            || basename == "conftest.py";
        let file_category = if is_py_test_file {
            FileCategory::Test
        } else {
            FileCategory::Source
        };
        let raw_function_metas =
            crate::function_meta::python::extract(tree.root_node(), source, &nodes, file_category);

        let tx_scopes = resolve_tx_scopes(&nodes, &pending_tx_scopes);

        // §4.5b: promote Class → Interface when ALL bases are Protocol-markers.
        // Runs after the match loop so each node's heritage is fully assembled
        // (tree-sitter emits one capture per base expression, so per-match
        // classification would misclassify mixed-base classes on first match).
        for node in &mut nodes {
            if node.kind == NodeKind::Class && is_protocol_marker_only(&node.heritage) {
                node.kind = NodeKind::Interface;
            }
        }

        crate::framework_helpers::stamp_owner_class_by_span(&mut nodes);
        crate::framework_helpers::stamp_owner_fn_by_span(&mut nodes);

        // T4-7 refactor: `RawSchemaField` now stores owned `Box<str>` so the
        // per-file parser scope can drop cleanly without dangling-pool risk.
        let fields = crate::schema_field::extract_schema_fields(
            &tree,
            source,
            &self.query,
            &[
                crate::python::schema_extractors::PYDANTIC_CONFIG,
                crate::python::schema_extractors::SQLALCHEMY_CONFIG,
            ],
            &imports,
        );
        let schema_fields = (!fields.is_empty()).then(|| fields.into_boxed_slice());

        let event_topics = {
            let topics = crate::event_topic::extract_event_topics(
                &tree,
                source,
                &self.query,
                &[
                    crate::event_topic::KAFKA_PYTHON,
                    crate::event_topic::CELERY_PYTHON,
                    crate::event_topic::REDIS_PYTHON,
                    crate::event_topic::RABBITMQ_PYTHON,
                    crate::event_topic::SQS_PYTHON,
                ],
                &imports,
            );
            (!topics.is_empty()).then(|| topics.into_boxed_slice())
        };

        let path_literals =
            (!raw_path_literals.is_empty()).then(|| raw_path_literals.into_boxed_slice());

        Ok(LocalGraph {
            content_hash: [0; 8],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs,
            blind_spots,
            schema_fields,
            event_topics,
            tx_scopes,
            path_literals,
            call_metas,
            raw_function_metas,
        })
    }
}

/// Match each pending `(handler_row, handler_col, framework)` against the
/// Function/Method/Constructor whose span contains that position and emit a
/// packed `RawTxScope`. The captured identifier is `name: (identifier)` of a
/// `function_definition`, so the handler position falls inside the function
/// node's span (which is the surrounding `function_definition`).
///
/// `pending_tx_scopes` is typically small (<10 per file); a linear scan is
/// faster than building a HashMap below ~50 entries due to hash overhead.
fn resolve_tx_scopes(
    nodes: &[RawNode],
    pending_tx_scopes: &[(u32, u32, FrameworkId)],
) -> Option<Box<[RawTxScope]>> {
    if pending_tx_scopes.is_empty() {
        return None;
    }
    let scopes: Vec<RawTxScope> = pending_tx_scopes
        .iter()
        .filter_map(|&(row, col, framework)| {
            nodes
                .iter()
                .enumerate()
                .find(|(_, n)| {
                    matches!(
                        n.kind,
                        NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                    ) && point_in_span(n.span, row, col)
                })
                .map(|(idx, _)| RawTxScope::new(idx as u32, framework))
        })
        .collect();
    (!scopes.is_empty()).then(|| scopes.into_boxed_slice())
}
