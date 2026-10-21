//! Laravel route groups: the prefix, middleware, controller namespace and
//! controller class that enclosing `Route::group([...], fn)` /
//! `Route::prefix('p')->group(fn)` calls apply to a route, the middleware
//! they remove, the controller action a route names, the middleware a
//! controller registers for its own actions, the routes a
//! `Route::resource` / `match` / `any` call registers, and the route files
//! a group loads.
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
use ecp_core::analyzer::types::{
    LocalGraph, RawImport, RawRoute, RawRouteScope, RouteInclude, Span,
};
use ecp_core::graph::NodeKind;
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

/// Include scopes a file can reach through chains of includes. Past this, a
/// file's routes stop multiplying: each scope is one more Route node per route.
/// A file past it gets a [`BS_KIND_INCLUDE_SCOPE_CAP`] BlindSpot.
const MAX_INCLUDE_SCOPES: usize = 16;

/// BlindSpot `kind` of a route file whose routes lost include scopes to
/// [`MAX_INCLUDE_SCOPES`].
pub const BS_KIND_INCLUDE_SCOPE_CAP: &str = "laravel-include-scope-cap";

/// What one group level, or a route's own call chain, sets.
#[derive(Default)]
struct GroupAttrs {
    prefix: Option<String>,
    middleware: Vec<String>,
    /// `withoutMiddleware` / `excluded_middleware`.
    excluded: Vec<String>,
    namespace: Option<String>,
    controller: Option<String>,
}

