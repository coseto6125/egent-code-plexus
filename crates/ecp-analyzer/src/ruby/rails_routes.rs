//! Rails routing-DSL expansion for `Rails.application.routes.draw do … end`.
//!
//! The generic Ruby route matcher in `queries.scm` sees one `call` node with a
//! string first argument, so it captures `get "/zendesk/chat"` and nothing
//! else. A Rails routes file states most of its surface through `resources`,
//! which names a symbol and leaves the REST paths to convention. Measured
//! 2026-09-02 on a Rails 8.1 application's 465-line routes file: the generic
//! matcher reported 12 routes; `ActionDispatch::Routing::RouteSet` drawing the
//! same file reports 272, and this walker reports the same 272.
//!
//! The walker reads the block structure: it enters `namespace`, `scope`,
//! `resources`, `resource`, `member` and `collection` blocks, accumulates the
//! path prefix each one contributes, and expands every REST declaration it
//! passes. A `concern` body is expanded where `concerns:` mounts it, under the
//! mounting resource's prefix, which is where Rails serves it.
//!
//! Out of scope, and stated so a reader does not assume otherwise: `shallow:`
//! (nested member routes keep the parent prefix they would lose), `direct`,
//! `resolve`, and gem-generated route sets such as `devise_for`. A construct
//! outside this list that wraps declarations in a block (`constraints`,
//! `defaults`, `devise_scope`) is entered as a transparent scope, so the
//! declarations it holds stay in the inventory.

use ecp_core::analyzer::types::RawRoute;
use std::collections::HashMap;
use tree_sitter::Node;

/// One REST action as Rails maps it onto a method and a path suffix.
///
/// `update` appears twice because Rails routes both PATCH and PUT to it, and
/// an inventory that lists one and not the other misreports the surface.
struct RestAction {
    method: &'static str,
    /// Path suffix appended to the resource path.
    suffix: &'static str,
    action: &'static str,
    /// A member action addresses one record. A plural resource inserts `/:id`
    /// for it; a singular resource has no id, so the flag only names the
    /// action's role there.
    member: bool,
}

/// `resources :posts` — the collection is addressable, so `index` exists.
const PLURAL_ACTIONS: &[RestAction] = &[
    RestAction {
        method: "GET",
        suffix: "",
        action: "index",
        member: false,
    },
    RestAction {
        method: "GET",
        suffix: "/new",
        action: "new",
        member: false,
    },
    RestAction {
        method: "POST",
        suffix: "",
        action: "create",
        member: false,
    },
    RestAction {
        method: "GET",
        suffix: "",
        action: "show",
        member: true,
    },
    RestAction {
        method: "GET",
        suffix: "/edit",
        action: "edit",
        member: true,
    },
    RestAction {
        method: "PATCH",
        suffix: "",
        action: "update",
        member: true,
    },
    RestAction {
        method: "PUT",
        suffix: "",
        action: "update",
        member: true,
    },
    RestAction {
        method: "DELETE",
        suffix: "",
        action: "destroy",
        member: true,
    },
];

/// `resource :profile` — one record, addressed without an id, so no `index`.
const SINGULAR_ACTIONS: &[RestAction] = &[
    RestAction {
        method: "GET",
        suffix: "/new",
        action: "new",
        member: false,
    },
    RestAction {
        method: "POST",
        suffix: "",
        action: "create",
        member: false,
    },
    RestAction {
        method: "GET",
        suffix: "",
        action: "show",
        member: true,
    },
    RestAction {
        method: "GET",
        suffix: "/edit",
        action: "edit",
        member: true,
    },
    RestAction {
        method: "PATCH",
        suffix: "",
        action: "update",
        member: true,
    },
    RestAction {
        method: "PUT",
        suffix: "",
        action: "update",
        member: true,
    },
    RestAction {
        method: "DELETE",
        suffix: "",
        action: "destroy",
        member: true,
    },
];

/// Actions Rails serves at the resource's own path, with no action segment
/// appended. `edit` and `new` are absent because they keep their suffix.
/// Mirrors `ActionDispatch::Routing::Mapper::CANONICAL_ACTIONS`.
const CANONICAL_ACTIONS: &[&str] = &["index", "create", "show", "update", "destroy"];

