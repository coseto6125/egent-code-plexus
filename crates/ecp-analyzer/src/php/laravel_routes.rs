//! Laravel route groups: the prefix, middleware, controller namespace and
//! controller class that enclosing `Route::group([...], fn)` /
//! `Route::prefix('p')->group(fn)` calls apply to a route, the controller
//! action a route names, the routes a `Route::resource` / `match` / `any`
//! call registers, and the route files a group loads.
//!
//! A file shows only its own groups. A group that loads another route file
//! (`->group(base_path('routes/admin.php'))`, `require __DIR__.'/auth.php'`)
//! becomes a [`RawRoute::INCLUDE`] record, and [`LaravelRoutes`] applies it
//! to that file's routes when the builder promotes them. The same records
//! admit a loaded route file that imports no framework.

use super::parser::{walk_to_scoped_root_scope, PHP_ROUTER_SCOPES};
use crate::framework_helpers::node_span;
use crate::resolution::index::{SymbolTable, MAX_HERITAGE_DEPTH};
use crate::route_detector::{clean_route_path_lax, join_route_path};
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawRoute, RawRouteScope, RouteInclude};
use ecp_core::graph::NodeKind;
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

/// Include scopes a file can reach through chains of includes. Past this, a
/// file's routes stop multiplying: each scope is one more Route node per route.
const MAX_INCLUDE_SCOPES: usize = 16;

/// What one group level, or a route's own call chain, sets.
#[derive(Default)]
struct GroupAttrs {
    prefix: Option<String>,
    middleware: Vec<String>,
    namespace: Option<String>,
    controller: Option<String>,
}

/// The group levels around `node`, outermost first. A level is a `group`
/// call on the router whose closure holds `node`.
fn enclosing_groups(node: Node, source: &[u8]) -> Vec<GroupAttrs> {
    let mut levels = Vec::new();
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        if matches!(parent.kind(), "anonymous_function" | "arrow_function") {
            if let Some(call) = closure_group_call(parent, source) {
                levels.push(group_call_attrs(call, source));
            }
        }
        cur = parent;
    }
    levels.reverse();
    levels
}

/// The router `group` call that takes `closure` as an argument.
fn closure_group_call<'t>(closure: Node<'t>, source: &[u8]) -> Option<Node<'t>> {
    let argument = closure.parent().filter(|n| n.kind() == "argument")?;
    let call = argument
        .parent()
        .filter(|n| n.kind() == "arguments")?
        .parent()?;
    (call_name(call, source) == Some("group") && is_router_call(call, source)).then_some(call)
}

/// Whether `call` is `Route::x(...)` or a chain rooted in one.
fn is_router_call(call: Node, source: &[u8]) -> bool {
    walk_to_scoped_root_scope(call, source)
        .is_some_and(|scope| PHP_ROUTER_SCOPES.contains(&scope.as_str()))
}

fn group_call_attrs(call: Node, source: &[u8]) -> GroupAttrs {
    let mut attrs = GroupAttrs::default();
    if let Some(object) = call.child_by_field_name("object") {
        chain_attrs(object, source, &mut attrs);
    }
    if let Some(options) = nth_argument(call, 0).filter(|n| n.kind() == "array_creation_expression")
    {
        option_attrs(options, source, &mut attrs);
    }
    attrs
}

/// Read `Route::prefix('p')->middleware('m')->...` from the root call
/// outward, so a later call of the same attribute replaces an earlier one,
/// as Laravel's `RouteRegistrar` does.
fn chain_attrs(mut node: Node, source: &[u8], attrs: &mut GroupAttrs) {
    let mut calls = Vec::new();
    loop {
        match node.kind() {
            "member_call_expression" => {
                calls.push(node);
                match node.child_by_field_name("object") {
                    Some(object) => node = object,
                    None => break,
                }
            }
            "scoped_call_expression" => {
                calls.push(node);
                break;
            }
            _ => break,
        }
    }
    for call in calls.into_iter().rev() {
        let (Some(name), Some(value)) = (call_name(call, source), nth_argument(call, 0)) else {
            continue;
        };
        set_attr(attrs, name, value, source);
    }
}

/// `Route::group(['prefix' => 'p', 'middleware' => [...]], ...)`.
fn option_attrs(options: Node, source: &[u8], attrs: &mut GroupAttrs) {
    for (key, value) in array_pairs(options, source) {
        set_attr(attrs, &key, value, source);
    }
}

fn set_attr(attrs: &mut GroupAttrs, name: &str, value: Node, source: &[u8]) {
    match name {
        "prefix" => attrs.prefix = string_value(value, source),
        "middleware" => attrs.middleware = string_list(value, source),
        "namespace" => attrs.namespace = namespace_value(value, source),
        "controller" => attrs.controller = class_name(value, source),
        _ => {}
    }
}

/// `->middleware('auth')` calls chained after the verb call:
/// `Route::get('/x', ...)->middleware('auth')`.
fn trailing_middleware(call: Node, source: &[u8]) -> Vec<String> {
    let mut middleware = Vec::new();
    let mut cur = call;
    while let Some(parent) = cur
        .parent()
        .filter(|p| p.kind() == "member_call_expression")
        .filter(|p| p.child_by_field_name("object") == Some(cur))
    {
        if call_name(parent, source) == Some("middleware") {
            if let Some(value) = nth_argument(parent, 0) {
                middleware.extend(string_list(value, source));
            }
        }
        cur = parent;
    }
    middleware
}

/// The scope of a route or include site: each enclosing group, then `own`.
struct SiteScope {
    prefixes: Vec<String>,
    scope: RawRouteScope,
    controller: Option<String>,
}