/// The group levels around `node`, outermost first. A level is a `group`
/// call on the router whose closure holds `node`.
fn enclosing_groups(node: Node, source: &[u8], names: FileNames) -> Vec<GroupAttrs> {
    let mut levels = Vec::new();
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        if matches!(parent.kind(), "anonymous_function" | "arrow_function") {
            if let Some(call) = closure_group_call(parent, source) {
                levels.push(group_call_attrs(call, source, names));
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

fn group_call_attrs(call: Node, source: &[u8], names: FileNames) -> GroupAttrs {
    let mut attrs = GroupAttrs::default();
    if let Some(object) = call.child_by_field_name("object") {
        chain_attrs(object, source, names, &mut attrs);
    }
    if let Some(options) = nth_argument(call, 0).filter(|n| n.kind() == "array_creation_expression")
    {
        option_attrs(options, source, names, &mut attrs);
    }
    attrs
}

/// Read `Route::prefix('p')->middleware('m')->...` from the root call
/// outward, so a later call of the same attribute replaces an earlier one,
/// as Laravel's `RouteRegistrar` does.
fn chain_attrs(mut node: Node, source: &[u8], names: FileNames, attrs: &mut GroupAttrs) {
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
        match call_name(call, source) {
            Some("middleware") => attrs.middleware = variadic_middleware(call, source, names),
            // `RouteRegistrar` adds each `withoutMiddleware` to the earlier ones.
            Some("withoutMiddleware") => {
                attrs.excluded.extend(first_middleware(call, source, names))
            }
            Some(name) => {
                if let Some(value) = nth_argument(call, 0) {
                    set_attr(attrs, name, value, source, names);
                }
            }
            None => {}
        }
    }
}

/// `Route::group(['prefix' => 'p', 'middleware' => [...]], ...)`.
/// `Router::group` merges the array as written, so `excluded_middleware`
/// removes middleware and a `withoutMiddleware` key, the registrar method's
/// name, does nothing.
fn option_attrs(options: Node, source: &[u8], names: FileNames, attrs: &mut GroupAttrs) {
    for (key, value) in array_pairs(options, source) {
        match key.as_str() {
            "excluded_middleware" => attrs.excluded.extend(middleware_list(value, source, names)),
            _ => set_attr(attrs, &key, value, source, names),
        }
    }
}

fn set_attr(attrs: &mut GroupAttrs, name: &str, value: Node, source: &[u8], names: FileNames) {
    match name {
        "prefix" => attrs.prefix = string_value(value, source),
        "middleware" => attrs.middleware = middleware_list(value, source, names),
        "namespace" => attrs.namespace = namespace_value(value, source),
        "controller" => attrs.controller = class_name(value, source),
        _ => {}
    }
}

/// `->middleware('auth')` / `->withoutMiddleware('auth')` calls chained
/// after the verb call: `Route::get('/x', ...)->middleware('auth')`.
fn trailing_attrs(call: Node, source: &[u8], names: FileNames, own: &mut GroupAttrs) {
    let mut cur = call;
    while let Some(parent) = cur
        .parent()
        .filter(|p| p.kind() == "member_call_expression")
        .filter(|p| p.child_by_field_name("object") == Some(cur))
    {
        match call_name(parent, source) {
            Some("middleware") => own
                .middleware
                .extend(variadic_middleware(parent, source, names)),
            Some("withoutMiddleware") => {
                own.excluded.extend(first_middleware(parent, source, names))
            }
            _ => {}
        }
        cur = parent;
    }
}

/// The middleware of a variadic call, as `Route::middleware` and
/// `RouteRegistrar` read them: an array first argument is the list, else
/// each argument is one name.
fn variadic_middleware(call: Node, source: &[u8], names: FileNames) -> Vec<String> {
    let values: Vec<Node> = arguments(call)
        .into_iter()
        .filter_map(argument_value)
        .collect();
    match values.as_slice() {
        [first, ..] if first.kind() == "array_creation_expression" => {
            middleware_list(*first, source, names)
        }
        many => many
            .iter()
            .filter_map(|v| middleware_name(*v, source, names))
            .collect(),
    }
}

/// The middleware of the first argument: `'auth'` or `['auth', 'verified']`.
fn first_middleware(call: Node, source: &[u8], names: FileNames) -> Vec<String> {
    nth_argument(call, 0).map_or_else(Vec::new, |value| middleware_list(value, source, names))
}

/// The scope of a route or include site: each enclosing group, then `own`.
struct SiteScope {
    prefixes: Vec<String>,
    scope: RawRouteScope,
    controller: Option<String>,
}

fn site_scope(node: Node, own: GroupAttrs, source: &[u8], names: FileNames) -> SiteScope {
    let mut levels = enclosing_groups(node, source, names);
    levels.push(own);
    let mut site = SiteScope {
        prefixes: Vec::new(),
        scope: RawRouteScope::default(),
        controller: None,
    };
    for level in levels {
        site.prefixes.extend(level.prefix);
        site.scope.middleware.extend(level.middleware);
        site.scope.excluded_middleware.extend(level.excluded);
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
fn call_scope(call: Node, source: &[u8], names: FileNames) -> SiteScope {
    let mut own = GroupAttrs::default();
    if let Some(object) = call.child_by_field_name("object") {
        chain_attrs(object, source, names, &mut own);
    }
    trailing_attrs(call, source, names, &mut own);
    site_scope(call, own, source, names)
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

    /// The string `X::class` evaluates to: the class [`Self::resolve`]
    /// names, without the leading `\`. `self`, `static` and `parent` name
    /// a class only the enclosing class decides.
    fn class_value(&self, name: &str) -> Option<String> {
        if ["self", "static", "parent"]
            .iter()
            .any(|keyword| name.eq_ignore_ascii_case(keyword))
        {
            return None;
        }
        Some(self.resolve(name).trim_start_matches('\\').to_string())
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
    let site = call_scope(call, source, names);
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
    let site = call_scope(call, source, names);
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
///
/// `Invokable::class` names the class's `__invoke`, never a method of the
/// group controller: `prependGroupController` keeps a name `class_exists`
/// finds. A string action without `@` (`'list'`) stays as written when no
/// group in the file sets a controller: a group in an including file may
/// set one, so [`LaravelRoutes::promote`] completes it.
fn route_action(
    action: Node,
    controller: Option<&str>,
    source: &[u8],
    names: FileNames,
) -> Option<String> {
    let text = match action.kind() {
        "string" | "encapsed_string" => string_value(action, source)?,
        "class_constant_access_expression" => {
            let class = names.resolve(class_constant_class(action, source)?);
            return Some(format!("{class}@__invoke"));
        }
        "array_creation_expression" => {
            let mut values = array_values(action);
            let class = values.next()?;
            let method = string_value(values.next()?, source)?;
            let class = match class_constant_class(class, source) {
                Some(class) => names.resolve(class),
                None => string_value(class, source)?,
            };
            return Some(format!("{class}@{method}"));
        }
        _ => return None,
    };
    Some(match controller {
        Some(class) => complete_action(&text, Some(class)),
        None => text,
    })
}

/// Laravel's `prependGroupController`, then `RouteAction::makeInvokable`:
/// an action without `@` names a method of the group controller, else the
/// invokable class it names. Laravel keeps a name of an existing class as
/// a class; a method name never holds a `\`, so a name with one is a class.
fn complete_action(action: &str, controller: Option<&str>) -> String {
    match controller {
        _ if action.contains('@') => action.to_string(),
        Some(class) if !action.contains('\\') => format!("{class}@{action}"),
        _ => format!("{action}@__invoke"),
    }
}

/// Append a [`RawRoute::INCLUDE`] record for each route file `node` loads:
/// `->group(<file>)` or `Route::group([...], <file>)` on the router, the
/// Laravel 11 `->withRouting(web: <file>, api: <file>)` builder, or a
/// `require <file>`. A `group` whose argument is a closure loads no file.
pub(super) fn push_includes(node: Node, source: &[u8], names: FileNames, out: &mut Vec<RawRoute>) {
    match node.kind() {
        "member_call_expression" | "scoped_call_expression" => match call_name(node, source) {
            Some("group") if is_router_call(node, source) => {
                let Some(target) = last_argument(node).and_then(|arg| include_target(arg, source))
                else {
                    return;
                };
                let attrs = group_call_attrs(node, source, names);
                push_include(node, target, attrs, source, names, out);
            }
            Some("withRouting") => push_with_routing(node, source, names, out),
            _ => {}
        },
        _ => {
            if let Some(target) = node
                .named_child(0)
                .and_then(|path| include_target(path, source))
            {
                push_include(node, target, GroupAttrs::default(), source, names, out);
            }
        }
    }
}

/// `->withRouting(web: ..., api: ..., apiPrefix: 'api')`: Laravel 11 loads
/// `web` files under the `web` middleware group and `api` files under the
/// `api` middleware group and prefix.
fn push_with_routing(call: Node, source: &[u8], names: FileNames, out: &mut Vec<RawRoute>) {
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
            push_include(file, target, attrs, source, names, out);
        }
    }
}

fn push_include(
    node: Node,
    target: RouteInclude,
    own: GroupAttrs,
    source: &[u8],
    names: FileNames,
    out: &mut Vec<RawRoute>,
) {
    let site = site_scope(node, own, source, names);
    let mut scope = site.scope;
    scope.include = Some(target);
    out.push(RawRoute {
        method: RawRoute::INCLUDE.to_string(),
        path: join_prefix(site.prefixes.iter().map(String::as_str)),
        // As written; [`resolve_include_controllers`] qualifies it once the
        // file's imports are known.
        handler: site.controller,
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

/// The most directories a `dirname(__DIR__, n)` include climbs.
const MAX_DIR_LEVELS: usize = 64;

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
            // PHP rejects 0 levels; past `MAX_DIR_LEVELS` no route file
            // lies, and an unchecked count overflows the path built from it.
            let levels = match nth_argument(expr, 1) {
                Some(n) => n
                    .utf8_text(source)
                    .ok()?
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=MAX_DIR_LEVELS).contains(n))?,
                None => 1,
            };
            inner
                .checked_add(levels)
                .filter(|total| *total <= MAX_DIR_LEVELS)
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

/// A middleware value: `'auth'`, `['auth', 'verified']`, or classes
/// (`[Ensure::class]`); non-literal entries dropped.
fn middleware_list(node: Node, source: &[u8], names: FileNames) -> Vec<String> {
    match node.kind() {
        "array_creation_expression" => array_values(node)
            .filter_map(|value| middleware_name(value, source, names))
            .collect(),
        _ => middleware_name(node, source, names).into_iter().collect(),
    }
}

/// `'auth'`, or the class `Ensure::class` names in its own file, the
/// string Laravel compares: `withoutMiddleware(Guard::class)` in a route
/// file and `Guard::class` in a controller name one class only when both
/// files resolve `Guard` to it.
fn middleware_name(node: Node, source: &[u8], names: FileNames) -> Option<String> {
    string_value(node, source)
        .or_else(|| class_constant_class(node, source).and_then(|class| names.class_value(class)))
}

/// A list value: `'a'`, `['a', 'b']`, or class names (`[C::class]`), kept
/// as written; non-literal entries dropped.
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
    excluded: Vec<String>,
    namespace: Option<String>,
    /// `Route::controller(C::class)->group(<file>)`: a bare action of the
    /// file names a method of `C`.
    controller: Option<String>,
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
            excluded: self
                .excluded
                .iter()
                .chain(&inner.excluded)
                .cloned()
                .collect(),
            namespace: compose_namespace(self.namespace.as_deref(), inner.namespace.as_deref()),
            // `RouteGroup::merge`: an inner group's controller replaces the
            // outer one.
            controller: inner.controller.clone().or_else(|| self.controller.clone()),
        }
    }

    /// The Route node a route with `scope`, checked `path` and action
    /// `handler` becomes under this scope.
    fn promote(&self, scope: &RawRouteScope, path: String, handler: Option<&str>) -> PromotedRoute {
        let excluded: Vec<String> = self
            .excluded
            .iter()
            .chain(&scope.excluded_middleware)
            .cloned()
            .collect();
        let mut middleware: Vec<String> = Vec::new();
        for name in self.middleware.iter().chain(&scope.middleware) {
            if !middleware.contains(name) && !excluded.contains(name) {
                middleware.push(name.clone());
            }
        }
        PromotedRoute {
            path,
            middleware,
            excluded,
            namespace: compose_namespace(self.namespace.as_deref(), scope.namespace.as_deref()),
            action: handler.map(|action| complete_action(action, self.controller.as_deref())),
        }
    }
}

/// One Route node a Laravel route becomes under one include scope.
pub struct PromotedRoute {
    pub path: String,
    /// Including groups' middleware, then the file's groups', then the
    /// route's own: each name once, at its first place, and none that
    /// `excluded` names. Laravel's `uniqueMiddleware`, then the exact-name
    /// match of `resolveMiddleware`, before it expands middleware groups.
    pub middleware: Vec<String>,
    /// Middleware the route's groups remove, controller middleware too.
    pub excluded: Vec<String>,
    pub namespace: Option<String>,
    /// The `Class@method` action, a bare one completed with an including
    /// group's controller.
    pub action: Option<String>,
}

/// The controller method a route action binds.
#[derive(Debug, PartialEq)]
pub struct ControllerAction {
    pub node: u32,
    /// A namespace, or the class name written fully qualified, pinned the
    /// class; else the class name matched one class.
    pub by_namespace: bool,
    /// The class does not declare the method: a used trait or a parent
    /// class, reached through resolved heritage, does.
    pub inherited: bool,
    /// The middleware the controller registers for the action's method, in
    /// the order Laravel runs the registrations; `Err` with what kept it
    /// from being statically certain.
    pub middleware: Result<Vec<String>, Vec<UnreadMiddleware>>,
}

/// Controller middleware that only the runtime decides: a registration
/// under a branch, a value only the runtime knows, or a parent, trait or
/// interface outside the graph that could change which code runs. The
/// routes the controller handles then carry none of its middleware.
#[derive(Debug, Clone, PartialEq)]
pub struct UnreadMiddleware {
    /// The file and place of the code that decides.
    pub file_idx: u32,
    pub span: Span,
    pub hint: String,
}

/// BlindSpot `kind` of an [`UnreadMiddleware`].
pub const BS_KIND_CONTROLLER_MIDDLEWARE: &str = "laravel-controller-middleware-dynamic";

/// The cross-file half of Laravel routing: the include scopes of each
/// route file, and the controller classes route actions name.
#[derive(Default)]
pub struct LaravelRoutes {
    /// Keyed by the included file's index; a file nothing includes has no
    /// entry and keeps the paths its own file shows.
    include_scopes: FxHashMap<u32, Vec<IncludeScope>>,
    /// Files whose routes lost include scopes to [`MAX_INCLUDE_SCOPES`].
    capped: Vec<CappedIncludes>,
    controllers: ControllerIndex,
}

/// A route file that more include scopes reach than [`MAX_INCLUDE_SCOPES`].
pub struct CappedIncludes {
    pub file_idx: u32,
    /// The scopes this file dropped itself.
    pub dropped: usize,
    /// A file that includes it dropped scopes too, so more are missing.
    pub upstream: bool,
}

impl CappedIncludes {
    pub fn hint(&self) -> String {
        let mut hint = if self.dropped > 0 {
            format!(
                "{} more include scopes reach this route file than the {MAX_INCLUDE_SCOPES} \
                 expanded; its routes under them have no Route node",
                self.dropped
            )
        } else {
            format!(
                "a file that includes this route file passed the {MAX_INCLUDE_SCOPES} \
                 include-scope cap; its routes under the dropped scopes have no Route node"
            )
        };
        if self.dropped > 0 && self.upstream {
            hint.push_str("; a file that includes it was capped too, so more are missing");
        }
        hint
    }
}

impl LaravelRoutes {
    pub fn build(local_graphs: &[LocalGraph]) -> Self {
        let routes = || local_graphs.iter().flat_map(|graph| &graph.routes);
        let has_includes = routes().any(|route| route.method == RawRoute::INCLUDE);
        let has_actions = routes().any(|route| {
            route.scope.is_some()
                && route.handler.is_some()
                && route.method != RawRoute::INCLUDE
                && route.method != RawRoute::CONTROLLER_MIDDLEWARE
        });
        let (include_scopes, capped) = if has_includes {
            include_scopes(local_graphs)
        } else {
            Default::default()
        };
        Self {
            include_scopes,
            capped,
            controllers: if has_actions {
                ControllerIndex::build(local_graphs)
            } else {
                ControllerIndex::default()
            },
        }
    }

    /// The Route nodes a route of file `file_idx` with `scope`, checked
    /// `path` and action `handler` becomes, one per include scope that
    /// reaches the file. A route of a file without a framework import
    /// becomes none unless a file with one loads its file.
    pub fn promote(
        &self,
        file_idx: u32,
        scope: &RawRouteScope,
        path: &str,
        handler: Option<&str>,
    ) -> Vec<PromotedRoute> {
        let Some(includes) = self.include_scopes.get(&file_idx) else {
            if scope.needs_include {
                return Vec::new();
            }
            return vec![IncludeScope::default().promote(scope, path.to_string(), handler)];
        };
        includes
            .iter()
            .map(|include| {
                let path = join_route_path([include.prefix.as_str(), path]);
                include.promote(scope, path, handler)
            })
            .collect()
    }

    /// The route files whose routes lost include scopes to the cap, by
    /// file index.
    pub fn capped_includes(&self) -> &[CappedIncludes] {
        &self.capped
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
/// that reaches it from a file nothing includes, and the files past
/// [`MAX_INCLUDE_SCOPES`].
///
/// Only an admitted file's includes count. A file is admitted when it
/// imports an HTTP framework, or when an admitted file includes it: a
/// `require` in plain PHP code loads no route file.
fn include_scopes(
    local_graphs: &[LocalGraph],
) -> (FxHashMap<u32, Vec<IncludeScope>>, Vec<CappedIncludes>) {
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
            route.method != RawRoute::CONTROLLER_MIDDLEWARE
                && route
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
                excluded: scope.excluded_middleware.clone(),
                namespace: scope.namespace.clone(),
                controller: record.handler.clone(),
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
    let mut walk = ScopeWalk {
        into: &into,
        memo: FxHashMap::default(),
        lossy: FxHashMap::default(),
        stack: Vec::new(),
    };
    let targets: Vec<u32> = into.keys().copied().collect();
    for target in targets {
        walk.scopes_of(target);
    }
    let mut capped: Vec<CappedIncludes> = walk
        .lossy
        .into_iter()
        .map(|(file_idx, (dropped, upstream))| CappedIncludes {
            file_idx,
            dropped,
            upstream,
        })
        .collect();
    capped.sort_unstable_by_key(|cap| cap.file_idx);
    (walk.memo, capped)
}

struct ScopeWalk<'a> {
    into: &'a FxHashMap<u32, Vec<(u32, IncludeScope)>>,
    memo: FxHashMap<u32, Vec<IncludeScope>>,
    /// File → (scopes it dropped itself, an including file dropped some).
    lossy: FxHashMap<u32, (usize, bool)>,
    stack: Vec<u32>,
}

impl ScopeWalk<'_> {
    fn scopes_of(&mut self, file: u32) -> Vec<IncludeScope> {
        if let Some(scopes) = self.memo.get(&file) {
            return scopes.clone();
        }
        let into = self.into;
        let Some(edges) = into.get(&file) else {
            return vec![IncludeScope::default()];
        };
        if self.stack.contains(&file) {
            return Vec::new();
        }
        self.stack.push(file);
        // Each including file contributes at most MAX_INCLUDE_SCOPES, so
        // the list stays bounded before the cut below.
        let mut scopes: Vec<IncludeScope> = Vec::new();
        let mut upstream = false;
        for (from, inner) in edges {
            for outer in self.scopes_of(*from) {
                let scope = outer.then(inner);
                // A repeated scope keeps its last place: the builder lets the
                // last registration of a path win, and `api, web, api`
                // registers under `api` last.
                scopes.retain(|seen| *seen != scope);
                scopes.push(scope);
            }
            upstream |= self.lossy.contains_key(from);
        }
        self.stack.pop();
        let dropped = scopes.len().saturating_sub(MAX_INCLUDE_SCOPES);
        scopes.truncate(MAX_INCLUDE_SCOPES);
        if dropped > 0 || upstream {
            self.lossy.insert(file, (dropped, upstream));
        }
        // Every include of the file came back round a cycle: nothing outside
        // the cycle loads it, so its own paths are all there is.
        if scopes.is_empty() {
            scopes.push(IncludeScope::default());
        }
        self.memo.insert(file, scopes.clone());
        scopes
    }
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
    /// Fully qualified class or trait name → indices into `types`, for the
    /// `extends` and `use` a controller's middleware runs through.
    by_fqn: FxHashMap<String, Vec<usize>>,
    /// Class or trait node index → index into `types`.
    by_node: FxHashMap<u32, usize>,
    types: Vec<ControllerType>,
}

struct ControllerType {
    node: u32,
    file_idx: u32,
    is_trait: bool,
    /// Lower-cased, without a leading `\`.
    fqn: String,
    /// Lower-cased method name → node index; the first declaration wins.
    methods: FxHashMap<String, u32>,
    has_constructor: bool,
    /// What the constructor does with middleware, in the order it runs.
    constructor: Vec<ConstructorStep>,
    /// The entries of a static `middleware()`; `Some` when the type
    /// declares one, even with no entry this reads.
    static_middleware: Option<Vec<StaticEntry>>,
    /// The `extends` class, fully qualified, and where the class names it.
    parent: Option<(String, Span)>,
    /// The `implements` interfaces, likewise.
    interfaces: Vec<(String, Span)>,
    /// The used traits, likewise.
    traits: Vec<(String, Span)>,
}

/// One step of a constructor that bears on middleware.
enum ConstructorStep {
    Register(MiddlewareRule),
    /// `parent::__construct()`: the parent's constructor runs here.
    Parent,
    /// A registration this cannot read, and why.
    Unread(Span, String),
}

/// One entry of a static `middleware()`.
enum StaticEntry {
    Register(MiddlewareRule),
    Unread(Span, String),
}

/// One controller-middleware registration and the methods it covers.
struct MiddlewareRule {
    names: Vec<String>,
    only: Option<Vec<String>>,
    except: Option<Vec<String>>,
}

impl MiddlewareRule {
    fn none() -> Self {
        MiddlewareRule {
            names: Vec::new(),
            only: None,
            except: None,
        }
    }

    /// The negation of Laravel's `methodExcludedByOptions`: an `only` list
    /// names the method, and no non-empty `except` list does. The names
    /// compare as written, as `in_array` does.
    fn covers(&self, method: &str) -> bool {
        self.only
            .as_ref()
            .is_none_or(|only| only.iter().any(|m| m == method))
            && !self
                .except
                .as_ref()
                .is_some_and(|except| except.iter().any(|m| m == method))
    }
}

/// The type whose constructor or static `middleware()` PHP runs.
enum Supplier {
    None,
    /// The type `ty`, declared by or used in `chain[at]`.
    Found {
        ty: usize,
        at: usize,
    },
    Unread(UnreadMiddleware),
}

/// Whether span `inner` lies within span `outer`.
fn span_within(inner: Span, outer: Span) -> bool {
    (outer.0, outer.1) <= (inner.0, inner.1) && (inner.2, inner.3) <= (outer.2, outer.3)
}

/// Laravel's own types outside the graph register no controller middleware:
/// `Illuminate\Routing\Controller` and the `AuthorizesRequests`-style
/// traits declare neither a constructor nor a static `middleware()`.
fn is_framework(fqn: &str) -> bool {
    fqn.get(..11)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("illuminate\\"))
}

impl ControllerIndex {
    /// Node indices are the builder's: each file's raw nodes in file order,
    /// one after another from 0.
    fn build(local_graphs: &[LocalGraph]) -> Self {
        let mut index = Self::default();
        let mut start = 0u32;
        for (file_idx, graph) in local_graphs.iter().enumerate() {
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
            let mut spans: Vec<(&str, Span)> = Vec::new();
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
                }
                .to_ascii_lowercase();
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
                index.by_fqn.entry(fqn.clone()).or_default().push(slot);
                index.types.push(ControllerType {
                    node: node_idx,
                    file_idx: file_idx as u32,
                    is_trait,
                    fqn,
                    methods: FxHashMap::default(),
                    has_constructor: false,
                    constructor: Vec::new(),
                    static_middleware: None,
                    parent: None,
                    interfaces: Vec::new(),
                    traits: Vec::new(),
                });
                spans.push((node.name.as_str(), node.span));
            }
            for (local_idx, node) in graph.nodes.iter().enumerate() {
                if !matches!(node.kind, NodeKind::Method | NodeKind::Constructor) {
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
                let owner = &mut index.types[first + offset];
                if node.kind == NodeKind::Constructor {
                    owner.has_constructor = true;
                    continue;
                }
                owner
                    .methods
                    .entry(node.name.to_ascii_lowercase())
                    .or_insert(base + local_idx as u32);
            }
            for record in graph
                .routes
                .iter()
                .filter(|route| route.method == RawRoute::CONTROLLER_MIDDLEWARE)
            {
                let Some(offset) = spans
                    .iter()
                    .position(|(_, span)| span_within(record.span, *span))
                else {
                    continue;
                };
                let owner = &mut index.types[first + offset];
                let (marker, payload) = record
                    .path
                    .split_once(' ')
                    .unwrap_or((record.path.as_str(), ""));
                let rule = || {
                    record.scope.as_deref().map(|scope| MiddlewareRule {
                        names: scope.middleware.clone(),
                        only: scope.only.clone(),
                        except: scope.except.clone(),
                    })
                };
                let named = || (payload.to_string(), record.span);
                match marker {
                    FROM_CONSTRUCTOR => {
                        owner.has_constructor = true;
                        owner
                            .constructor
                            .extend(rule().map(ConstructorStep::Register));
                    }
                    CALLS_PARENT_CONSTRUCTOR => {
                        owner.has_constructor = true;
                        owner.constructor.push(ConstructorStep::Parent);
                    }
                    CONSTRUCTOR_UNREAD => {
                        owner.has_constructor = true;
                        owner
                            .constructor
                            .push(ConstructorStep::Unread(record.span, payload.to_string()));
                    }
                    FROM_STATIC => {
                        let entries = owner.static_middleware.get_or_insert_with(Vec::new);
                        entries.extend(
                            rule()
                                .filter(|rule| !rule.names.is_empty())
                                .map(StaticEntry::Register),
                        );
                    }
                    STATIC_UNREAD => owner
                        .static_middleware
                        .get_or_insert_with(Vec::new)
                        .push(StaticEntry::Unread(record.span, payload.to_string())),
                    EXTENDS => owner.parent = Some(named()),
                    IMPLEMENTS => owner.interfaces.push(named()),
                    USES => owner.traits.push(named()),
                    _ => {}
                }
            }
        }
        index
    }

    /// Laravel's `prependGroupNamespace` decides the class: a leading `\`,
    /// or a name that already starts with the namespace in scope, is taken
    /// as written; any other name gets the namespace. Without a namespace,
    /// a name equal to one class's fully qualified name pins that class;
    /// else the class name must match exactly one class. No class, two
    /// classes, or a class that neither declares nor inherits the method:
    /// `None`.
    fn resolve(
        &self,
        action: &str,
        namespace: Option<&str>,
        symbol_table: &SymbolTable,
    ) -> Option<ControllerAction> {
        let (class, method) = action.rsplit_once('@')?;
        let short = class.rsplit('\\').next()?.to_ascii_lowercase();
        let candidates = self.by_name.get(&short)?;
        let namespace = namespace.map(|ns| ns.trim_start_matches('\\').trim_end_matches('\\'));
        let qualified = match (class.strip_prefix('\\'), namespace) {
            (Some(absolute), _) => Some(absolute.to_string()),
            (None, Some(namespace)) if class.starts_with(namespace) => Some(class.to_string()),
            (None, Some(namespace)) => Some(format!("{namespace}\\{class}")),
            (None, None) => None,
        };
        let with_fqn = |fqn: &str| -> Vec<usize> {
            let fqn = fqn.to_ascii_lowercase();
            candidates
                .iter()
                .copied()
                .filter(|&c| self.types[c].fqn == fqn)
                .collect()
        };
        let (matches, by_namespace) = match &qualified {
            Some(fqn) => (with_fqn(fqn), true),
            None => match with_fqn(class).as_slice() {
                [exact] => (vec![*exact], true),
                _ => {
                    let suffix = format!("\\{}", class.to_ascii_lowercase());
                    let by_suffix: Vec<usize> = candidates
                        .iter()
                        .copied()
                        .filter(|&c| self.types[c].fqn.ends_with(&suffix))
                        .collect();
                    (by_suffix, false)
                }
            },
        };
        let [class] = matches.as_slice() else {
            return None;
        };
        let (node, inherited) =
            self.method_of(*class, &method.to_ascii_lowercase(), symbol_table)?;
        Some(ControllerAction {
            node,
            by_namespace,
            inherited,
            middleware: self.controller_middleware(*class, method),
        })
    }

    /// The resolved supertypes of `class` that the index holds.
    fn bases(&self, class: usize, symbol_table: &SymbolTable) -> Vec<usize> {
        symbol_table
            .supertypes(self.types[class].node)
            .map(|st| {
                st.bases
                    .iter()
                    .filter_map(|base| self.by_node.get(base).copied())
                    .collect()
            })
            .unwrap_or_default()
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
            let (traits, parents): (Vec<usize>, Vec<usize>) = self
                .bases(class, symbol_table)
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

    /// The one class or trait of the graph named `fqn`.
    fn type_named(&self, fqn: &str, is_trait: bool) -> Option<usize> {
        let found: Vec<usize> = self
            .by_fqn
            .get(&fqn.to_ascii_lowercase())?
            .iter()
            .copied()
            .filter(|&t| self.types[t].is_trait == is_trait)
            .collect();
        match found.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// `class`, then the class its `extends` names, and on up while each
    /// names exactly one class of the graph; and whether the last one's
    /// `extends` names a class this cannot follow.
    fn parent_chain(&self, class: usize) -> (Vec<usize>, bool) {
        let mut chain = vec![class];
        while chain.len() < MAX_HERITAGE_DEPTH {
            let Some((parent, _)) = &self.types[chain[chain.len() - 1]].parent else {
                return (chain, false);
            };
            match self.type_named(parent, false) {
                Some(parent) if !chain.contains(&parent) => chain.push(parent),
                _ => return (chain, true),
            }
        }
        (chain, true)
    }

    fn unread(&self, ty: usize, span: Span, hint: String) -> UnreadMiddleware {
        UnreadMiddleware {
            file_idx: self.types[ty].file_idx,
            span,
            hint,
        }
    }

    /// The parent `extends` names past the end of `chain`, when it may
    /// declare `what`.
    fn open_parent(&self, chain: &[usize], open: bool, what: &str) -> Option<UnreadMiddleware> {
        let last = *chain.last()?;
        let (parent, span) = self.types[last].parent.as_ref().filter(|_| open)?;
        (!is_framework(parent)).then(|| {
            self.unread(
                last,
                *span,
                format!("parent class `{parent}` is outside the graph and may declare {what}"),
            )
        })
    }

    /// The type whose `what` PHP runs for an object of `chain[from]`: the
    /// first class from there that declares it, else that class's used
    /// trait that does, then on up the parents. A trait or parent outside
    /// the graph may declare it, so the answer is unread.
    fn supplier(
        &self,
        chain: &[usize],
        open: bool,
        from: usize,
        declares: impl Fn(&ControllerType) -> bool,
        what: &str,
    ) -> Supplier {
        for (at, &class) in chain.iter().enumerate().skip(from) {
            let ty = &self.types[class];
            if declares(ty) {
                return Supplier::Found { ty: class, at };
            }
            let mut found: Vec<(usize, Span)> = Vec::new();
            for (name, span) in &ty.traits {
                match self.type_named(name, true) {
                    Some(t) if declares(&self.types[t]) => found.push((t, *span)),
                    Some(_) => {}
                    None if is_framework(name) => {}
                    None => {
                        return Supplier::Unread(self.unread(
                            class,
                            *span,
                            format!("trait `{name}` is outside the graph and may declare {what}"),
                        ))
                    }
                }
            }
            match found.as_slice() {
                [] => {}
                [(t, _)] => return Supplier::Found { ty: *t, at },
                [.., (_, span)] => {
                    return Supplier::Unread(self.unread(
                        class,
                        *span,
                        format!("two used traits declare {what}"),
                    ))
                }
            }
        }
        match self.open_parent(chain, open, what) {
            Some(unread) => Supplier::Unread(unread),
            None => Supplier::None,
        }
    }

    /// Whether `chain[0]` is a `HasMiddleware`, as `is_a` checks it: a class
    /// of the chain implements the interface. `Err` when an interface, or a
    /// parent outside the graph, may extend or implement it.
    fn has_middleware(&self, chain: &[usize], open: bool) -> Result<bool, UnreadMiddleware> {
        let mut doubt = None;
        for &class in chain {
            for (interface, span) in &self.types[class].interfaces {
                if interface.eq_ignore_ascii_case(HAS_MIDDLEWARE) {
                    return Ok(true);
                }
                if doubt.is_none() && !is_framework(interface) {
                    doubt = Some(self.unread(
                        class,
                        *span,
                        format!("interface `{interface}` may extend `HasMiddleware`"),
                    ));
                }
            }
        }
        match doubt.or_else(|| self.open_parent(chain, open, "`HasMiddleware`")) {
            Some(unread) => Err(unread),
            None => Ok(false),
        }
    }

    /// Laravel's `Route::controllerMiddleware` for an action of method
    /// `method` on `class`: a `HasMiddleware` class's static entries, else
    /// what the constructor PHP runs registers. `Err` when a step that
    /// decides it is not statically certain.
    fn controller_middleware(
        &self,
        class: usize,
        method: &str,
    ) -> Result<Vec<String>, Vec<UnreadMiddleware>> {
        let (chain, open) = self.parent_chain(class);
        let static_supplier = self.supplier(
            &chain,
            open,
            0,
            |ty| ty.static_middleware.is_some(),
            "a static `middleware()`",
        );
        let mut rules: Vec<&MiddlewareRule> = Vec::new();
        let mut unread: Vec<UnreadMiddleware> = Vec::new();
        match (self.has_middleware(&chain, open), static_supplier) {
            (Ok(true), Supplier::Found { ty, .. }) => {
                for entry in self.types[ty].static_middleware.iter().flatten() {
                    match entry {
                        StaticEntry::Register(rule) => rules.push(rule),
                        StaticEntry::Unread(span, why) => {
                            unread.push(self.unread(ty, *span, why.clone()))
                        }
                    }
                }
            }
            // PHP refuses a concrete `HasMiddleware` class without one.
            (Ok(true), Supplier::None) => {}
            (Ok(true), Supplier::Unread(why)) => unread.push(why),
            // The graph declares a static `middleware()`, and only what it
            // cannot see decides whether Laravel calls it.
            (Err(why), Supplier::Found { .. }) => unread.push(why),
            // Without a static `middleware()` in the graph, an interface or
            // parent outside it is taken for no `HasMiddleware`: a concrete
            // implementer must declare the method.
            (Ok(false) | Err(_), _) => {
                self.constructor_rules(&chain, open, 0, &mut rules, &mut unread)
            }
        }
        if !unread.is_empty() {
            return Err(unread);
        }
        let mut middleware: Vec<String> = Vec::new();
        for name in rules
            .into_iter()
            .filter(|rule| rule.covers(method))
            .flat_map(|rule| &rule.names)
        {
            if !middleware.contains(name) {
                middleware.push(name.clone());
            }
        }
        Ok(middleware)
    }

    /// The registrations of the constructor PHP runs for an object of
    /// `chain[from]`, in the order they run: a parent's inlined where
    /// `parent::__construct()` calls it.
    fn constructor_rules<'a>(
        &'a self,
        chain: &[usize],
        open: bool,
        from: usize,
        rules: &mut Vec<&'a MiddlewareRule>,
        unread: &mut Vec<UnreadMiddleware>,
    ) {
        match self.supplier(chain, open, from, |ty| ty.has_constructor, "`__construct`") {
            Supplier::None => {}
            Supplier::Unread(why) => unread.push(why),
            Supplier::Found { ty, at } => {
                for step in &self.types[ty].constructor {
                    match step {
                        ConstructorStep::Register(rule) => rules.push(rule),
                        // `parent` in a trait is the using class's parent.
                        ConstructorStep::Parent => {
                            self.constructor_rules(chain, open, at + 1, rules, unread)
                        }
                        ConstructorStep::Unread(span, why) => {
                            unread.push(self.unread(ty, *span, why.clone()))
                        }
                    }
                }
            }
        }
    }
}

/// [`RawRoute::CONTROLLER_MIDDLEWARE`] `path` of a `$this->middleware(...)`
/// call in a constructor.
const FROM_CONSTRUCTOR: &str = "__construct";
/// ... of an entry of a static `middleware()`.
const FROM_STATIC: &str = "middleware";
/// ... of a constructor's `parent::__construct()` call, at its place among
/// the constructor's registrations.
const CALLS_PARENT_CONSTRUCTOR: &str = "parent::__construct";
/// ... of a constructor registration this cannot read; the reason follows
/// a space.
const CONSTRUCTOR_UNREAD: &str = "__construct?";
/// ... of a static `middleware()` entry, or a body, this cannot read; the
/// reason follows a space.
const STATIC_UNREAD: &str = "middleware?";
/// ... of the class's `extends` class, an `implements` interface, and a
/// used trait; the fully qualified name follows a space.
const EXTENDS: &str = "extends";
const IMPLEMENTS: &str = "implements";
const USES: &str = "use";

/// The interface whose implementers Laravel asks for static middleware.
const HAS_MIDDLEWARE: &str = "Illuminate\\Routing\\Controllers\\HasMiddleware";

/// Nodes whose body runs only when something calls it, not where it stands.
const NESTED_SCOPES: &[&str] = &[
    "anonymous_function",
    "arrow_function",
    "function_definition",
    "anonymous_class",
];

/// Nodes that run every child whenever they run.
const STRAIGHT_LINE: &[&str] = &[
    "compound_statement",
    "expression_statement",
    "parenthesized_expression",
    "assignment_expression",
    "member_call_expression",
];

const CONDITIONAL_REGISTRATION: &str = "`$this->middleware()` runs under a branch, loop or `try`";
const CONDITIONAL_PARENT: &str = "`parent::__construct()` runs under a branch, loop or `try`";
const UNREAD_NAMES: &str = "`$this->middleware()` names middleware only the runtime knows";
const UNREAD_OPTIONS: &str =
    "`$this->middleware()` has `only` / `except` methods only the runtime knows";
const UNREAD_STATIC_BODY: &str = "the static `middleware()` does not return one array literal";
const UNREAD_STATIC_ENTRY: &str = "a static `middleware()` entry only the runtime knows";

/// Qualify each include record's group controller, as written, through the
/// file's imports and namespace.
pub(super) fn resolve_include_controllers(records: &mut [RawRoute], names: FileNames) {
    for record in records {
        if let Some(class) = record.handler.as_mut() {
            *class = names.resolve(class);
        }
    }
}

/// Append [`RawRoute::CONTROLLER_MIDDLEWARE`] records for the middleware
/// each class and trait of the file registers for its own actions:
/// `$this->middleware(...)` in its constructor, and each entry of a static
/// `middleware()`, the `HasMiddleware` contract; and for the parent,
/// interfaces and traits that decide which of them Laravel runs. A
/// middleware closure has no name and makes no record.
pub(super) fn push_controller_middleware(
    root: Node,
    source: &[u8],
    names: FileNames,
    out: &mut Vec<RawRoute>,
) {
    let mut containers = vec![root];
    while let Some(container) = containers.pop() {
        let mut cursor = container.walk();
        let children: Vec<Node> = container.named_children(&mut cursor).collect();
        for child in children {
            match child.kind() {
                "namespace_definition" | "compound_statement" => containers.push(child),
                "class_declaration" | "trait_declaration" => {
                    type_middleware(child, source, names, out)
                }
                _ => {}
            }
        }
    }
}

fn type_middleware(ty: Node, source: &[u8], names: FileNames, out: &mut Vec<RawRoute>) {
    let mut cursor = ty.walk();
    let clauses: Vec<Node> = ty.named_children(&mut cursor).collect();
    for clause in clauses {
        match clause.kind() {
            "base_clause" => push_type_names(clause, EXTENDS, source, names, out),
            "class_interface_clause" => push_type_names(clause, IMPLEMENTS, source, names, out),
            _ => {}
        }
    }
    let Some(body) = ty.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    let members: Vec<Node> = body.named_children(&mut cursor).collect();
    for member in members {
        match member.kind() {
            "use_declaration" => push_type_names(member, USES, source, names, out),
            "method_declaration" => {
                let (Some(name), Some(block)) = (
                    member
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok()),
                    member.child_by_field_name("body"),
                ) else {
                    continue;
                };
                if name.eq_ignore_ascii_case("__construct") {
                    constructor_middleware(block, source, names, out);
                } else if name.eq_ignore_ascii_case("middleware") && is_static(member) {
                    static_middleware(member, block, source, names, out);
                }
            }
            _ => {}
        }
    }
}