const HTTP_VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "options", "head"];

/// Routing-block nesting cap. Only a block that contributes a scope counts
/// (`namespace`, `scope`, `resources`, `member`, …); an `if` or a `def` the
/// declarations sit inside costs nothing. A file that exceeds the cap is not
/// a shape this walker models, so it stops, records where, and leaves the
/// generic matcher's captures for that region in place.
const MAX_DEPTH: usize = 32;

/// Where the walker currently sits in the routes tree.
#[derive(Clone)]
struct Ctx {
    /// Path every nested declaration hangs off, e.g. `/admin/posts/:post_id`.
    prefix: String,
    /// Controller module prefix contributed by `namespace` / `scope module:`.
    module_prefix: String,
    /// Where a bare-symbol verb (`get :confirm_delete`) attaches: the
    /// enclosing resource's own path.
    action_base: String,
    /// Controller a bare-symbol verb belongs to, set by the enclosing resource.
    controller: Option<String>,
    /// Whether a member action inserts `/:id`. A singular `resource` addresses
    /// one record with no id, so `post :validate, on: :member` inside it
    /// serves the resource path plus the action and nothing between.
    member_takes_id: bool,
}

impl Ctx {
    fn root() -> Self {
        Self {
            prefix: String::new(),
            module_prefix: String::new(),
            action_base: String::new(),
            controller: None,
            member_takes_id: true,
        }
    }

    /// Path a member action hangs off: one record of a plural resource, or
    /// the singular resource itself.
    fn member_base(&self) -> String {
        if self.member_takes_id {
            format!("{}/:id", self.action_base)
        } else {
            self.action_base.clone()
        }
    }
}

/// What one file's `routes.draw` blocks declare.
#[derive(Default)]
pub struct RailsRoutes {
    pub routes: Vec<RawRoute>,
    /// Inclusive start/end rows of each `routes.draw` block. The generic
    /// `queries.scm` matcher also fires on the literal verb lines inside these
    /// blocks, and this walker reports the same lines with the enclosing
    /// prefix applied, so the caller drops the generic captures that land here.
    pub draw_rows: Vec<(u32, u32)>,
    /// Spans of blocks the walker refused to enter at [`MAX_DEPTH`]. The
    /// generic captures inside them are kept — a prefix-less route beats no
    /// route — and the parser records one blind spot per entry.
    pub truncated: Vec<(u32, u32, u32, u32)>,
}

impl RailsRoutes {
    /// Whether the walker's output replaces the generic capture on `row`.
    pub fn covers(&self, row: u32) -> bool {
        let in_draw = self.draw_rows.iter().any(|&(a, b)| row >= a && row <= b);
        let in_truncated = self
            .truncated
            .iter()
            .any(|&(a, _, b, _)| row >= a && row <= b);
        in_draw && !in_truncated
    }
}

/// Expand every route declared in `root`. Returns an empty result when the
/// file holds no `routes.draw` block, which is every Ruby file but a routes
/// file.
pub fn extract(root: Node, source: &[u8]) -> RailsRoutes {
    let mut out = RailsRoutes::default();
    for draw_block in find_draw_blocks(root, source) {
        out.draw_rows.push((
            draw_block.start_position().row as u32,
            draw_block.end_position().row as u32,
        ));
        let mut walk = Walk {
            source,
            concerns: collect_concerns(draw_block, source),
            routes: Vec::new(),
            truncated: Vec::new(),
        };
        walk.block(draw_block, &Ctx::root(), 0);
        out.routes.extend(walk.routes);
        out.truncated.extend(walk.truncated);
    }
    out
}

/// Locate the `do` block of each `<something>.routes.draw do` call.
///
/// The receiver must end in `routes`: `Rails.application.routes.draw` and
/// `Foo::Engine.routes.draw` both do, so engine route files stay in scope,
/// while a report's `canvas.draw do … end` does not become a route table.
fn find_draw_blocks<'t>(root: Node<'t>, source: &[u8]) -> Vec<Node<'t>> {
    let mut found = Vec::new();
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if is_routes_draw(n, source) {
            if let Some(block) = n.child_by_field_name("block") {
                found.push(block);
                continue;
            }
        }
        stack.extend(n.children(&mut cursor));
    }
    found
}