fn site_scope(node: Node, own: GroupAttrs, source: &[u8]) -> SiteScope {
    let mut levels = enclosing_groups(node, source);
    levels.push(own);
    let mut site = SiteScope {
        prefixes: Vec::new(),
        scope: RawRouteScope::default(),
        controller: None,
    };
    for level in levels {
        site.prefixes.extend(level.prefix);
        site.scope.middleware.extend(level.middleware);
        site.scope.namespace =
            compose_namespace(site.scope.namespace.as_deref(), level.namespace.as_deref());
        if level.controller.is_some() {
            site.controller = level.controller;
        }
    }
    site
}

/// The scope of the router call `call` with what its own chain sets:
/// `Route::middleware('m')->get(...)->middleware('n')`.
fn call_scope(call: Node, source: &[u8]) -> SiteScope {
    let mut own = GroupAttrs::default();
    if let Some(object) = call.child_by_field_name("object") {
        chain_attrs(object, source, &mut own);
    }
    own.middleware.extend(trailing_middleware(call, source));
    site_scope(call, own, source)
}

impl SiteScope {
    /// The full in-file path of a route whose path literal is `raw_path`.
    /// `None` when the path is empty and no group prefixes it, which is the
    /// pre-group behaviour of [`clean_route_path_lax`].
    fn route_path(&self, raw_path: &str) -> Option<String> {
        if self.prefixes.is_empty() {
            return clean_route_path_lax(raw_path);
        }
        let tail = clean_route_path_lax(raw_path).unwrap_or_default();
        Some(join_route_path(
            self.prefixes
                .iter()
                .map(String::as_str)
                .chain([tail.as_str()]),
        ))
    }
}

/// One Laravel route registration, as its file shows it.
pub(super) struct RouteSite {
    pub path: String,
    pub scope: RawRouteScope,
    pub handler: Option<String>,
}

/// The names of the file, for resolving the class names a route names.
#[derive(Clone, Copy)]
pub(super) struct FileNames<'a> {
    pub imports: &'a [RawImport],
    pub namespace: Option<&'a str>,
}

impl FileNames<'_> {
    /// `X::class` names the class PHP resolves `X` to: through the file's
    /// `use` imports, else inside the file's namespace. Both are fully
    /// qualified. In a file with neither, the name stays relative, and
    /// Laravel prefixes the group namespace.
    fn resolve(&self, name: &str) -> String {
        if name.starts_with('\\') {
            return name.to_string();
        }
        let (first, rest) = match name.split_once('\\') {
            Some((first, rest)) => (first, Some(rest)),
            None => (name, None),
        };
        let imported = self.imports.iter().find(|import| {
            import
                .alias
                .as_deref()
                .unwrap_or(&import.imported_name)
                .eq_ignore_ascii_case(first)
        });
        match (imported, self.namespace) {
            (Some(import), _) => {
                let source = import.source.trim_start_matches('\\');
                match rest {
                    Some(rest) => format!("\\{source}\\{rest}"),
                    None => format!("\\{source}"),
                }
            }
            (None, Some(namespace)) => format!("\\{namespace}\\{name}"),
            (None, None) => name.to_string(),
        }
    }
}

/// The full in-file path, scope and controller action of the verb call
/// `call` (`Route::get(...)`, or the outer call of
/// `Route::middleware('m')->get(...)`) whose path literal is `raw_path`.
pub(super) fn route_site(
    call: Node,
    raw_path: &str,
    source: &[u8],
    names: FileNames,
) -> Option<RouteSite> {
    let site = call_scope(call, source);
    let controller = site.controller.as_deref().map(|class| names.resolve(class));
    Some(RouteSite {
        path: site.route_path(raw_path)?,
        handler: nth_argument(call, 1)
            .and_then(|action| route_action(action, controller.as_deref(), source, names)),
        scope: site.scope,
    })
}

/// One action of a resource controller, as Laravel's `ResourceRegistrar`
/// registers it.
struct ResourceAction {
    action: &'static str,
    method: &'static str,
    /// The action addresses one record, so its path ends in the resource's
    /// wildcard (`/photos/{photo}`) and `shallow` drops the parents.
    member: bool,
    suffix: &'static str,
}

/// `update` appears twice: Laravel routes both PUT and PATCH to it.
const RESOURCE_ACTIONS: &[ResourceAction] = &[
    ResourceAction {
        action: "index",
        method: "GET",
        member: false,
        suffix: "",
    },
    ResourceAction {
        action: "create",
        method: "GET",
        member: false,
        suffix: "create",
    },
    ResourceAction {
        action: "store",
        method: "POST",
        member: false,
        suffix: "",
    },
    ResourceAction {
        action: "show",
        method: "GET",
        member: true,
        suffix: "",
    },
    ResourceAction {
        action: "edit",
        method: "GET",
        member: true,
        suffix: "edit",
    },
    ResourceAction {
        action: "update",
        method: "PUT",
        member: true,
        suffix: "",
    },
    ResourceAction {
        action: "update",
        method: "PATCH",
        member: true,
        suffix: "",
    },
    ResourceAction {
        action: "destroy",
        method: "DELETE",
        member: true,
        suffix: "",
    },
];

/// `apiResource` registers no `create` / `edit` form pages.
const API_RESOURCE_ACTIONS: &[&str] = &["index", "store", "show", "update", "destroy"];