/// A record per class name of `clause` (`extends A`, `implements B, C`,
/// `use T;`), fully qualified through the file's names. A `use` block's
/// conflict rules name no further trait.
fn push_type_names(
    clause: Node,
    marker: &str,
    source: &[u8],
    names: FileNames,
    out: &mut Vec<RawRoute>,
) {
    let mut cursor = clause.walk();
    let types: Vec<Node> = clause
        .named_children(&mut cursor)
        .filter(|n| matches!(n.kind(), "name" | "qualified_name" | "relative_name"))
        .collect();
    for ty in types {
        let Some(fqn) = ty
            .utf8_text(source)
            .ok()
            .and_then(|name| names.class_value(name))
        else {
            continue;
        };
        push_marker(out, marker, &fqn, ty);
    }
}

fn is_static(method: Node) -> bool {
    let mut cursor = method.walk();
    let is_static = method
        .children(&mut cursor)
        .any(|child| child.kind() == "static_modifier");
    is_static
}

fn push_middleware_record(out: &mut Vec<RawRoute>, from: &str, node: Node, rule: MiddlewareRule) {
    out.push(RawRoute {
        method: RawRoute::CONTROLLER_MIDDLEWARE.to_string(),
        path: from.to_string(),
        handler: None,
        span: node_span(&node),
        scope: Some(Box::new(RawRouteScope {
            middleware: rule.names,
            only: rule.only,
            except: rule.except,
            ..RawRouteScope::default()
        })),
    });
}