fn is_routes_draw(n: Node, source: &[u8]) -> bool {
    if n.kind() != "call" || method_name(n, source) != Some("draw") {
        return false;
    }
    n.child_by_field_name("receiver")
        .filter(|r| r.kind() == "call")
        .and_then(|r| method_name(r, source))
        == Some("routes")
}

/// `concern :name do … end` blocks, by name. A concern serves a path only
/// where `concerns:` mounts it, so its body is expanded at the mount site
/// under that resource's prefix rather than where it is defined.
fn collect_concerns<'t>(draw_block: Node<'t>, source: &[u8]) -> HashMap<String, Node<'t>> {
    let mut found = HashMap::new();
    let mut cursor = draw_block.walk();
    let mut stack = vec![draw_block];
    while let Some(n) = stack.pop() {
        if n.kind() == "call" && method_name(n, source) == Some("concern") {
            let name = args(n).and_then(|a| leading_symbols(a, source).into_iter().next());
            if let (Some(name), Some(block)) = (name, n.child_by_field_name("block")) {
                found.insert(name, block);
                continue;
            }
        }
        stack.extend(n.children(&mut cursor));
    }
    found
}

/// One traversal of one `routes.draw` block.
struct Walk<'s, 't> {
    source: &'s [u8],
    concerns: HashMap<String, Node<'t>>,
    routes: Vec<RawRoute>,
    truncated: Vec<(u32, u32, u32, u32)>,
}

impl<'s, 't> Walk<'s, 't> {
    /// Enter one routing block, one level deeper.
    fn block(&mut self, block: Node<'t>, ctx: &Ctx, depth: usize) {
        if depth > MAX_DEPTH {
            self.truncated.push(span(block));
            return;
        }
        if let Some(body) = block.child_by_field_name("body") {
            self.statements(body, ctx, depth);
        }
    }

    /// Visit every declaration in statement position under `node`, at the
    /// same depth. A routes file wraps declarations in structure that is not
    /// itself a call — GitLab defines `def draw_all_routes` inside the draw
    /// block, and an environment-dependent route sits behind
    /// `if Rails.env.development?` — and neither contributes a scope.
    fn statements(&mut self, node: Node<'t>, ctx: &Ctx, depth: usize) {
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        for stmt in children {
            if stmt.kind() == "call" {
                self.call(stmt, ctx, depth);
            } else {
                self.statements(stmt, ctx, depth);
            }
        }
    }