/// The routes one `Route::match([...], ...)`, `Route::any(...)`,
/// `Route::resource(...)`, `Route::apiResource(...)`, `Route::resources([...])`
/// or `Route::apiResources([...])` call registers, as `(method, site)`.
///
/// `any` is one route of method `ANY`, the name Laravel's `route:list`
/// shows for it: seven verb rows would state the same handler seven times.
pub(super) fn route_set_sites(
    call: Node,
    source: &[u8],
    names: FileNames,
) -> Vec<(String, RouteSite)> {
    if !is_router_call(call, source) {
        return Vec::new();
    }
    let site = call_scope(call, source);
    let controller = site.controller.as_deref().map(|class| names.resolve(class));
    // `path_arg` is the path argument's position; the action follows it.
    let verb_sites = |methods: Vec<String>, path_arg: usize| {
        let Some(path) = nth_argument(call, path_arg)
            .filter(|path| matches!(path.kind(), "string" | "encapsed_string"))
            .and_then(|path| path.utf8_text(source).ok())
            .and_then(|raw| site.route_path(raw))
        else {
            return Vec::new();
        };
        let handler = nth_argument(call, path_arg + 1)
            .and_then(|action| route_action(action, controller.as_deref(), source, names));
        let mut out: Vec<(String, RouteSite)> = Vec::new();
        for method in methods {
            let method = method.to_ascii_uppercase();
            if out.iter().any(|(seen, _)| *seen == method) {
                continue;
            }
            let route = RouteSite {
                path: path.clone(),
                scope: site.scope.clone(),
                handler: handler.clone(),
            };
            out.push((method, route));
        }
        out
    };
    match call_name(call, source) {
        Some("match") => verb_sites(
            nth_argument(call, 0).map_or_else(Vec::new, |verbs| string_list(verbs, source)),
            1,
        ),
        Some("any") => verb_sites(vec!["ANY".to_string()], 0),
        Some(name @ ("resource" | "apiResource")) => {
            let resource =
                nth_argument(call, 0).and_then(|resource| string_value(resource, source));
            let (Some(resource), Some(class)) = (resource, nth_argument(call, 1)) else {
                return Vec::new();
            };
            let options =
                resource_options(call, nth_argument(call, 2), name == "apiResource", source);
            resource_sites(&site, &resource, class, &options, source, names)
        }
        Some(name @ ("resources" | "apiResources")) => {
            let Some(resources) = nth_argument(call, 0) else {
                return Vec::new();
            };
            let options =
                resource_options(call, nth_argument(call, 1), name == "apiResources", source);
            array_pairs(resources, source)
                .into_iter()
                .flat_map(|(resource, class)| {
                    resource_sites(&site, &resource, class, &options, source, names)
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// What a resource registration's options and chained calls keep or rename.
#[derive(Default)]
struct ResourceOptions {
    only: Option<Vec<String>>,
    except: Option<Vec<String>>,
    /// Resource segment → wildcard name: `->parameters(['photos' => 'pic'])`.
    parameters: FxHashMap<String, String>,
    shallow: bool,
}

impl ResourceOptions {
    /// Laravel's `getResourceMethods`: `only` first, then `except`.
    fn keeps(&self, action: &str) -> bool {
        self.only
            .as_ref()
            .is_none_or(|only| only.iter().any(|a| a == action))
            && self
                .except
                .as_ref()
                .is_none_or(|except| except.iter().all(|a| a != action))
    }

    /// Laravel's `getResourceWildcard`.
    fn wildcard(&self, segment: &str) -> String {
        self.parameters
            .get(segment)
            .cloned()
            .unwrap_or_else(|| singular(segment))
            .replace('-', "_")
    }
}

/// The options array argument, then the calls chained on the pending
/// registration (`->only([...])->except('show')`): a later setting
/// replaces an earlier one, as the `PendingResourceRegistration` option
/// array does. `apiResource` starts from its own `only` list.
fn resource_options(
    call: Node,
    options: Option<Node>,
    api: bool,
    source: &[u8],
) -> ResourceOptions {
    let mut out = ResourceOptions {
        only: api.then(|| API_RESOURCE_ACTIONS.iter().map(|a| a.to_string()).collect()),
        ..ResourceOptions::default()
    };
    if let Some(options) = options {
        for (name, value) in array_pairs(options, source) {
            set_resource_option(&mut out, &name, &[value], source);
        }
    }
    let mut cur = call;
    while let Some(parent) = cur
        .parent()
        .filter(|p| p.kind() == "member_call_expression")
        .filter(|p| p.child_by_field_name("object") == Some(cur))
    {
        if let Some(name) = call_name(parent, source) {
            let values: Vec<Node> = arguments(parent)
                .into_iter()
                .filter_map(argument_value)
                .collect();
            set_resource_option(&mut out, name, &values, source);
        }
        cur = parent;
    }
    out
}

fn set_resource_option(options: &mut ResourceOptions, name: &str, values: &[Node], source: &[u8]) {
    // `only(['index', 'show'])` and `only('index', 'show')` both work.
    let actions = || match values {
        [single] => string_list(*single, source),
        many => many
            .iter()
            .filter_map(|v| string_value(*v, source))
            .collect(),
    };
    match name {
        "only" => options.only = Some(actions()),
        "except" => options.except = Some(actions()),
        "parameters" => {
            if let Some(map) = values.first() {
                for (segment, wildcard) in array_pairs(*map, source) {
                    if let Some(wildcard) = string_value(wildcard, source) {
                        options.parameters.insert(segment, wildcard);
                    }
                }
            }
        }
        "parameter" => {
            if let [segment, wildcard] = values {
                if let (Some(segment), Some(wildcard)) = (
                    string_value(*segment, source),
                    string_value(*wildcard, source),
                ) {
                    options.parameters.insert(segment, wildcard);
                }
            }
        }
        "shallow" => {
            options.shallow = values
                .first()
                .is_none_or(|v| v.utf8_text(source).ok() != Some("false"))
        }
        _ => {}
    }
}

/// The routes of `Route::resource(resource, class)`. `admin/photos` puts
/// the resource under the `admin` prefix; `photos.comments` nests it under
/// one photo: `/photos/{photo}/comments/{comment}`.
fn resource_sites(
    site: &SiteScope,
    resource: &str,
    class: Node,
    options: &ResourceOptions,
    source: &[u8],
    names: FileNames,
) -> Vec<(String, RouteSite)> {
    // A string class is relative to the group namespace, as in 'C@m'.
    let Some(class) = class_constant_class(class, source)
        .map(|class| names.resolve(class))
        .or_else(|| string_value(class, source))
    else {
        return Vec::new();
    };
    let (prefix, resource) = resource.rsplit_once('/').unwrap_or(("", resource));
    let nested: Vec<&str> = resource.split('.').filter(|s| !s.is_empty()).collect();
    let Some((last, parents)) = nested.split_last() else {
        return Vec::new();
    };
    let parents: Vec<String> = parents
        .iter()
        .flat_map(|parent| {
            [
                parent.to_string(),
                format!("{{{}}}", options.wildcard(parent)),
            ]
        })
        .collect();
    let wildcard = format!("{{{}}}", options.wildcard(last));
    RESOURCE_ACTIONS
        .iter()
        .filter(|action| options.keeps(action.action))
        .map(|action| {
            let mut segments: Vec<&str> = site.prefixes.iter().map(String::as_str).collect();
            segments.push(prefix);
            if !(options.shallow && action.member) {
                segments.extend(parents.iter().map(String::as_str));
            }
            segments.push(last);
            if action.member {
                segments.push(&wildcard);
            }
            segments.push(action.suffix);
            let route = RouteSite {
                path: join_route_path(segments),
                scope: site.scope.clone(),
                handler: Some(format!("{class}@{}", action.action)),
            };
            (action.method.to_string(), route)
        })
        .collect()
}

/// Words Laravel's pluralizer leaves as they are.
const UNCOUNTABLE: &[&str] = &[
    "audio",
    "data",
    "education",
    "equipment",
    "evidence",
    "feedback",
    "fish",
    "information",
    "knowledge",
    "metadata",
    "money",
    "news",
    "police",
    "series",
    "sheep",
    "software",
    "species",
    "traffic",
];

const IRREGULAR: &[(&str, &str)] = &[
    ("children", "child"),
    ("criteria", "criterion"),
    ("feet", "foot"),
    ("geese", "goose"),
    ("men", "man"),
    ("mice", "mouse"),
    ("movies", "movie"),
    ("people", "person"),
    ("teeth", "tooth"),
    ("women", "woman"),
];

/// Plurals that add `es` to a word ending in `s`, `x`, `z`, `ch` or `sh`.
const ES_PLURALS: &[&str] = &[
    "sses", "xes", "zes", "ches", "shes", "buses", "statuses", "aliases", "campuses", "viruses",
];

/// `Str::singular` of the last word of a resource segment, for the common
/// English plurals: `categories` → `category`, `boxes` → `box`,
/// `buses` → `bus`, `news` → `news`.
fn singular(segment: &str) -> String {
    let split = segment.rfind(['_', '-']).map_or(0, |i| i + 1);
    let (head, word) = segment.split_at(split);
    // ASCII lower-casing keeps byte lengths, so `lower` slices like `word`.
    let lower = word.to_ascii_lowercase();
    if UNCOUNTABLE.contains(&lower.as_str()) {
        return segment.to_string();
    }
    if let Some((_, one)) = IRREGULAR.iter().find(|(many, _)| *many == lower) {
        return format!("{head}{one}");
    }
    if lower.len() > 3 && lower.ends_with("ies") {
        return format!("{head}{}y", &word[..word.len() - 3]);
    }
    let cut = if ES_PLURALS.iter().any(|suffix| lower.ends_with(suffix)) {
        2
    } else if lower.ends_with('s') && !["ss", "us", "is"].iter().any(|s| lower.ends_with(s)) {
        1
    } else {
        0
    };
    format!("{head}{}", &word[..word.len() - cut])
}

/// The `Class@method` the action argument `action` names. A class name
/// with a leading `\` is fully qualified; any other is relative to the
/// group namespace. Closures and anything else name no action.
fn route_action(
    action: Node,
    controller: Option<&str>,
    source: &[u8],
    names: FileNames,
) -> Option<String> {
    match action.kind() {
        "string" | "encapsed_string" => {
            let text = string_value(action, source)?;
            Some(match (text.contains('@'), controller) {
                (true, _) => text,
                // Inside `Route::controller(C::class)->group(...)` a bare
                // string names a method of `C`.
                (false, Some(class)) => format!("{class}@{text}"),
                (false, None) => format!("{text}@__invoke"),
            })
        }
        "class_constant_access_expression" => Some(format!(
            "{}@__invoke",
            names.resolve(class_constant_class(action, source)?)
        )),
        "array_creation_expression" => {
            let mut values = array_values(action);
            let class = values.next()?;
            let method = string_value(values.next()?, source)?;
            let class = match class_constant_class(class, source) {
                Some(class) => names.resolve(class),
                None => string_value(class, source)?,
            };
            Some(format!("{class}@{method}"))
        }
        _ => None,
    }
}

/// Append a [`RawRoute::INCLUDE`] record for each route file `node` loads:
/// `->group(<file>)` or `Route::group([...], <file>)` on the router, the
/// Laravel 11 `->withRouting(web: <file>, api: <file>)` builder, or a
/// `require <file>`. A `group` whose argument is a closure loads no file.
pub(super) fn push_includes(node: Node, source: &[u8], out: &mut Vec<RawRoute>) {
    match node.kind() {
        "member_call_expression" | "scoped_call_expression" => match call_name(node, source) {
            Some("group") if is_router_call(node, source) => {
                let Some(target) = last_argument(node).and_then(|arg| include_target(arg, source))
                else {
                    return;
                };
                push_include(node, target, group_call_attrs(node, source), source, out);
            }
            Some("withRouting") => push_with_routing(node, source, out),
            _ => {}
        },
        _ => {
            if let Some(target) = node
                .named_child(0)
                .and_then(|path| include_target(path, source))
            {
                push_include(node, target, GroupAttrs::default(), source, out);
            }
        }
    }
}

/// `->withRouting(web: ..., api: ..., apiPrefix: 'api')`: Laravel 11 loads
/// `web` files under the `web` middleware group and `api` files under the
/// `api` middleware group and prefix.
fn push_with_routing(call: Node, source: &[u8], out: &mut Vec<RawRoute>) {
    let named: Vec<(&str, Node)> = arguments(call)
        .into_iter()
        .filter_map(|argument| {
            let name = argument
                .child_by_field_name("name")?
                .utf8_text(source)
                .ok()?;
            Some((name, argument_value(argument)?))
        })
        .collect();
    let api_prefix = named
        .iter()
        .find(|(name, _)| *name == "apiPrefix")
        .and_then(|(_, value)| string_value(*value, source))
        .unwrap_or_else(|| "api".to_string());
    for (name, value) in &named {
        let (prefix, middleware) = match *name {
            "web" => (None, "web"),
            "api" => (Some(api_prefix.clone()), "api"),
            _ => continue,
        };
        let files: Vec<Node> = match value.kind() {
            "array_creation_expression" => array_values(*value).collect(),
            _ => vec![*value],
        };
        for file in files {
            let Some(target) = include_target(file, source) else {
                continue;
            };
            let attrs = GroupAttrs {
                prefix: prefix.clone(),
                middleware: vec![middleware.to_string()],
                ..GroupAttrs::default()
            };
            push_include(file, target, attrs, source, out);
        }
    }
}

fn push_include(
    node: Node,
    target: RouteInclude,
    own: GroupAttrs,
    source: &[u8],
    out: &mut Vec<RawRoute>,
) {
    let site = site_scope(node, own, source);
    let mut scope = site.scope;
    scope.include = Some(target);
    out.push(RawRoute {
        method: RawRoute::INCLUDE.to_string(),
        path: join_prefix(site.prefixes.iter().map(String::as_str)),
        handler: None,
        span: node_span(&node),
        scope: Some(Box::new(scope)),
    });
}

/// The file a path expression names: `base_path('routes/x.php')`,
/// `__DIR__ . '/x.php'`, `dirname(__DIR__) . '/routes/x.php'`.
fn include_target(expr: Node, source: &[u8]) -> Option<RouteInclude> {
    match expr.kind() {
        "function_call_expression" => {
            let function = expr
                .child_by_field_name("function")?
                .utf8_text(source)
                .ok()?;
            if function.trim_start_matches('\\') != "base_path" {
                return None;
            }
            let path = string_value(nth_argument(expr, 0)?, source)?;
            Some(RouteInclude::AppRoot(
                path.trim_start_matches('/').to_string(),
            ))
        }
        "binary_expression" => {
            if expr.child_by_field_name("operator")?.kind() != "." {
                return None;
            }
            let base = dir_levels(expr.child_by_field_name("left")?, source)?;
            let tail = string_value(expr.child_by_field_name("right")?, source)?;
            Some(RouteInclude::FileDir(format!(
                "{}{}",
                "../".repeat(base),
                tail.trim_start_matches('/')
            )))
        }
        _ => None,
    }
}

/// How many directories above the current file's `expr` names: 0 for
/// `__DIR__`, `n` for `dirname(__DIR__, n)`.
fn dir_levels(expr: Node, source: &[u8]) -> Option<usize> {
    match expr.kind() {
        "name" => (expr.utf8_text(source).ok()? == "__DIR__").then_some(0),
        "function_call_expression" => {
            let function = expr
                .child_by_field_name("function")?
                .utf8_text(source)
                .ok()?;
            if function.trim_start_matches('\\') != "dirname" {
                return None;
            }
            let inner = dir_levels(nth_argument(expr, 0)?, source)?;
            let levels = match nth_argument(expr, 1) {
                Some(n) => n.utf8_text(source).ok()?.parse().ok()?,
                None => 1,
            };
            Some(inner + levels)
        }
        _ => None,
    }
}

/// `$this->namespace` reads the default of the enclosing class's
/// `$namespace` property, the `RouteServiceProvider` convention.
fn namespace_value(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() != "member_access_expression" {
        return string_value(node, source);
    }
    if node.child_by_field_name("object")?.utf8_text(source).ok()? != "$this" {
        return None;
    }
    let property = node.child_by_field_name("name")?.utf8_text(source).ok()?;
    let mut class = node.parent()?;
    while class.kind() != "class_declaration" {
        class = class.parent()?;
    }
    let body = class.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let declarations: Vec<Node> = body
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "property_declaration")
        .collect();
    declarations.into_iter().find_map(|declaration| {
        let mut cursor = declaration.walk();
        let elements: Vec<Node> = declaration
            .named_children(&mut cursor)
            .filter(|n| n.kind() == "property_element")
            .collect();
        elements.into_iter().find_map(|element| {
            let name = element
                .child_by_field_name("name")?
                .utf8_text(source)
                .ok()?;
            if name.trim_start_matches('$') != property {
                return None;
            }
            string_value(element.child_by_field_name("default_value")?, source)
        })
    })
}

/// A middleware value: `'auth'`, `['auth', 'verified']`, or class names
/// (`[Ensure::class]`), kept as written; non-literal entries dropped.
fn string_list(node: Node, source: &[u8]) -> Vec<String> {
    match node.kind() {
        "array_creation_expression" => array_values(node)
            .filter_map(|value| class_name(value, source))
            .collect(),
        _ => class_name(node, source).into_iter().collect(),
    }
}

/// `'C'` or `C::class`, as written.
fn class_name(node: Node, source: &[u8]) -> Option<String> {
    string_value(node, source).or_else(|| class_constant_class(node, source).map(str::to_string))
}

/// The class `C` of `C::class`.
fn class_constant_class<'s>(node: Node, source: &'s [u8]) -> Option<&'s str> {
    if node.kind() != "class_constant_access_expression" {
        return None;
    }
    let constant = node.named_child(node.named_child_count().checked_sub(1)? as u32)?;
    if constant.utf8_text(source).ok()? != "class" {
        return None;
    }
    node.named_child(0)?.utf8_text(source).ok()
}

/// The value of a quoted literal without interpolation.
fn string_value(node: Node, source: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let mut cursor = node.walk();
    if node
        .named_children(&mut cursor)
        .any(|c| !matches!(c.kind(), "string_content" | "escape_sequence"))
    {
        return None;
    }
    let text = node.utf8_text(source).ok()?;
    let inner = text.get(1..text.len().checked_sub(1)?)?;
    Some(inner.replace("\\\\", "\\").replace("\\'", "'"))
}

fn call_name<'s>(call: Node, source: &'s [u8]) -> Option<&'s str> {
    call.child_by_field_name("name")?.utf8_text(source).ok()
}