/// A record whose `path` carries `payload` after `marker`. The handler
/// stays empty: a route record's handler names an entry point.
fn push_marker(out: &mut Vec<RawRoute>, marker: &str, payload: &str, node: Node) {
    out.push(RawRoute {
        method: RawRoute::CONTROLLER_MIDDLEWARE.to_string(),
        path: format!("{marker} {payload}"),
        handler: None,
        span: node_span(&node),
        scope: None,
    });
}

/// `$this->middleware('auth', ['except' => [...]])`, with `->only(...)` /
/// `->except(...)` chained on the options it returns, and
/// `parent::__construct()`, in the order the constructor body runs them.
/// A nested function or class runs nothing here. A call that runs under a
/// branch, loop or `try`, or whose names or methods only the runtime
/// knows, becomes a [`CONSTRUCTOR_UNREAD`] record.
fn constructor_middleware(block: Node, source: &[u8], names: FileNames, out: &mut Vec<RawRoute>) {
    // Each node with whether it runs whenever the constructor does.
    let mut stack = vec![(block, true)];
    while let Some((node, certain)) = stack.pop() {
        if NESTED_SCOPES.contains(&node.kind()) {
            continue;
        }
        if is_this_middleware_call(node, source) {
            match constructor_rule(node, source, names) {
                _ if !certain => {
                    push_marker(out, CONSTRUCTOR_UNREAD, CONDITIONAL_REGISTRATION, node)
                }
                Err(why) => push_marker(out, CONSTRUCTOR_UNREAD, why, node),
                Ok(Some(rule)) => push_middleware_record(out, FROM_CONSTRUCTOR, node, rule),
                Ok(None) => {}
            }
            continue;
        }
        if is_parent_constructor_call(node, source) {
            if certain {
                push_middleware_record(out, CALLS_PARENT_CONSTRUCTOR, node, MiddlewareRule::none());
            } else {
                push_marker(out, CONSTRUCTOR_UNREAD, CONDITIONAL_PARENT, node);
            }
            continue;
        }
        let certain = certain && STRAIGHT_LINE.contains(&node.kind());
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev().map(|child| (child, certain)));
    }
}