    fn call(&mut self, call: Node<'t>, ctx: &Ctx, depth: usize) {
        // Every Rails routing verb is sent to the mapper itself, so a call
        // with a receiver is not a declaration: `x = client.get("/remote")`
        // written in routes.rb stays an outbound call. Its block, if any, is
        // still entered — `%w[edit tree].each do … end` wraps real
        // declarations (GitLab routes.rb:177). The out-of-draw rule lives in
        // `parser.rs` with a router-object allowance this block does not need.
        if call.child_by_field_name("receiver").is_some() {
            if let Some(block) = call.child_by_field_name("block") {
                self.block(block, ctx, depth + 1);
            }
            return;
        }
        let Some(method) = method_name(call, self.source) else {
            return;
        };
        match method {
            "resources" => self.resource(call, ctx, depth, PLURAL_ACTIONS, true),
            "resource" => self.resource(call, ctx, depth, SINGULAR_ACTIONS, false),
            "namespace" => self.namespace(call, ctx, depth),
            "scope" => self.scope(call, ctx, depth),
            "member" | "collection" => {
                // Both attach to the resource's own path, not to `ctx.prefix`,
                // which inside a resource block already carries the parent id
                // segment nested resources need (`/agents/:agent_id`).
                let base = if method == "member" {
                    ctx.member_base()
                } else {
                    ctx.action_base.clone()
                };
                // A string path inside the block hangs off the same base, so
                // `prefix` moves with it — `get "ticket_info/:ticket_id"`
                // inside `member do` serves `/conversations/:id/ticket_info/:ticket_id`.
                let inner = Ctx {
                    prefix: base.clone(),
                    action_base: base,
                    module_prefix: ctx.module_prefix.clone(),
                    controller: ctx.controller.clone(),
                    member_takes_id: ctx.member_takes_id,
                };
                if let Some(block) = call.child_by_field_name("block") {
                    self.block(block, &inner, depth + 1);
                }
            }
            "concerns" => {
                // `concerns :a, :b` inside a resource block mounts each here.
                if let Some(a) = args(call) {
                    for name in leading_symbols(a, self.source) {
                        self.mount_concern(&name, ctx, depth);
                    }
                }
            }
            "root" => {
                let handler = pair_string(call, "to", self.source);
                self.push("GET", normalize(&format!("{}/", ctx.prefix)), handler, call);
            }
            "mount" => {
                // `mount X, at: "/p"` and `mount X => "/p"` both name a mount path.
                let path = pair_string(call, "at", self.source)
                    .or_else(|| engine_rocket_path(call, self.source));
                if let Some(path) = path {
                    self.push("MOUNT", normalize(&join(&ctx.prefix, &path)), None, call);
                }
            }
            v if HTTP_VERBS.contains(&v) => self.verb(call, ctx, v),
            // `match :decline, via: [:get, :post]` declares one route per verb.
            "match" => {
                for verb in match_verbs(call, self.source) {
                    self.verb(call, ctx, &verb);
                }
            }
            // The definition site of a concern serves no path; see
            // `collect_concerns`.
            "concern" => {}
            // Every other block-bearing call wraps route declarations without
            // contributing a path segment — `devise_scope`, `constraints`,
            // `defaults`, `authenticate`. The emit arms above are the only
            // thing that produces output, so a block holding no declaration
            // adds nothing.
            _ => {
                if let Some(block) = call.child_by_field_name("block") {
                    self.block(block, ctx, depth + 1);
                }
            }
        }
    }

    /// Expand one `resources` / `resource` declaration, then descend into its
    /// block under each named resource so nested routes inherit the parent's
    /// path. Rails applies the block to every name (`Mapper#apply_common_behavior_for`
    /// iterates `resources.each { |r| … &block }`), not only the last.
    fn resource(
        &mut self,
        call: Node<'t>,
        ctx: &Ctx,
        depth: usize,
        actions: &[RestAction],
        plural: bool,
    ) {
        let Some(a) = args(call) else {
            return;
        };
        let names = leading_symbols(a, self.source);
        if names.is_empty() {
            return;
        }
        let allowed = action_filter(a, self.source);
        // `path:` renames the URL segment only; the controller keeps the
        // resource name, so `resources :api_keys, path: "api"` serves `/api`
        // from `ApiKeysController`.
        let path_override = pair_text(call, "path", self.source);
        // `module:` moves the controller without touching the path.
        let module_prefix = match pair_text(call, "module", self.source) {
            Some(m) => format!("{}{m}/", ctx.module_prefix),
            None => ctx.module_prefix.clone(),
        };
        let mounted: Vec<String> = symbol_list_for(a, "concerns", self.source).unwrap_or_default();

        for name in &names {
            let segment = path_override.as_deref().unwrap_or(name);
            let resource_path = join(&ctx.prefix, segment);
            let controller = format!("{module_prefix}{name}");

            for act in actions {
                if !allowed(act.action) {
                    continue;
                }
                let base = if act.member && plural {
                    format!("{resource_path}/:id")
                } else {
                    resource_path.clone()
                };
                let path = normalize(&format!("{base}{}", act.suffix));
                self.push(
                    act.method,
                    path,
                    Some(format!("{controller}#{}", act.action)),
                    call,
                );
            }

            // A plural resource scopes its children under one record, so
            // nested paths carry `/:<singular>_id`; a singular resource has
            // no id to insert.
            let nested_prefix = if plural {
                format!("{resource_path}/:{}_id", singularize(name))
            } else {
                resource_path.clone()
            };
            let inner = Ctx {
                prefix: nested_prefix,
                action_base: resource_path,
                controller: Some(controller),
                module_prefix: module_prefix.clone(),
                member_takes_id: plural,
            };
            for concern in &mounted {
                self.mount_concern(concern, &inner, depth);
            }
            if let Some(block) = call.child_by_field_name("block") {
                self.block(block, &inner, depth + 1);
            }
        }
    }