fn arguments<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    let Some(list) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|n| n.kind() == "argument")
        .collect()
}

/// The expression of an `argument`, past a PHP 8 `name:` label.
fn argument_value(argument: Node) -> Option<Node> {
    argument.named_child(argument.named_child_count().checked_sub(1)? as u32)
}

fn nth_argument(call: Node, n: usize) -> Option<Node> {
    arguments(call).into_iter().nth(n).and_then(argument_value)
}

fn last_argument(call: Node) -> Option<Node> {
    arguments(call).into_iter().last().and_then(argument_value)
}

/// The `'key' => value` pairs of an array literal; positional values and
/// non-literal keys skipped.
fn array_pairs<'t>(array: Node<'t>, source: &[u8]) -> Vec<(String, Node<'t>)> {
    if array.kind() != "array_creation_expression" {
        return Vec::new();
    }
    let mut cursor = array.walk();
    let elements: Vec<Node<'t>> = array
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "array_element_initializer" && n.named_child_count() == 2)
        .collect();
    elements
        .into_iter()
        .filter_map(|element| {
            let key = string_value(element.named_child(0)?, source)?;
            Some((key, element.named_child(1)?))
        })
        .collect()
}

/// The positional values of an array literal; `key => value` pairs skipped.
fn array_values<'t>(array: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    let mut cursor = array.walk();
    let values: Vec<Node<'t>> = array
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "array_element_initializer" && n.named_child_count() == 1)
        .filter_map(|n| n.named_child(0))
        .collect();
    values.into_iter()
}

