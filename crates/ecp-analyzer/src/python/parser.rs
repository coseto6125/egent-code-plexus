use super::receiver_types::{collect_local_types, extract_python_calls_and_path_literals};
use super::schema_extractors::{PYDANTIC_CONFIG, SQLALCHEMY_CONFIG};
use super::spec::PythonSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture,
    Op, ParseSpec, Step, TestRule, TxRule,
};
use crate::event_topic::{
    EventTopicConfig, CELERY_PYTHON, KAFKA_PYTHON, RABBITMQ_PYTHON, REDIS_PYTHON, SQS_PYTHON,
};
use crate::framework_confidence;
use crate::framework_helpers::{
    enclosing_class, enclosing_function_name, enumerate_class_methods, has_import_from, node_span,
    push_blind_spot, Span, MODULE_LEVEL_SOURCE,
};
use crate::indirect_dispatch::{collect_python_param_names, detect_python_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use crate::schema_field::SchemaFieldConfig;
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    BlindSpot, FrameworkId, LocalGraph, RawFanoutRef, RawFrameworkRef, RawFunctionMeta, RawImport,
    RawNode, RawRoute,
};
use ecp_core::graph::{FileCategory, NodeKind};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

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

/// The identifier a call is made on: `router` in `router.get("/x")`; `None`
/// for any other receiver (`self.router`, a call result) or a bare call.
fn call_receiver(call: Node, source: &[u8]) -> Option<String> {
    let object = call
        .child_by_field_name("function")?
        .child_by_field_name("object")?;
    (object.kind() == "identifier").then(|| node_text(object, source).to_string())
}

fn node_text<'s>(node: Node, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// The value of keyword argument `key` when it is a plain string literal:
/// not an f-string with a placeholder, a name, a list or a concatenation.
fn keyword_string<'s>(call: Node, source: &'s [u8], key: &str) -> Option<&'s str> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let value = args
        .children(&mut cursor)
        .filter(|arg| arg.kind() == "keyword_argument")
        .find(|arg| {
            arg.child_by_field_name("name")
                .is_some_and(|name| node_text(name, source) == key)
        })?
        .child_by_field_name("value")?;
    if value.kind() != "string" {
        return None;
    }
    let mut children = value.walk();
    if value
        .children(&mut children)
        .any(|child| child.kind() == "interpolation")
    {
        return None;
    }
    crate::framework_helpers::strip_python_string_quotes(node_text(value, source))
}

/// The FastAPI routers of one file and what includes them.
#[derive(Default)]
struct RouterPrefixes {
    /// `router = APIRouter(prefix="/items")`: router variable → its prefix
    /// (`""` for a router built with no literal prefix).
    own: std::collections::HashMap<String, String>,
    /// `parent.include_router(child, prefix="/v1")`: `(parent, child, prefix)`.
    includes: Vec<(String, String, String)>,
}

/// The most prefixes one router takes from its includes: a cap against a
/// router that a cycle of includes reaches again.
const MAX_ROUTER_PREFIXES: usize = 16;

impl RouterPrefixes {
    fn collect(root: Node, source: &[u8]) -> Self {
        let mut found = Self::default();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
            match node.kind() {
                "assignment" => {
                    let (Some(left), Some(right)) = (
                        node.child_by_field_name("left"),
                        node.child_by_field_name("right"),
                    ) else {
                        continue;
                    };
                    let is_router = right.kind() == "call"
                        && right.child_by_field_name("function").is_some_and(|callee| {
                            node_text(callee, source).rsplit('.').next() == Some("APIRouter")
                        });
                    if left.kind() == "identifier" && is_router {
                        let prefix = keyword_string(right, source, "prefix").unwrap_or("");
                        found
                            .own
                            .insert(node_text(left, source).to_string(), prefix.to_string());
                    }
                }
                "call" => {
                    let Some(function) = node.child_by_field_name("function") else {
                        continue;
                    };
                    let is_include = function.kind() == "attribute"
                        && function
                            .child_by_field_name("attribute")
                            .is_some_and(|name| node_text(name, source) == "include_router");
                    let child = node
                        .child_by_field_name("arguments")
                        .and_then(|args| args.named_child(0))
                        .filter(|arg| arg.kind() == "identifier");
                    let parent = function
                        .child_by_field_name("object")
                        .filter(|object| object.kind() == "identifier");
                    if let (true, Some(child), Some(parent)) = (is_include, child, parent) {
                        let prefix = keyword_string(node, source, "prefix").unwrap_or("");
                        found.includes.push((
                            node_text(parent, source).to_string(),
                            node_text(child, source).to_string(),
                            prefix.to_string(),
                        ));
                    }
                }
                _ => {}
            }
        }
        found
    }

    /// The prefixes of `router`, one per chain of includes that reaches it,
    /// outermost include first and its own prefix last, joined by the one
    /// prefix rule. A router no include in this file attaches has its own
    /// prefix alone (`""` for a name that is no router of the file).
    /// `capped` is set when [`MAX_ROUTER_PREFIXES`] left a prefix out.
    fn of<'a>(
        &'a self,
        router: &'a str,
        chain: &mut Vec<&'a str>,
        capped: &mut bool,
    ) -> Vec<String> {
        let own = self.own.get(router).map_or("", String::as_str);
        let parents: Vec<_> = self
            .includes
            .iter()
            .filter(|(_, child, _)| child == router)
            .collect();
        if parents.is_empty() {
            return vec![own.to_string()];
        }
        chain.push(router);
        let mut prefixes: Vec<String> = Vec::new();
        for (parent, _, include_prefix) in parents {
            let parent = parent.as_str();
            if chain.contains(&parent) {
                continue;
            }
            for outer in self.of(parent, chain, capped) {
                let prefix = crate::route_detector::join_route_segments([
                    outer.as_str(),
                    include_prefix.as_str(),
                    own,
                ]);
                if prefixes.contains(&prefix) {
                    continue;
                }
                match prefixes.len() < MAX_ROUTER_PREFIXES {
                    true => prefixes.push(prefix),
                    false => *capped = true,
                }
            }
        }
        chain.pop();
        if prefixes.is_empty() {
            prefixes.push(own.to_string());
        }
        prefixes
    }
}