    /// Expand a concern's body as if written at the mount site.
    fn mount_concern(&mut self, name: &str, ctx: &Ctx, depth: usize) {
        if let Some(block) = self.concerns.get(name).copied() {
            self.block(block, ctx, depth + 1);
        }
    }

    /// `namespace :admin do` contributes a path segment (`path:` renames it)
    /// and a module prefix. Neither changes the enclosing resource's
    /// plurality, so `member_takes_id` carries through.
    fn namespace(&mut self, call: Node<'t>, ctx: &Ctx, depth: usize) {
        let Some(name) =
            args(call).and_then(|a| leading_symbols(a, self.source).into_iter().next())
        else {
            return;
        };
        let segment = pair_text(call, "path", self.source).unwrap_or_else(|| name.clone());
        let prefix = join(&ctx.prefix, &segment);
        let inner = Ctx {
            action_base: prefix.clone(),
            prefix,
            module_prefix: format!("{}{name}/", ctx.module_prefix),
            controller: None,
            member_takes_id: ctx.member_takes_id,
        };
        if let Some(block) = call.child_by_field_name("block") {
            self.block(block, &inner, depth + 1);
        }
    }

    /// `scope` sets a path prefix, a module prefix, or both. The path comes
    /// from `path:`, or from the leading arguments — `scope :api` and
    /// `scope "api"` both prefix `/api` (`Mapper#scope` joins `args.flatten`).
    fn scope(&mut self, call: Node<'t>, ctx: &Ctx, depth: usize) {
        let mut prefix = ctx.prefix.clone();
        if let Some(a) = args(call) {
            let segment = pair_text(call, "path", self.source).or_else(|| {
                let mut parts = leading_strings(a, self.source);
                parts.extend(leading_symbols(a, self.source));
                (!parts.is_empty()).then(|| parts.join("/"))
            });
            if let Some(s) = segment {
                prefix = join(&prefix, &s);
            }
        }
        let module_prefix = match pair_text(call, "module", self.source) {
            Some(m) => format!("{}{m}/", ctx.module_prefix),
            None => ctx.module_prefix.clone(),
        };
        let inner = Ctx {
            action_base: prefix.clone(),
            prefix,
            module_prefix,
            controller: ctx.controller.clone(),
            member_takes_id: ctx.member_takes_id,
        };
        if let Some(block) = call.child_by_field_name("block") {
            self.block(block, &inner, depth + 1);
        }
    }

    /// Emit an explicit verb line: `get "/x", to: "c#a"`, `get :action`, or
    /// the `get "up" => "rails/health#show"` hashrocket form.
    fn verb(&mut self, call: Node<'t>, ctx: &Ctx, verb: &str) {
        let Some(a) = args(call) else {
            return;
        };
        let handler = pair_string(call, "to", self.source);

        // `on:` is the inline form of the `member` / `collection` / `new`
        // blocks and moves this one route to the matching base.
        let base = match pair_symbol(call, "on", self.source).as_deref() {
            Some("member") => ctx.member_base(),
            Some("collection") => ctx.action_base.clone(),
            Some("new") => join(&ctx.action_base, "new"),
            _ => ctx.prefix.clone(),
        };

        if let Some(literal) = leading_strings(a, self.source).into_iter().next() {
            self.push(
                &verb.to_uppercase(),
                normalize(&join(&base, &literal)),
                handler,
                call,
            );
            return;
        }
        if let Some((path, target)) = rocket_string_pair(call, self.source) {
            let handler = handler.or(Some(target));
            self.push(
                &verb.to_uppercase(),
                normalize(&join(&base, &path)),
                handler,
                call,
            );
            return;
        }
        // A bare symbol names an action on the enclosing resource. A
        // canonical action is served at the resource path itself, so
        // `delete :destroy` inside `resource :knowledge_base` reinstates
        // `DELETE /knowledge_base` rather than adding `/destroy`.
        if let Some(action) = leading_symbols(a, self.source).into_iter().next() {
            let handler =
                handler.or_else(|| ctx.controller.as_ref().map(|c| format!("{c}#{action}")));
            let path = if ctx.controller.is_some() && CANONICAL_ACTIONS.contains(&action.as_str()) {
                base
            } else {
                join(&base, &action)
            };
            self.push(&verb.to_uppercase(), normalize(&path), handler, call);
        }
    }