/// Laravel's `RouteGroup::formatNamespace`: a namespace with a leading `\`
/// replaces the outer one, any other is appended to it.
fn compose_namespace(outer: Option<&str>, inner: Option<&str>) -> Option<String> {
    match (outer, inner) {
        (_, Some(inner)) if inner.starts_with('\\') => Some(inner.to_string()),
        (Some(outer), Some(inner)) => Some(format!("{}\\{inner}", outer.trim_end_matches('\\'))),
        (None, Some(inner)) => Some(inner.to_string()),
        (outer, None) => outer.map(str::to_string),
    }
}

/// Prefix segments joined as a route path, empty when they hold none.
fn join_prefix<'a>(segments: impl IntoIterator<Item = &'a str>) -> String {
    let path = join_route_path(segments);
    if path == "/" {
        String::new()
    } else {
        path
    }
}

/// What an including group applies to an included file's routes.
#[derive(Clone, Default, PartialEq)]
struct IncludeScope {
    /// `/admin`, or empty for none.
    prefix: String,
    middleware: Vec<String>,
    namespace: Option<String>,
}

impl IncludeScope {
    fn then(&self, inner: &IncludeScope) -> IncludeScope {
        IncludeScope {
            prefix: join_prefix([self.prefix.as_str(), inner.prefix.as_str()]),
            middleware: self
                .middleware
                .iter()
                .chain(&inner.middleware)
                .cloned()
                .collect(),
            namespace: compose_namespace(self.namespace.as_deref(), inner.namespace.as_deref()),
        }
    }
}