fn is_this_middleware_call(node: Node, source: &[u8]) -> bool {
    node.kind() == "member_call_expression"
        && call_name(node, source) == Some("middleware")
        && node
            .child_by_field_name("object")
            .and_then(|object| object.utf8_text(source).ok())
            == Some("$this")
}

fn is_parent_constructor_call(node: Node, source: &[u8]) -> bool {
    node.kind() == "scoped_call_expression"
        && call_name(node, source).is_some_and(|name| name.eq_ignore_ascii_case("__construct"))
        && node
            .child_by_field_name("scope")
            .and_then(|scope| scope.utf8_text(source).ok())
            .is_some_and(|scope| scope.eq_ignore_ascii_case("parent"))
}

/// The rule one `$this->middleware(...)` call registers; `None` for a
/// closure, which has no name.
fn constructor_rule(
    call: Node,
    source: &[u8],
    names: FileNames,
) -> Result<Option<MiddlewareRule>, &'static str> {
    let registered = nth_argument(call, 0)
        .and_then(|value| literal_middleware(value, source, names))
        .ok_or(UNREAD_NAMES)?;
    let mut rule = MiddlewareRule {
        names: registered,
        ..MiddlewareRule::none()
    };
    match nth_argument(call, 1) {
        None => {}
        Some(options) if options.kind() == "array_creation_expression" => {
            let mut cursor = options.walk();
            let elements: Vec<Node> = options
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "array_element_initializer")
                .collect();
            for element in elements {
                // A positional value is no option; a key only the runtime
                // knows may be `only` or `except`.
                if element.named_child_count() != 2 {
                    continue;
                }
                let key = element
                    .named_child(0)
                    .and_then(|key| string_value(key, source))
                    .ok_or(UNREAD_OPTIONS)?;
                let value = element.named_child(1).ok_or(UNREAD_OPTIONS)?;
                match key.as_str() {
                    "only" => {
                        rule.only = Some(literal_methods(value, source).ok_or(UNREAD_OPTIONS)?)
                    }
                    "except" => {
                        rule.except = Some(literal_methods(value, source).ok_or(UNREAD_OPTIONS)?)
                    }
                    _ => {}
                }
            }
        }
        Some(_) => return Err(UNREAD_OPTIONS),
    }
    // `ControllerMiddlewareOptions` writes through to the same options
    // array, so a chained call replaces the option.
    let mut cur = call;
    while let Some(parent) = cur
        .parent()
        .filter(|p| p.kind() == "member_call_expression")
        .filter(|p| p.child_by_field_name("object") == Some(cur))
    {
        match call_name(parent, source) {
            Some("only") => {
                rule.only = Some(variadic_methods(parent, source).ok_or(UNREAD_OPTIONS)?)
            }
            Some("except") => {
                rule.except = Some(variadic_methods(parent, source).ok_or(UNREAD_OPTIONS)?)
            }
            _ => {}
        }
        cur = parent;
    }
    Ok((!rule.names.is_empty()).then_some(rule))
}