    fn push(&mut self, method: &str, path: String, handler: Option<String>, call: Node) {
        self.routes.push(RawRoute {
            method: method.to_string(),
            path,
            handler,
            span: span(call),
        });
    }
}

// ── AST helpers ─────────────────────────────────────────────────────────────

fn span(n: Node) -> (u32, u32, u32, u32) {
    let s = n.start_position();
    let e = n.end_position();
    (s.row as u32, s.column as u32, e.row as u32, e.column as u32)
}

fn method_name<'a>(call: Node, source: &'a [u8]) -> Option<&'a str> {
    call.child_by_field_name("method")?.utf8_text(source).ok()
}

fn args(call: Node) -> Option<Node> {
    call.child_by_field_name("arguments")
}

/// Positional arguments before the first keyword pair, of one node kind.
fn leading<'t>(args: Node<'t>, kind: &str) -> Vec<Node<'t>> {
    let mut cursor = args.walk();
    args.named_children(&mut cursor)
        .take_while(|n| n.kind() != "pair")
        .filter(|n| n.kind() == kind)
        .collect()
}

fn leading_symbols(args: Node, source: &[u8]) -> Vec<String> {
    leading(args, "simple_symbol")
        .into_iter()
        .filter_map(|n| symbol_name(n, source))
        .collect()
}

fn leading_strings(args: Node, source: &[u8]) -> Vec<String> {
    leading(args, "string")
        .into_iter()
        .filter_map(|n| string_content(n, source))
        .collect()
}

/// The `key: value` pairs of a call's argument list.
fn pairs<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    let Some(a) = args(call) else {
        return Vec::new();
    };
    let mut cursor = a.walk();
    a.named_children(&mut cursor)
        .filter(|n| n.kind() == "pair")
        .collect()
}

/// Value node of the pair whose key is `key`.
fn pair_value<'t>(call: Node<'t>, key: &str, source: &[u8]) -> Option<Node<'t>> {
    pairs(call)
        .into_iter()
        .find(|p| {
            p.child_by_field_name("key")
                .and_then(|k| k.utf8_text(source).ok())
                .is_some_and(|k| k.trim_end_matches(':') == key)
        })
        .and_then(|p| p.child_by_field_name("value"))
}

fn pair_string(call: Node, key: &str, source: &[u8]) -> Option<String> {
    pair_value(call, key, source).and_then(|v| string_content(v, source))
}

fn pair_symbol(call: Node, key: &str, source: &[u8]) -> Option<String> {
    pair_value(call, key, source).and_then(|v| symbol_name(v, source))
}

/// A string or a symbol under `key:` — `path: "api"` and `path: :api` name
/// the same segment.
fn pair_text(call: Node, key: &str, source: &[u8]) -> Option<String> {
    pair_string(call, key, source).or_else(|| pair_symbol(call, key, source))
}

/// `"path" => "controller#action"`: the hashrocket form of a verb line.
fn rocket_string_pair(call: Node, source: &[u8]) -> Option<(String, String)> {
    pairs(call).into_iter().find_map(|p| {
        let k = p.child_by_field_name("key")?;
        let v = p.child_by_field_name("value")?;
        Some((string_content(k, source)?, string_content(v, source)?))
    })
}

/// `Engine => "/path"`: the hashrocket form of `mount`. Only a constant key
/// qualifies, so `mount Foo::Engine, as: "bar"` does not read `as:` as the
/// mount path.
fn engine_rocket_path(call: Node, source: &[u8]) -> Option<String> {
    pairs(call).into_iter().find_map(|p| {
        let k = p.child_by_field_name("key")?;
        if k.kind() != "constant" && k.kind() != "scope_resolution" {
            return None;
        }
        string_content(p.child_by_field_name("value")?, source)
    })
}