/// One Route node a Laravel route becomes under one include scope.
pub struct PromotedRoute {
    pub path: String,
    pub middleware: Vec<String>,
    pub namespace: Option<String>,
}

/// The controller method a route action binds.
#[derive(Debug, PartialEq)]
pub struct ControllerAction {
    pub node: u32,
    /// A namespace pinned the class; else the class name matched one class.
    pub by_namespace: bool,
    /// The class does not declare the method: a used trait or a parent
    /// class, reached through resolved heritage, does.
    pub inherited: bool,
}

/// The cross-file half of Laravel routing: the include scopes of each
/// route file, and the controller classes route actions name.
#[derive(Default)]
pub struct LaravelRoutes {
    /// Keyed by the included file's index; a file nothing includes has no
    /// entry and keeps the paths its own file shows.
    include_scopes: FxHashMap<u32, Vec<IncludeScope>>,
    controllers: ControllerIndex,
}

impl LaravelRoutes {
    pub fn build(local_graphs: &[LocalGraph]) -> Self {
        let routes = || local_graphs.iter().flat_map(|graph| &graph.routes);
        let has_includes = routes().any(|route| route.method == RawRoute::INCLUDE);
        let has_actions = routes().any(|route| route.scope.is_some() && route.handler.is_some());
        Self {
            include_scopes: if has_includes {
                include_scopes(local_graphs)
            } else {
                FxHashMap::default()
            },
            controllers: if has_actions {
                ControllerIndex::build(local_graphs)
            } else {
                ControllerIndex::default()
            },
        }
    }

    /// The Route nodes a route of file `file_idx` with `scope` and checked
    /// `path` becomes, one per include scope that reaches the file. A route
    /// of a file without a framework import becomes none unless a file
    /// with one loads its file.
    pub fn promote(&self, file_idx: u32, scope: &RawRouteScope, path: &str) -> Vec<PromotedRoute> {
        let Some(includes) = self.include_scopes.get(&file_idx) else {
            if scope.needs_include {
                return Vec::new();
            }
            return vec![PromotedRoute {
                path: path.to_string(),
                middleware: scope.middleware.clone(),
                namespace: scope.namespace.clone(),
            }];
        };
        includes
            .iter()
            .map(|include| PromotedRoute {
                path: join_route_path([include.prefix.as_str(), path]),
                middleware: include
                    .middleware
                    .iter()
                    .chain(&scope.middleware)
                    .cloned()
                    .collect(),
                namespace: compose_namespace(
                    include.namespace.as_deref(),
                    scope.namespace.as_deref(),
                ),
            })
            .collect()
    }