/// The entries of the one array a static `middleware()` returns. Any other
/// body (several returns, a return under a branch, a value that is not an
/// array literal) leaves every entry unread.
fn static_middleware(
    method: Node,
    block: Node,
    source: &[u8],
    names: FileNames,
    out: &mut Vec<RawRoute>,
) {
    push_middleware_record(out, FROM_STATIC, method, MiddlewareRule::none());
    let returns = returns_of(block);
    let array = match returns.as_slice() {
        [ret] if ret.parent() == Some(block) => ret
            .named_child(0)
            .filter(|value| value.kind() == "array_creation_expression"),
        _ => None,
    };
    let Some(entries) = array.and_then(array_entries) else {
        push_marker(out, STATIC_UNREAD, UNREAD_STATIC_BODY, method);
        return;
    };
    for entry in entries {
        match static_entry(entry, source, names) {
            Ok(Some(rule)) => push_middleware_record(out, FROM_STATIC, entry, rule),
            Ok(None) => {}
            Err(why) => push_marker(out, STATIC_UNREAD, why, entry),
        }
    }
}

/// The `return` statements of a method body, past nested functions and
/// classes, whose returns are not the method's.
fn returns_of(block: Node) -> Vec<Node> {
    let mut returns = Vec::new();
    let mut stack = vec![block];
    while let Some(node) = stack.pop() {
        if NESTED_SCOPES.contains(&node.kind()) {
            continue;
        }
        if node.kind() == "return_statement" {
            returns.push(node);
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    returns
}

/// One entry of a static `middleware()` array: `'auth'`, `Ensure::class`,
/// or `new Middleware('auth', only: [...], except: [...])` with
/// `->only(...)` / `->except(...)` chained on it; `None` for a closure.
fn static_entry(
    entry: Node,
    source: &[u8],
    names: FileNames,
) -> Result<Option<MiddlewareRule>, &'static str> {
    match entry.kind() {
        "parenthesized_expression" => static_entry(
            entry.named_child(0).ok_or(UNREAD_STATIC_ENTRY)?,
            source,
            names,
        ),
        "member_call_expression" => {
            let object = entry
                .child_by_field_name("object")
                .ok_or(UNREAD_STATIC_ENTRY)?;
            let rule = static_entry(object, source, names)?;
            let methods = nth_argument(entry, 0)
                .and_then(|value| literal_methods(value, source))
                .ok_or(UNREAD_STATIC_ENTRY)?;
            let Some(mut rule) = rule else {
                return Ok(None);
            };
            match call_name(entry, source) {
                Some("only") => rule.only = Some(methods),
                Some("except") => rule.except = Some(methods),
                _ => return Err(UNREAD_STATIC_ENTRY),
            }
            Ok(Some(rule))
        }
        "object_creation_expression" => {
            let mut cursor = entry.walk();
            let children: Vec<Node> = entry.named_children(&mut cursor).collect();
            let class = children
                .first()
                .and_then(|class| class.utf8_text(source).ok())
                .ok_or(UNREAD_STATIC_ENTRY)?;
            if class.rsplit('\\').next() != Some("Middleware") {
                return Err(UNREAD_STATIC_ENTRY);
            }
            let list = children
                .iter()
                .find(|n| n.kind() == "arguments")
                .ok_or(UNREAD_STATIC_ENTRY)?;
            let mut cursor = list.walk();
            let arguments: Vec<Node> = list
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "argument")
                .collect();
            let mut rule = MiddlewareRule::none();
            for (position, argument) in arguments.into_iter().enumerate() {
                let label = argument
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok());
                let value = argument_value(argument).ok_or(UNREAD_STATIC_ENTRY)?;
                // `null` leaves the filter unset: `?array $only = null`.
                let methods = || match value.kind() {
                    "null" => Ok(None),
                    _ => literal_methods(value, source)
                        .map(Some)
                        .ok_or(UNREAD_STATIC_ENTRY),
                };
                match (label, position) {
                    (Some("middleware"), _) | (None, 0) => {
                        rule.names =
                            literal_middleware(value, source, names).ok_or(UNREAD_STATIC_ENTRY)?
                    }
                    (Some("only"), _) | (None, 1) => rule.only = methods()?,
                    (Some("except"), _) | (None, 2) => rule.except = methods()?,
                    _ => return Err(UNREAD_STATIC_ENTRY),
                }
            }
            Ok((!rule.names.is_empty()).then_some(rule))
        }
        _ => {
            let names = literal_middleware(entry, source, names).ok_or(UNREAD_STATIC_ENTRY)?;
            Ok((!names.is_empty()).then(|| MiddlewareRule {
                names,
                ..MiddlewareRule::none()
            }))
        }
    }
}