/// Symbols under `key:`, accepting `[:a, :b]`, `%i[a b]`, and a lone `:a`.
fn symbol_list_for(args: Node, key: &str, source: &[u8]) -> Option<Vec<String>> {
    let call = args.parent()?;
    let value = pair_value(call, key, source)?;
    let mut cursor = value.walk();
    let list = match value.kind() {
        "array" | "symbol_array" => value
            .named_children(&mut cursor)
            .filter_map(|n| symbol_name(n, source))
            .collect(),
        "simple_symbol" => symbol_name(value, source).into_iter().collect(),
        _ => return None,
    };
    Some(list)
}

/// Build the `only:` / `except:` predicate. Absent both, every action passes.
fn action_filter(args: Node, source: &[u8]) -> impl Fn(&str) -> bool + use<> {
    let only = symbol_list_for(args, "only", source);
    let except = symbol_list_for(args, "except", source);
    move |action: &str| match (&only, &except) {
        (Some(list), _) => list.iter().any(|a| a == action),
        (None, Some(list)) => !list.iter().any(|a| a == action),
        (None, None) => true,
    }
}

/// Verbs a `match` line declares through `via:`. `via: :all` means every
/// verb Rails routes; an absent `via:` declares no verb, so the line
/// contributes nothing to an HTTP inventory.
fn match_verbs(call: Node, source: &[u8]) -> Vec<String> {
    let Some(a) = args(call) else {
        return Vec::new();
    };
    match symbol_list_for(a, "via", source) {
        Some(list) if list.iter().any(|v| v == "all") => {
            HTTP_VERBS.iter().map(|v| v.to_string()).collect()
        }
        Some(list) => list
            .into_iter()
            .filter(|v| HTTP_VERBS.contains(&v.as_str()))
            .collect(),
        None => Vec::new(),
    }
}

/// Name of a `:sym` / `%i[sym]` element, sigil removed.
fn symbol_name(n: Node, source: &[u8]) -> Option<String> {
    match n.kind() {
        "simple_symbol" => n
            .utf8_text(source)
            .ok()
            .map(|s| s.trim_start_matches(':').to_string()),
        "bare_symbol" => n.utf8_text(source).ok().map(str::to_string),
        _ => None,
    }
}

/// Content of a `string` node. An interpolated segment becomes a dynamic
/// segment named after the expression, so GitLab's
/// `get "/#{action}/*branch"` reads `/:action/*branch` in the inventory
/// rather than vanishing.
fn string_content(n: Node, source: &[u8]) -> Option<String> {
    if n.kind() != "string" {
        return None;
    }
    let mut cursor = n.walk();
    let mut out = String::new();
    for c in n.named_children(&mut cursor) {
        match c.kind() {
            "string_content" => out.push_str(c.utf8_text(source).ok()?),
            "interpolation" => {
                let expr = c
                    .named_child(0)
                    .filter(|e| e.kind() == "identifier")
                    .and_then(|e| e.utf8_text(source).ok())
                    .unwrap_or("param");
                out.push(':');
                out.push_str(expr);
            }
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

// ── Path helpers ────────────────────────────────────────────────────────────

fn join(prefix: &str, segment: &str) -> String {
    format!(
        "{}/{}",
        prefix.trim_end_matches('/'),
        segment.trim_start_matches('/')
    )
}

/// Collapse repeated and trailing slashes so `/admin//posts/` reads
/// `/admin/posts`. The root path stays `/`.
fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 1);
    let mut last_slash = false;
    for c in path.chars() {
        if c == '/' {
            if !last_slash {
                out.push(c);
            }
            last_slash = true;
        } else {
            out.push(c);
            last_slash = false;
        }
    }
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Rails builds a nested route's id parameter from the singular resource
/// name. This covers the regular English endings a routes file uses; an
/// irregular plural yields a slightly wrong parameter name, not a missing
/// route.
fn singularize(name: &str) -> String {
    for (plural, singular) in [
        ("ies", "y"),
        ("sses", "ss"),
        ("ches", "ch"),
        ("shes", "sh"),
        ("xes", "x"),
    ] {
        if let Some(stem) = name.strip_suffix(plural) {
            return format!("{stem}{singular}");
        }
    }
    name.strip_suffix('s')
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string())
}