/// Compose FastAPI router prefixes onto `routes`; `receivers[i]` is the
/// variable `routes[i]` is registered on. A prefix is the router's own
/// `APIRouter(prefix="/items")` and the `include_router(router, prefix=)`
/// calls of this file that attach it. A router that another file includes
/// keeps its own path here: the include is not in this file. A router
/// included twice gives a record per include, as FastAPI registers it twice.
fn compose_router_prefixes(
    root: Node,
    source: &[u8],
    routes: &mut Vec<RawRoute>,
    receivers: &[Option<String>],
) {
    if receivers.iter().all(Option::is_none) {
        return;
    }
    let prefixes = RouterPrefixes::collect(root, source);
    if prefixes.own.is_empty() && prefixes.includes.is_empty() {
        return;
    }
    let mut extra: Vec<RawRoute> = Vec::new();
    for (route, receiver) in routes.iter_mut().zip(receivers) {
        let Some(receiver) = receiver.as_deref() else {
            continue;
        };
        let mut capped = false;
        let mut paths = prefixes
            .of(receiver, &mut Vec::new(), &mut capped)
            .into_iter()
            .map(|prefix| match prefix.trim().trim_matches('/').is_empty() {
                true => route.path.clone(),
                false => crate::route_detector::join_route_segments([
                    prefix.as_str(),
                    route.path.as_str(),
                ]),
            })
            .collect::<Vec<_>>();
        let mut unique: Vec<String> = Vec::new();
        for path in paths.drain(..) {
            if !unique.contains(&path) {
                unique.push(path);
            }
        }
        // Past the cap some includes are not composed: the route stays at its
        // own path instead of being dropped.
        if capped && !unique.contains(&route.path) {
            unique.push(route.path.clone());
        }
        let mut paths = unique.into_iter();
        if let Some(first) = paths.next() {
            for path in paths {
                extra.push(RawRoute {
                    path,
                    ..route.clone()
                });
            }
            route.path = first;
        }
    }
    routes.extend(extra);
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
    compiled: Compiled<PythonSpec>,
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

/// Stdlib base classes that mark a class as a Python Enum. Dotted forms
/// (`enum.Enum`, `enum.IntEnum`, …) also count — matched on trailing identifier.
const ENUM_BASE_CLASSES: &[&str] = &["Enum", "IntEnum", "StrEnum", "Flag", "IntFlag", "ReprEnum"];

/// True when any string in `bases` ends with one of `ENUM_BASE_CLASSES`.
/// Handles bare (`Enum`) and dotted (`enum.Enum`) forms by splitting on `.`
/// and checking the last segment.
fn has_enum_base(bases: &[String]) -> bool {
    bases.iter().any(|base| {
        // Strip subscript suffix: `Enum[T]` → `Enum`
        let stripped = base
            .find('[')
            .map(|i| &base[..i])
            .unwrap_or(base.as_str())
            .trim();
        // Take only the last dotted segment: `enum.IntEnum` → `IntEnum`
        let tail = stripped.rsplit('.').next().unwrap_or(stripped);
        ENUM_BASE_CLASSES.contains(&tail)
    })
}

/// True when `node` is a scalar literal eligible as an enum variant value:
/// integer, float, string, boolean, None, or a unary-negated number.
fn is_scalar_literal(node: Node) -> bool {
    match node.kind() {
        "integer" | "float" | "string" | "true" | "false" | "none" => true,
        // `-1`, `-0.5` — unary negation of a numeric literal
        "unary_operator" => {
            let mut cursor = node.walk();
            let result = node
                .children(&mut cursor)
                .any(|c| matches!(c.kind(), "integer" | "float"));
            result
        }
        "tuple" => {
            // Allow tuples of scalars: `A = (1, "x")`
            let mut cursor = node.walk();
            let children: Vec<_> = node
                .children(&mut cursor)
                .filter(|c| c.is_named())
                .collect();
            children.into_iter().all(is_scalar_literal)
        }
        _ => false,
    }
}

/// Walk a `class_definition` node and emit `EnumVariant` nodes or a
/// `python-class-as-enum` BlindSpot, depending on whether the class inherits
/// from an Enum base.
///
/// - **Track A** (true Enum): for each `name = value` assignment in the class
///   body where the LHS is a bare identifier, emit one `NodeKind::EnumVariant`
///   with `owner_class` set to the class name. Methods in an Enum class stay
///   as Method (already emitted by the main query loop).
///
/// - **Track B** (imitation): when the class does NOT have an Enum base, has
///   ≥2 ALL_UPPERCASE name assignments with scalar RHS, and has no
///   `function_definition` anywhere in its direct body, push one BlindSpot.
fn process_class_for_enum(
    class_node: Node,
    source: &[u8],
    heritage: &[String],
    path: &std::path::Path,
    is_test_file: bool,
    nodes: &mut Vec<RawNode>,
    blind_spots: &mut Vec<BlindSpot>,
) {
    let Some(name_node) = class_node.child_by_field_name("name") else {
        return;
    };
    let Ok(class_name) = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
    else {
        return;
    };
    let Some(body) = class_node.child_by_field_name("body") else {
        return;
    };

    let is_enum = has_enum_base(heritage);

    if is_enum {
        // Track A: emit EnumVariant for each bare `NAME = value` assignment.
        let mut body_cursor = body.walk();
        for stmt in body.children(&mut body_cursor) {
            // Assignments appear as `expression_statement` wrapping `assignment`
            if stmt.kind() != "expression_statement" {
                continue;
            }
            let Some(assign) = stmt.named_child(0) else {
                continue;
            };
            if assign.kind() != "assignment" {
                continue;
            }
            let Some(lhs) = assign.child_by_field_name("left") else {
                continue;
            };
            if lhs.kind() != "identifier" {
                continue;
            }
            let Ok(var_name) = std::str::from_utf8(&source[lhs.start_byte()..lhs.end_byte()])
            else {
                continue;
            };
            // Skip dunder assignments like `__order__` or `_ignore_`
            if var_name.starts_with('_') {
                continue;
            }
            // The existing Property node uses the `assignment` node span (mirrors
            // how `@property` is captured in queries.scm). Match on that span so
            // we can upgrade Property → EnumVariant in-place.
            let assign_start = assign.start_position();
            let assign_end = assign.end_position();
            let assign_span = (
                assign_start.row as u32,
                assign_start.column as u32,
                assign_end.row as u32,
                assign_end.column as u32,
            );
            let lhs_start = lhs.start_position();
            let lhs_end = lhs.end_position();
            let lhs_span = (
                lhs_start.row as u32,
                lhs_start.column as u32,
                lhs_end.row as u32,
                lhs_end.column as u32,
            );
            // Upgrade an existing Property node (emitted by the main query loop) to
            // EnumVariant, or push a new node if none exists at this span.
            if let Some(existing) = nodes
                .iter_mut()
                .find(|n| n.name == var_name && (n.span == assign_span || n.span == lhs_span))
            {
                existing.kind = NodeKind::EnumVariant;
                existing.owner_class = Some(class_name.to_string());
            } else {
                nodes.push(RawNode {
                    decorators: vec![],
                    is_exported: true,
                    heritage: vec![],
                    type_annotation: None,
                    name: var_name.to_string(),
                    kind: NodeKind::EnumVariant,
                    span: assign_span,
                    calls: vec![],
                    field_reads: Vec::new(),
                    owner_class: Some(class_name.to_string()),
                    content_hash: ecp_core::uid::xxh3_64_bytes(
                        &source[stmt.start_byte()..stmt.end_byte()],
                    ),
                });
            }
        }
    } else {
        // Track B: imitation detection.
        // Conditions: ≥2 ALL_UPPERCASE assignments with scalar RHS + no def.
        let mut uppercase_const_count: u32 = 0;
        let mut has_def = false;
        let mut body_cursor = body.walk();
        for stmt in body.children(&mut body_cursor) {
            if matches!(stmt.kind(), "function_definition" | "decorated_definition") {
                // Check if decorated_definition wraps a function_definition
                if stmt.kind() == "decorated_definition" {
                    if let Some(def) = stmt.child_by_field_name("definition") {
                        if def.kind() == "function_definition" {
                            has_def = true;
                            break;
                        }
                    }
                } else {
                    has_def = true;
                    break;
                }
            }
            if stmt.kind() != "expression_statement" {
                continue;
            }
            let Some(assign) = stmt.named_child(0) else {
                continue;
            };
            if assign.kind() != "assignment" {
                continue;
            }
            let Some(lhs) = assign.child_by_field_name("left") else {
                continue;
            };
            if lhs.kind() != "identifier" {
                continue;
            }
            let Ok(var_name) = std::str::from_utf8(&source[lhs.start_byte()..lhs.end_byte()])
            else {
                continue;
            };
            // ALL_UPPERCASE heuristic: every character is either uppercase ASCII or `_`,
            // and at least one char is alphabetic.
            let all_upper = var_name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                && var_name.chars().any(|c| c.is_ascii_alphabetic());
            if !all_upper {
                continue;
            }
            let Some(rhs) = assign.child_by_field_name("right") else {
                continue;
            };
            if is_scalar_literal(rhs) {
                uppercase_const_count += 1;
            }
        }
        if !has_def && uppercase_const_count >= 2 {
            const IMITATION_SPEC: (&str, &str) = (
                "python-class-as-enum",
                "class with \u{2265}2 constant assignments and no methods \u{2014} may be a \
                 pre-`enum.Enum` imitation that should inherit Enum; verify before treating as \
                 plain Class",
            );
            push_blind_spot(blind_spots, IMITATION_SPEC, &class_node, path, is_test_file);
        }
    }
}

impl PythonProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for PythonProvider {
    fn name(&self) -> &'static str {
        "python"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures Python handles itself. A root capture sets the declaration
/// root; the `.name` kind capture of the same match sets name and kind.
#[derive(Clone, Copy)]
pub enum PythonRole {
    Root,
    Type,
    Heritage,
    Decorator,
    ImportName,
    ImportSource,
    ImportAlias,
    RouteMethod,
    RoutePath,
    RouteCall,
    FastapiDependsTarget,
    FastapiRouteApp,
    FastapiRouteMethod,
    FastapiRouteHandler,
    DjangoUrlHandler,
    DjangoSignalReceiverName,
    DjangoSignalReceiverHandler,
    DjangoSignalConnectName,
    DjangoSignalConnectHandler,
    CeleryTaskHandler,
    Tx(FrameworkId),
    ReflectionGetattrSite,
}

#[derive(Clone, Copy)]
pub enum PythonHook {
    ImportPreScan,
    Calls,
    Indirect,
    Frameworks,
    ClassKinds,
    RootImports,
}

#[derive(Default)]
pub struct PythonMatch<'t> {
    type_annotation_node: Option<Node<'t>>,
    heritage: Vec<String>,
    decorators: Vec<String>,
    route_method: Option<Node<'t>>,
    route_path: Option<Node<'t>>,
    route_call_node: Option<Node<'t>>,
    is_route: bool,
    fa_route_app_node: Option<Node<'t>>,
    fa_route_method_node: Option<Node<'t>>,
    fa_route_handler_node: Option<Node<'t>>,
    dj_recv_name_node: Option<Node<'t>>,
    dj_recv_handler_node: Option<Node<'t>>,
    dj_connect_name_node: Option<Node<'t>>,
    dj_connect_handler_node: Option<Node<'t>>,
}

#[derive(Default)]
pub struct PythonFile {
    /// Framework-presence flags from the import pre-scan: the query gates
    /// framework captures the moment they fire.
    has_fastapi: bool,
    has_django: bool,
    has_celery: bool,
    has_any_http_framework: bool,
    /// The variable each record's call is made on, in step with `routes`.
    route_receivers: Vec<Option<String>>,
    /// `(target_name, span)` of FastAPI Depends() refs; the enclosing
    /// function resolves by span containment after nodes are built.
    pending_depends: Vec<(String, Span)>,
    /// Per-framework pending refs, pushed only when the file imports the
    /// matching framework.
    pending_fastapi_refs: Vec<RawFrameworkRef>,
    pending_django_refs: Vec<RawFrameworkRef>,
    pending_celery_refs: Vec<RawFrameworkRef>,
    /// Outer `getattr(self, name)()` call spans, resolved once `nodes`
    /// holds the enclosing class and its sibling methods.
    pending_getattr_sites: Vec<Span>,
}

impl ParseSpec for PythonSpec {
    type Role = PythonRole;
    type Match<'t> = PythonMatch<'t>;
    type File<'t> = PythonFile;
    type Hook = PythonHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// Lambda in arg position. Emit an `<anonymous>` Function node only
    /// when the body contains a call, so attach_to_enclosing can host
    /// those calls instead of dropping them; empty lambdas stay out of
    /// the graph.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call"],
        key: AnonKey::AnyNodeSpan,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.eval",
            (
                "python-eval",
                "eval(arg) — runtime Python code execution; called function is not statically determinable",
            ),
        ),
        (
            "blind.exec",
            (
                "python-exec",
                "exec(arg) — runtime statement execution; executed code is not statically determinable",
            ),
        ),
        (
            "blind.compile",
            (
                "python-compile",
                "compile(arg) — runtime bytecode compile; produced code object is not statically determinable",
            ),
        ),
        (
            "blind.dynamic_import",
            (
                "python-dynamic-import",
                "importlib.import_module(...) — dynamic module loading; imported module name depends on runtime value",
            ),
        ),
        (
            "blind.builtin_import",
            (
                "python-builtin-import",
                "__import__(...) — dynamic builtin import; module name depends on runtime value",
            ),
        ),
        (
            "blind.cross_getattr",
            (
                "python-cross-getattr",
                "getattr(<obj>, name)() with obj != self — cross-object reflection; target class not enumerated by ecp Phase 2",
            ),
        ),
    ];
    /// `is_test_path` also drives the test-file-only direct-receiver route
    /// filter and each BlindSpot's `is_test`.
    const TEST_RULE: TestRule = TestRule::PythonPath;
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["attribute"],
        names: NameRefSpec {
            ident_kinds: &["identifier"],
            kind: NameRefKind::Value,
            binding_fields: &[
                ("assignment", "left"),
                ("for_statement", "left"),
                ("for_in_clause", "left"),
                ("named_expression", "name"),
            ],
            binding_parents: &["splat_pattern"],
            capture_patterns: &[
                ("case_pattern", "dotted_name"),
                ("keyword_pattern", "dotted_name"),
            ],
            skip_kinds: &[
                "import_statement",
                "import_from_statement",
                "future_import_statement",
                "decorator",
                "nonlocal_statement",
                "dotted_name",
                "type",
            ],
            skip_parents: &["keyword_pattern"],
            global_decls: &["global_statement"],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        // A `case Color.RED:` pattern holds a `dotted_name`.
        path_kinds: &["attribute", "dotted_name"],
        label_parents: &[],
        bare_label_parents: &[],
        bare_expressions: false,
        token_trees: false,
    });
    const EVENT_TOPICS: &'static [EventTopicConfig] = &[
        KAFKA_PYTHON,
        CELERY_PYTHON,
        REDIS_PYTHON,
        RABBITMQ_PYTHON,
        SQS_PYTHON,
    ];
    const SCHEMA_FIELDS: &'static [SchemaFieldConfig] = &[PYDANTIC_CONFIG, SQLALCHEMY_CONFIG];
    const TX: TxRule = TxRule::Sites;
    const STEPS: &'static [Step<PythonHook>] = &[
        Step::hook(Pass::Walk, Effect::Neutral, PythonHook::ImportPreScan),
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Calls, Effect::Nodes, PythonHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Walk, Effect::Neutral, PythonHook::Indirect),
        Step::hook(Pass::Build, Effect::Neutral, PythonHook::Frameworks),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::hook(Pass::Build, Effect::Nodes, PythonHook::ClassKinds),
        Step::fixed(Pass::Build, Op::StampOwners),
        // The language `file_identities` reads off the same path, so a
        // nested def loses its owner exactly where its uid is scoped.
        Step::fixed(Pass::Build, Op::StampOwnerFns),
        // Last owner step: a nested def or lambda node has a lexical parent
        // and no owner (invariant I1), whatever the class pass gave it.
        Step::fixed(Pass::Build, Op::ClearScopedOwners),
        // Runs after the node-changing steps: they retag only non-callables
        // (Class to Interface, an enum-body assignment to EnumVariant),
        // append nodes, or write `owner_class`, and the binder reads only the
        // index, kind and span of a Function, Method or Constructor.
        Step::fixed(Pass::Build, Op::TxScopes),
        Step::fixed(Pass::Walk, Op::SchemaFieldsRequery),
        Step::fixed(Pass::Walk, Op::EventTopicsRequery),
        // Framework extractors above need Python module names; the shared
        // resolver consumes paths and explicit namespace bindings.
        Step::hook(Pass::Imports, Effect::Neutral, PythonHook::RootImports),
    ];

    fn query() -> anyhow::Result<Query> {
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        Ok(Query::new(
            &tree_sitter_python::LANGUAGE.into(),
            &query_source,
        )?)
    }

    fn role(capture: &str) -> Option<PythonRole> {
        Some(match capture {
            "function" | "class" | "property" | "variable" => PythonRole::Root,
            "type" => PythonRole::Type,
            "heritage" => PythonRole::Heritage,
            "decorator" => PythonRole::Decorator,
            "import.name" => PythonRole::ImportName,
            "import.source" => PythonRole::ImportSource,
            "import.alias" => PythonRole::ImportAlias,
            "route.method" => PythonRole::RouteMethod,
            "route.path" => PythonRole::RoutePath,
            "route.call" => PythonRole::RouteCall,
            "fastapi.depends.target" => PythonRole::FastapiDependsTarget,
            "fastapi.route.app" => PythonRole::FastapiRouteApp,
            "fastapi.route.method" => PythonRole::FastapiRouteMethod,
            "fastapi.route.handler" => PythonRole::FastapiRouteHandler,
            "django.url.handler" => PythonRole::DjangoUrlHandler,
            "django.signal.receiver_name" => PythonRole::DjangoSignalReceiverName,
            "django.signal.receiver_handler" => PythonRole::DjangoSignalReceiverHandler,
            "django.signal.connect_name" => PythonRole::DjangoSignalConnectName,
            "django.signal.connect_handler" => PythonRole::DjangoSignalConnectHandler,
            "celery.task.handler" => PythonRole::CeleryTaskHandler,
            "tx.atomic.handler" => PythonRole::Tx(FrameworkId::DjangoAtomic),
            "tx.db_session.handler" => PythonRole::Tx(FrameworkId::PonyDbSession),
            "reflection.getattr.site" => PythonRole::ReflectionGetattrSite,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: PythonRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut PythonMatch<'t>,
        f: &mut PythonFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            PythonRole::Type => m.type_annotation_node = Some(node),
            PythonRole::Heritage => {
                if let Ok(h) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.heritage.push(h.to_string());
                }
            }
            PythonRole::Decorator => {
                if let Ok(d_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.decorators.push(d_str.to_string());
                }
            }
            // Already collected in the import pre-scan; nothing to do here.
            PythonRole::ImportName | PythonRole::ImportSource | PythonRole::ImportAlias => {}
            PythonRole::RouteMethod => m.route_method = Some(node),
            PythonRole::RoutePath => m.route_path = Some(node),
            PythonRole::RouteCall => {
                m.is_route = true;
                d.root = Some(node);
                m.route_call_node = Some(node);
            }
            PythonRole::Root => d.root = Some(node),
            PythonRole::FastapiDependsTarget => {
                if !f.has_fastapi {
                    return;
                }
                if let Ok(target_name) =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    f.pending_depends
                        .push((target_name.to_string(), node_span(&node)));
                }
            }
            PythonRole::FastapiRouteApp => {
                if f.has_fastapi {
                    m.fa_route_app_node = Some(node);
                }
            }
            PythonRole::FastapiRouteMethod => {
                if f.has_fastapi {
                    m.fa_route_method_node = Some(node);
                }
            }
            PythonRole::FastapiRouteHandler => {
                if f.has_fastapi {
                    m.fa_route_handler_node = Some(node);
                }
            }
            PythonRole::DjangoUrlHandler => {
                if !f.has_django {
                    return;
                }
                if let Ok(target_name) =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    f.pending_django_refs.push(RawFrameworkRef {
                        source_name: MODULE_LEVEL_SOURCE.to_string(),
                        target_name: target_name.to_string(),
                        confidence: framework_confidence::DJANGO_URL,
                        reason: "django-url-path".to_string(),
                        span: node_span(&node),
                    });
                }
            }
            PythonRole::DjangoSignalReceiverName => {
                if f.has_django {
                    m.dj_recv_name_node = Some(node);
                }
            }
            PythonRole::DjangoSignalReceiverHandler => {
                if f.has_django {
                    m.dj_recv_handler_node = Some(node);
                }
            }
            PythonRole::DjangoSignalConnectName => {
                if f.has_django {
                    m.dj_connect_name_node = Some(node);
                }
            }
            PythonRole::DjangoSignalConnectHandler => {
                if f.has_django {
                    m.dj_connect_handler_node = Some(node);
                }
            }
            PythonRole::CeleryTaskHandler => {
                if !f.has_celery {
                    return;
                }
                if let Ok(target_name) =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    f.pending_celery_refs.push(RawFrameworkRef {
                        source_name: MODULE_LEVEL_SOURCE.to_string(),
                        target_name: target_name.to_string(),
                        confidence: framework_confidence::CELERY_TASK,
                        reason: "celery-task".to_string(),
                        span: node_span(&node),
                    });
                }
            }
            // `(handler_name_start_row, handler_name_start_col, framework)`,
            // bound to the enclosing callable by the tx step.
            PythonRole::Tx(framework) => {
                let pos = node.start_position();
                cx.tx_sites
                    .push((pos.row as u32, pos.column as u32, framework));
            }
            PythonRole::ReflectionGetattrSite => f.pending_getattr_sites.push(node_span(&node)),
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut PythonMatch<'t>,
        f: &mut PythonFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let PythonMatch {
            type_annotation_node,
            heritage,
            decorators,
            route_method,
            route_path,
            route_call_node,
            is_route,
            fa_route_app_node,
            fa_route_method_node,
            fa_route_handler_node,
            dj_recv_name_node,
            dj_recv_handler_node,
            dj_connect_name_node,
            dj_connect_handler_node,
        } = std::mem::take(m);
        // `@route.call` also sets the root, and the route span reads it.
        let root_span_node = d.root;

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
                f.pending_fastapi_refs.push(RawFrameworkRef {
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
                &mut f.pending_django_refs,
            );
        }

        if let (Some(sig_n), Some(handler_n)) = (dj_connect_name_node, dj_connect_handler_node) {
            push_django_signal_ref(
                sig_n,
                handler_n,
                source,
                "django-signal-connect",
                &mut f.pending_django_refs,
            );
        }

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, root_span_node) {
            // No ancestor filter: emit every assignment that matches the
            // query. Lets downstream consumers decide what's a true
            // module-level vs class-attr vs local — we don't pre-classify.
            // (Round 2 already split class-attrs into Property via a
            // distinct query, so this Variable capture mostly fires for
            // non-class scopes.)
            if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                let span = node::span(root);

                let type_str = type_annotation_node.and_then(|t| {
                    std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                        .ok()
                        .map(|s| s.to_string())
                });

                // `X: TypeAlias = …` (PEP 613) is a reference target, not a
                // value binding — reclassify Variable→Typedef. Match the bare
                // and dotted (`typing.TypeAlias`) annotation forms.
                let k = if k == NodeKind::Variable
                    && type_str
                        .as_deref()
                        .is_some_and(|t| t == "TypeAlias" || t == "typing.TypeAlias")
                {
                    NodeKind::Typedef
                } else {
                    k
                };
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
                if let Some(existing) = cx.nodes.iter_mut().find(|node| node.span == span) {
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
                    cx.nodes.push(RawNode {
                        decorators,
                        is_exported: !name_str.starts_with('_'),
                        heritage,
                        type_annotation: type_str,
                        ..node::decl(
                            name_str.to_string(),
                            final_kind,
                            span,
                            &source[root.start_byte()..root.end_byte()],
                        )
                    });
                }
            }
        }

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
            && !cx.imports.is_empty();
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
                return;
            }
            // Path-conditional filter: in test files, also drop calls
            // whose immediate receiver is a known test-client fixture
            // name (`http_client`, `client`, `api_client`, ...). These
            // are pytest fixtures that bind to HTTP clients — not
            // route registrations. Production files keep emitting on
            // these names because there `client = Blueprint(...)`
            // / `client = APIRouter()` is legitimate.
            if cx.is_test && is_test_file_direct_receiver_call(call_node, source) {
                return;
            }
        }
        if is_route
            && (f.has_any_http_framework
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
                            match route_call_node.and_then(|n| extract_methods_kwarg(n, source)) {
                                None => vec!["GET".to_string()],
                                Some(methods) if !methods.is_empty() => methods,
                                Some(_) => {
                                    // kwarg present but unparseable — skip
                                    // rather than fabricate a method.
                                    return;
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
                        let decorator_handler = RawRoute::named_handler(
                            route_call_node
                                .and_then(|n| resolve_decorator_handler(n, source))
                                .as_deref(),
                        );
                        let receiver = route_call_node.and_then(|n| call_receiver(n, source));
                        for method in methods_to_emit {
                            f.route_receivers.push(receiver.clone());
                            cx.routes.push(RawRoute {
                                method,
                                path: clean_path.clone(),
                                handler: decorator_handler.clone(),
                                span: node_span(&root),
                                scope: None,
                            });
                        }
                    }
                }
            }
        }
    }

    fn hook<'t>(h: PythonHook, c: &Compiled<Self>, f: &mut PythonFile, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            PythonHook::ImportPreScan => {
                // Walk the same query once just to populate `imports`, so the
                // framework-presence flags are known up-front and the main
                // pass can short-circuit framework-specific captures the
                // moment they fire.
                let mut pre_cursor = QueryCursor::new();
                let mut pre_matches = pre_cursor.matches(c.query(), root, source);
                while let Some(m) = pre_matches.next() {
                    let mut import_name_node = None;
                    let mut import_src_node = None;
                    let mut import_alias_node = None;
                    for cap in m.captures() {
                        match c.role_at(cap.index) {
                            Some(PythonRole::ImportName) => import_name_node = Some(cap.node),
                            Some(PythonRole::ImportSource) => import_src_node = Some(cap.node),
                            Some(PythonRole::ImportAlias) => import_alias_node = Some(cap.node),
                            _ => {}
                        }
                    }
                    let Some(i_name) = import_name_node else {
                        continue;
                    };
                    let Some(name_str) = i_name.utf8_text(source).ok() else {
                        continue;
                    };
                    let src_str = import_src_node
                        .map_or_else(String::new, |i_src| node_text(i_src, source).to_string());
                    let alias = import_alias_node
                        .and_then(|a| a.utf8_text(source).ok())
                        .map(str::to_string);
                    cx.imports.push(driver::imports::raw(
                        src_str,
                        name_str.to_string(),
                        alias,
                        None,
                    ));
                }
                f.has_fastapi = has_import_from(&cx.imports, FASTAPI_REQUIRED);
                f.has_django = has_import_from(&cx.imports, DJANGO_REQUIRED);
                f.has_celery = has_import_from(&cx.imports, CELERY_REQUIRED);
                f.has_any_http_framework = has_import_from(&cx.imports, HTTP_FRAMEWORK_MODULES);
            }
            PythonHook::Calls => {
                // Extract call sites with receiver-type binding. Replaces the
                // shared `extract_calls` for Python so `x.method()` can be
                // rewritten to `Type.method` when `x`'s type is known from a
                // local annotation (typed param or annotated assignment) — fed
                // back into the resolver's Tier 2.5 qualifier-scoped lookup.
                // Falls back to bare member name when no annotation is in scope.
                let local_types = collect_local_types(root, source);
                let (raw_path_literals, raw_sql_refs) = extract_python_calls_and_path_literals(
                    root,
                    source,
                    &mut cx.nodes,
                    &local_types,
                    &mut cx.call_sites,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            PythonHook::Indirect => {
                let param_names = collect_python_param_names(root, source);
                cx.call_metas =
                    detect_python_indirect(root, source, &cx.nodes, &param_names, &cx.call_sites);
            }
            PythonHook::Frameworks => {
                // Resolve FastAPI Depends() refs: find the innermost enclosing
                // Function/Method node whose span contains the capture span. The
                // site capture itself was gated by `has_fastapi` in the query,
                // so no additional gate is needed here.
                for (target_name, span) in std::mem::take(&mut f.pending_depends) {
                    if let Some(source_name) = enclosing_function_name(&cx.nodes, span) {
                        f.pending_fastapi_refs.push(RawFrameworkRef {
                            source_name,
                            target_name,
                            confidence: framework_confidence::FASTAPI_DEPENDS,
                            reason: "fastapi-depends".to_string(),
                            span,
                        });
                    }
                }

                // pending_*_refs were only populated when the matching framework
                // gate was satisfied, so we can merge unconditionally here.
                cx.framework_refs
                    .extend(std::mem::take(&mut f.pending_fastapi_refs));
                cx.framework_refs
                    .extend(std::mem::take(&mut f.pending_django_refs));
                cx.framework_refs
                    .extend(std::mem::take(&mut f.pending_celery_refs));

                // Resolve reflection-getattr fan-out sites: enclosing method
                // (source) dispatches to any sibling method on the same class.
                // Skip sites with no enclosing class (module-level) or no
                // enclosing fn (defensive).
                for span in std::mem::take(&mut f.pending_getattr_sites) {
                    let Some(source_name) = enclosing_function_name(&cx.nodes, span) else {
                        continue;
                    };
                    let Some((_class_name, class_span)) = enclosing_class(&cx.nodes, span) else {
                        continue;
                    };
                    let candidates = enumerate_class_methods(&cx.nodes, class_span, &source_name);
                    if candidates.is_empty() {
                        continue;
                    }
                    cx.fanout_refs.push(RawFanoutRef {
                        source_name,
                        candidates,
                        base_confidence: framework_confidence::FANOUT_BASE,
                        reason: "reflection-getattr-fanout".to_string(),
                        span,
                    });
                }

                compose_router_prefixes(root, source, &mut cx.routes, &f.route_receivers);

                // Dedupe routes by (method, path, span). The generic tree-sitter
                // query `arguments: (argument_list (string) @route.path)` matches
                // EVERY string child of the argument list — so
                // `app.add_url_rule("/path", "endpoint", handler)` fires twice
                // (once for "/path", once for "endpoint"). The endpoint name
                // normally fails the strict `clean_route_path` filter (no leading
                // `/`), but for registration methods we use the lax variant which
                // prepends `/` and would emit `/endpoint` as a duplicate of
                // `/path` after both normalize. Dedupe is the simplest fix
                // without changing the query (which would break Sanic's
                // `add_route(handler, "/path")` arg-order variant).
                cx.routes.sort_by(|a, b| {
                    a.method
                        .cmp(&b.method)
                        .then(a.path.cmp(&b.path))
                        .then(a.span.cmp(&b.span))
                });
                cx.routes
                    .dedup_by(|a, b| a.method == b.method && a.path == b.path && a.span == b.span);
            }
            PythonHook::ClassKinds => {
                // §4.5b: promote Class → Interface when ALL bases are
                // Protocol-markers. Runs after the query so each node's
                // heritage is fully assembled (tree-sitter emits one capture
                // per base expression, so per-match classification would
                // misclassify mixed-base classes on first match).
                for node in &mut cx.nodes {
                    if node.kind == NodeKind::Class && is_protocol_marker_only(&node.heritage) {
                        node.kind = NodeKind::Interface;
                    }
                }

                // Track A + Track B: walk every class_definition in the AST.
                // For Enum subclasses → emit EnumVariant nodes.
                // For non-Enum classes with ≥2 UPPERCASE const assignments + no
                // defs → push BlindSpot. Runs after Protocol promotion so
                // `nodes` heritage is fully assembled.
                let mut stack = vec![root];
                while let Some(node) = stack.pop() {
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        stack.push(child);
                    }
                    if node.kind() != "class_definition" {
                        continue;
                    }
                    let start = node.start_position();
                    let end = node.end_position();
                    let class_span = (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    );
                    // Look up heritage from the already-collected Class node
                    // (matched by span).
                    let heritage: Vec<String> = cx
                        .nodes
                        .iter()
                        .find(|n| n.span == class_span && n.kind == NodeKind::Class)
                        .map(|n| n.heritage.clone())
                        .unwrap_or_default();
                    process_class_for_enum(
                        node,
                        source,
                        &heritage,
                        cx.path,
                        cx.is_test,
                        &mut cx.nodes,
                        &mut cx.blind_spots,
                    );
                }
            }
            PythonHook::RootImports => {
                let mut root_imports = Vec::new();
                let mut root_bindings: rustc_hash::FxHashSet<String> = cx
                    .imports
                    .iter()
                    .filter(|import| {
                        import.source.is_empty()
                            && !import.imported_name.contains('.')
                            && import
                                .alias
                                .as_deref()
                                .is_none_or(|alias| alias == import.imported_name)
                    })
                    .map(|import| import.imported_name.clone())
                    .collect();
                for import in &mut cx.imports {
                    if import.source.is_empty() {
                        // An unaliased dotted import also binds its root package.
                        if import.alias.is_none() {
                            if let Some((root, _)) = import.imported_name.split_once('.') {
                                if root_bindings.insert(root.to_string()) {
                                    root_imports.push(RawImport {
                                        alias: Some(root.to_string()),
                                        imported_name: "*".to_string(),
                                        source: root.to_string(),
                                        binding_kind: None,
                                    });
                                }
                            }
                        }
                        import.source = import.imported_name.replace('.', "/");
                        import.alias = Some(
                            import
                                .alias
                                .take()
                                .unwrap_or_else(|| import.imported_name.clone()),
                        );
                        import.imported_name = "*".to_string();
                    } else if !import.source.starts_with('.') {
                        import.source = import.source.replace('.', "/");
                    }
                }
                cx.imports.extend(root_imports);
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::python::extract(root, source, nodes, category)
    }
}