/// The middleware names `node` registers: `'auth'`, `Ensure::class`, a
/// closure, which has no name, or an array of them. `None` when an entry is
/// any other expression, whose value only the runtime knows.
fn literal_middleware(node: Node, source: &[u8], names: FileNames) -> Option<Vec<String>> {
    let values = match node.kind() {
        "array_creation_expression" => array_entries(node)?,
        _ => vec![node],
    };
    let mut out = Vec::new();
    for value in values {
        if matches!(value.kind(), "anonymous_function" | "arrow_function") {
            continue;
        }
        out.push(middleware_name(value, source, names)?);
    }
    Some(out)
}

/// The method names `node` lists: `'index'` or `['index', 'show']`; `None`
/// when one is not a literal.
fn literal_methods(node: Node, source: &[u8]) -> Option<Vec<String>> {
    match node.kind() {
        "array_creation_expression" => array_entries(node)?
            .into_iter()
            .map(|value| string_value(value, source))
            .collect(),
        _ => string_value(node, source).map(|method| vec![method]),
    }
}

/// The method names of `->only(...)` / `->except(...)`: an array first
/// argument is the list, else each argument is one name, as
/// `ControllerMiddlewareOptions` reads them.
fn variadic_methods(call: Node, source: &[u8]) -> Option<Vec<String>> {
    let values: Vec<Node> = arguments(call)
        .into_iter()
        .map(argument_value)
        .collect::<Option<_>>()?;
    match values.as_slice() {
        [first, ..] if first.kind() == "array_creation_expression" => {
            literal_methods(*first, source)
        }
        many => many.iter().map(|v| string_value(*v, source)).collect(),
    }
}

/// The value of every element of an array literal, a keyed one's too.
/// `None` for a spread, whose elements only the runtime knows.
fn array_entries(array: Node) -> Option<Vec<Node>> {
    let mut cursor = array.walk();
    let elements: Vec<Node> = array
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "array_element_initializer")
        .collect();
    elements
        .into_iter()
        .map(|element| {
            element
                .named_child(element.named_child_count().checked_sub(1)? as u32)
                .filter(|value| !matches!(value.kind(), "variadic_unpacking" | "by_ref"))
        })
        .collect()
}