    /// The controller method a `Class@method` action names. `symbol_table`
    /// holds Pass 1.9's resolved supertypes, for a method the class
    /// inherits.
    pub fn controller_action(
        &self,
        action: &str,
        namespace: Option<&str>,
        symbol_table: &SymbolTable,
    ) -> Option<ControllerAction> {
        self.controllers.resolve(action, namespace, symbol_table)
    }
}

/// Each included file's scopes, composed through every chain of includes
/// that reaches it from a file nothing includes.
///
/// Only an admitted file's includes count. A file is admitted when it
/// imports an HTTP framework, or when an admitted file includes it: a
/// `require` in plain PHP code loads no route file.
fn include_scopes(local_graphs: &[LocalGraph]) -> FxHashMap<u32, Vec<IncludeScope>> {
    let paths: Vec<String> = local_graphs
        .iter()
        .map(|graph| graph.file_path.to_string_lossy().replace('\\', "/"))
        .collect();
    let by_path: FxHashMap<&str, u32> = paths
        .iter()
        .enumerate()
        .map(|(idx, path)| (path.as_str(), idx as u32))
        .collect();
    let mut includes: Vec<(u32, u32, IncludeScope)> = Vec::new();
    let mut admitted: Vec<u32> = Vec::new();
    for (from, graph) in local_graphs.iter().enumerate() {
        let from = from as u32;
        if graph.routes.iter().any(|route| {
            route
                .scope
                .as_deref()
                .is_some_and(|scope| !scope.needs_include)
        }) {
            admitted.push(from);
        }
        for record in graph
            .routes
            .iter()
            .filter(|route| route.method == RawRoute::INCLUDE)
        {
            let Some(scope) = record.scope.as_deref() else {
                continue;
            };
            let Some(target) = scope
                .include
                .as_ref()
                .and_then(|include| resolve_include(include, &paths[from as usize], &by_path))
            else {
                continue;
            };
            if target == from {
                continue;
            }
            let include = IncludeScope {
                prefix: record.path.clone(),
                middleware: scope.middleware.clone(),
                namespace: scope.namespace.clone(),
            };
            includes.push((from, target, include));
        }
    }
    let mut is_admitted: FxHashSet<u32> = admitted.iter().copied().collect();
    while let Some(file) = admitted.pop() {
        for &(_, target, _) in includes.iter().filter(|(from, ..)| *from == file) {
            if is_admitted.insert(target) {
                admitted.push(target);
            }
        }
    }
    let mut into: FxHashMap<u32, Vec<(u32, IncludeScope)>> = FxHashMap::default();
    for (from, target, include) in includes {
        if is_admitted.contains(&from) {
            into.entry(target).or_default().push((from, include));
        }
    }
    let mut memo = FxHashMap::default();
    let targets: Vec<u32> = into.keys().copied().collect();
    for target in targets {
        scopes_of(target, &into, &mut memo, &mut Vec::new());
    }
    memo
}
fn scopes_of(
    file: u32,
    into: &FxHashMap<u32, Vec<(u32, IncludeScope)>>,
    memo: &mut FxHashMap<u32, Vec<IncludeScope>>,
    stack: &mut Vec<u32>,
) -> Vec<IncludeScope> {
    if let Some(scopes) = memo.get(&file) {
        return scopes.clone();
    }
    let Some(edges) = into.get(&file) else {
        return vec![IncludeScope::default()];
    };
    if stack.contains(&file) {
        return Vec::new();
    }
    stack.push(file);
    let mut scopes: Vec<IncludeScope> = Vec::new();
    for (from, inner) in edges {
        for outer in scopes_of(*from, into, memo, stack) {
            let scope = outer.then(inner);
            if scopes.len() < MAX_INCLUDE_SCOPES && !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
    }
    stack.pop();
    // Every include of the file came back round a cycle: nothing outside
    // the cycle loads it, so its own paths are all there is.
    if scopes.is_empty() {
        scopes.push(IncludeScope::default());
    }
    memo.insert(file, scopes.clone());
    scopes
}

/// The file index an include names, seen from the including file `from`.
/// `base_path()` is the application root, the nearest directory at or
/// above `from` that holds the named path; a repo may hold several apps.
fn resolve_include(
    include: &RouteInclude,
    from: &str,
    by_path: &FxHashMap<&str, u32>,
) -> Option<u32> {
    let dir = from.rsplit_once('/').map_or("", |(dir, _)| dir);
    match include {
        RouteInclude::FileDir(relative) => {
            let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
            for segment in relative.split('/') {
                match segment {
                    "" | "." => {}
                    ".." => {
                        parts.pop()?;
                    }
                    segment => parts.push(segment),
                }
            }
            by_path.get(parts.join("/").as_str()).copied()
        }
        RouteInclude::AppRoot(relative) => {
            let mut root = dir;
            loop {
                let candidate = if root.is_empty() {
                    relative.clone()
                } else {
                    format!("{root}/{relative}")
                };
                if let Some(&idx) = by_path.get(candidate.as_str()) {
                    return Some(idx);
                }
                if root.is_empty() {
                    return None;
                }
                root = root.rsplit_once('/').map_or("", |(parent, _)| parent);
            }
        }
    }
}

/// PHP classes by short name, for a route action's controller, and the
/// traits they use. Class and method names are case-insensitive in PHP, so
/// keys are lower-cased.
#[derive(Default)]
struct ControllerIndex {
    /// Short class name → indices into `types`. Traits are not listed: a
    /// route never names one.
    by_name: FxHashMap<String, Vec<usize>>,
    /// Class or trait node index → index into `types`.
    by_node: FxHashMap<u32, usize>,
    types: Vec<ControllerType>,
}

struct ControllerType {
    node: u32,
    is_trait: bool,
    /// Lower-cased, without a leading `\`.
    fqn: String,
    /// Lower-cased method name → node index; the first declaration wins.
    methods: FxHashMap<String, u32>,
}

impl ControllerIndex {
    /// Node indices are the builder's: each file's raw nodes in file order,
    /// one after another from 0.
    fn build(local_graphs: &[LocalGraph]) -> Self {
        let mut index = Self::default();
        let mut start = 0u32;
        for graph in local_graphs {
            let base = start;
            start += graph.nodes.len() as u32;
            let is_php = ecp_core::analyzer::pipeline::AnalyzerPipeline::provider_name_for_path(
                &graph.file_path,
            ) == Some("php");
            if !is_php {
                continue;
            }
            let namespaces: Vec<(u32, &str)> = graph
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Namespace)
                .map(|node| (node.span.0, node.name.as_str()))
                .collect();
            let first = index.types.len();
            let mut spans: Vec<(&str, (u32, u32, u32, u32))> = Vec::new();
            for (local_idx, node) in graph.nodes.iter().enumerate() {
                let is_trait = match node.kind {
                    NodeKind::Class => false,
                    NodeKind::Trait => true,
                    _ => continue,
                };
                let namespace = namespaces
                    .iter()
                    .rev()
                    .find(|(row, _)| *row <= node.span.0)
                    .map(|(_, namespace)| *namespace);
                let fqn = match namespace {
                    Some(namespace) => format!("{namespace}\\{}", node.name),
                    None => node.name.clone(),
                };
                let slot = index.types.len();
                let node_idx = base + local_idx as u32;
                index.by_node.insert(node_idx, slot);
                if !is_trait {
                    index
                        .by_name
                        .entry(node.name.to_ascii_lowercase())
                        .or_default()
                        .push(slot);
                }
                index.types.push(ControllerType {
                    node: node_idx,
                    is_trait,
                    fqn: fqn.to_ascii_lowercase(),
                    methods: FxHashMap::default(),
                });
                spans.push((node.name.as_str(), node.span));
            }
            for (local_idx, node) in graph.nodes.iter().enumerate() {
                if node.kind != NodeKind::Method {
                    continue;
                }
                let Some(owner) = node.owner_class.as_deref() else {
                    continue;
                };
                let Some(offset) = spans.iter().position(|(name, span)| {
                    *name == owner && span.0 <= node.span.0 && node.span.2 <= span.2
                }) else {
                    continue;
                };
                index.types[first + offset]
                    .methods
                    .entry(node.name.to_ascii_lowercase())
                    .or_insert(base + local_idx as u32);
            }
        }
        index
    }

    /// A namespace in scope, or a leading `\`, pins the class to one fully
    /// qualified name. Without either, the class name must match exactly one
    /// class. No class, two classes, or a class that neither declares nor
    /// inherits the method: `None`.
    fn resolve(
        &self,
        action: &str,
        namespace: Option<&str>,
        symbol_table: &SymbolTable,
    ) -> Option<ControllerAction> {
        let (class, method) = action.rsplit_once('@')?;
        let short = class.rsplit('\\').next()?.to_ascii_lowercase();
        let candidates = self.by_name.get(&short)?;
        let qualified = match (class.strip_prefix('\\'), namespace) {
            (Some(absolute), _) => Some(absolute.to_string()),
            (None, Some(namespace)) => Some(format!(
                "{}\\{class}",
                namespace.trim_start_matches('\\').trim_end_matches('\\')
            )),
            (None, None) => None,
        };
        let matches: Vec<usize> = match &qualified {
            Some(fqn) => {
                let fqn = fqn.to_ascii_lowercase();
                candidates
                    .iter()
                    .copied()
                    .filter(|&c| self.types[c].fqn == fqn)
                    .collect()
            }
            None => {
                let class = class.to_ascii_lowercase();
                let suffix = format!("\\{class}");
                candidates
                    .iter()
                    .copied()
                    .filter(|&c| self.types[c].fqn == class || self.types[c].fqn.ends_with(&suffix))
                    .collect()
            }
        };
        let [class] = matches.as_slice() else {
            return None;
        };
        let (node, inherited) =
            self.method_of(*class, &method.to_ascii_lowercase(), symbol_table)?;
        Some(ControllerAction {
            node,
            by_namespace: qualified.is_some(),
            inherited,
        })
    }

    /// The method `method` of class `class` as PHP looks it up: the class's
    /// own, then a used trait's, then the parent class's, by the same
    /// rule. A step that is not exactly one resolved type stops the walk:
    /// two traits that declare the method, two classes a parent name
    /// resolves to, or a parent outside the graph.
    fn method_of(
        &self,
        mut class: usize,
        method: &str,
        symbol_table: &SymbolTable,
    ) -> Option<(u32, bool)> {
        for depth in 0..MAX_HERITAGE_DEPTH {
            if let Some(&node) = self.types[class].methods.get(method) {
                return Some((node, depth > 0));
            }
            let bases: Vec<usize> = symbol_table
                .supertypes(self.types[class].node)
                .map(|st| {
                    st.bases
                        .iter()
                        .filter_map(|base| self.by_node.get(base).copied())
                        .collect()
                })
                .unwrap_or_default();
            let (traits, parents): (Vec<usize>, Vec<usize>) = bases
                .into_iter()
                .partition(|&base| self.types[base].is_trait);
            let from_traits: Vec<u32> = traits
                .iter()
                .filter_map(|&t| self.types[t].methods.get(method).copied())
                .collect();
            match from_traits.as_slice() {
                [node] => return Some((*node, true)),
                [] => {}
                _ => return None,
            }
            let [parent] = parents.as_slice() else {
                return None;
            };
            class = *parent;
        }
        None
    }
}
