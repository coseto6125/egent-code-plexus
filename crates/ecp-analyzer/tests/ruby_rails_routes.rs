//! Rails routing-DSL expansion — `resources`, nesting, and the constructs a
//! real `config/routes.rb` builds paths from.
//!
//! Every expectation here states a path Rails itself serves, so a failure
//! means the inventory disagrees with the running application, not that a
//! formatting choice changed.

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawRoute;

fn routes(src: &str) -> Vec<RawRoute> {
    let provider = RubyProvider::new().unwrap();
    provider
        .parse_file("config/routes.rb".as_ref(), src.as_bytes())
        .unwrap()
        .routes
}

/// `METHOD /path` pairs, for order-independent set assertions.
fn pairs(rs: &[RawRoute]) -> Vec<String> {
    let mut v: Vec<String> = rs
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    v.sort();
    v
}

fn has(rs: &[RawRoute], method: &str, path: &str) -> bool {
    rs.iter().any(|r| r.method == method && r.path == path)
}

fn draw(body: &str) -> String {
    format!("Rails.application.routes.draw do\n{body}\nend\n")
}

#[test]
fn test_resources_plural_expands_to_eight_rest_routes() {
    let rs = routes(&draw("  resources :posts"));
    assert_eq!(
        pairs(&rs),
        vec![
            "DELETE /posts/:id",
            "GET /posts",
            "GET /posts/:id",
            "GET /posts/:id/edit",
            "GET /posts/new",
            "PATCH /posts/:id",
            "POST /posts",
            "PUT /posts/:id",
        ]
    );
}

#[test]
fn test_resource_singular_omits_index_and_id() {
    let rs = routes(&draw("  resource :profile"));
    assert_eq!(
        pairs(&rs),
        vec![
            "DELETE /profile",
            "GET /profile",
            "GET /profile/edit",
            "GET /profile/new",
            "PATCH /profile",
            "POST /profile",
            "PUT /profile",
        ]
    );
}

#[test]
fn test_only_option_keeps_listed_actions_alone() {
    let rs = routes(&draw("  resources :posts, only: [:index, :show]"));
    assert_eq!(pairs(&rs), vec!["GET /posts", "GET /posts/:id"]);
}

#[test]
fn test_except_option_drops_listed_actions() {
    let rs = routes(&draw(
        "  resources :posts, except: %i[destroy new edit create update]",
    ));
    assert_eq!(pairs(&rs), vec!["GET /posts", "GET /posts/:id"]);
}

#[test]
fn test_namespace_prefixes_path_and_controller() {
    let rs = routes(&draw(
        "  namespace :admin do\n    resources :posts, only: [:index]\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /admin/posts"]);
    assert_eq!(rs[0].handler.as_deref(), Some("admin/posts#index"));
}

#[test]
fn test_nested_resources_carry_parent_id_parameter() {
    let rs = routes(&draw(
        "  resources :posts do\n    resources :comments, only: [:index]\n  end",
    ));
    assert!(
        has(&rs, "GET", "/posts/:post_id/comments"),
        "{:?}",
        pairs(&rs)
    );
}

#[test]
fn test_nested_under_irregular_plural_singularizes_parent() {
    let rs = routes(&draw(
        "  resources :companies do\n    resources :staff, only: [:index]\n  end",
    ));
    assert!(
        has(&rs, "GET", "/companies/:company_id/staff"),
        "{:?}",
        pairs(&rs)
    );
}

#[test]
fn test_member_block_action_addresses_one_record() {
    let rs = routes(&draw(
        "  resources :agents, only: [] do\n    member do\n      get :confirm_delete\n      patch :toggle_active\n    end\n  end",
    ));
    assert_eq!(
        pairs(&rs),
        vec![
            "GET /agents/:id/confirm_delete",
            "PATCH /agents/:id/toggle_active"
        ]
    );
    assert_eq!(rs[0].handler.as_deref(), Some("agents#confirm_delete"));
}

#[test]
fn test_collection_block_action_omits_record_id() {
    let rs = routes(&draw(
        "  resources :agents, only: [] do\n    collection do\n      get :search\n    end\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /agents/search"]);
}

#[test]
fn test_explicit_verb_keeps_literal_path_and_handler() {
    let rs = routes(&draw(
        r#"  get "/zendesk/chat", to: "zendesk#chat", as: :zendesk_chat"#,
    ));
    assert_eq!(pairs(&rs), vec!["GET /zendesk/chat"]);
    assert_eq!(rs[0].handler.as_deref(), Some("zendesk#chat"));
}

#[test]
fn test_explicit_verb_inherits_enclosing_namespace_prefix() {
    let rs = routes(&draw(
        "  namespace :api do\n    get \"/health\", to: \"health#show\"\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /api/health"]);
}

#[test]
fn test_scope_applies_path_and_module_separately() {
    let rs = routes(&draw(
        "  scope path: \"v1\", module: \"api\" do\n    resources :posts, only: [:index]\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /v1/posts"]);
    assert_eq!(rs[0].handler.as_deref(), Some("api/posts#index"));
}

#[test]
fn test_scope_with_module_only_leaves_path_unchanged() {
    let rs = routes(&draw(
        "  scope module: \"api\" do\n    resources :posts, only: [:index]\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /posts"]);
    assert_eq!(rs[0].handler.as_deref(), Some("api/posts#index"));
}

#[test]
fn test_root_maps_to_slash() {
    let rs = routes(&draw(r#"  root to: "home#index""#));
    assert_eq!(pairs(&rs), vec!["GET /"]);
    assert_eq!(rs[0].handler.as_deref(), Some("home#index"));
}

#[test]
fn test_mount_reports_engine_path_in_both_syntaxes() {
    let rs = routes(&draw(
        "  mount Foo::Engine, at: \"/foo\"\n  mount Bar::Engine => \"/bar\"",
    ));
    assert_eq!(pairs(&rs), vec!["MOUNT /bar", "MOUNT /foo"]);
}

#[test]
fn test_outbound_http_call_outside_draw_block_is_not_a_route() {
    // A service object calling an external API reads like `get "/v1/tag"`.
    // Only a `routes.draw` block declares routes this application serves.
    let rs = routes(
        r#"
        class ListTagsService
          def call
            client.get "/openapi/v1/tag/"
          end
        end
        "#,
    );
    assert!(!has(&rs, "GET", "/openapi/v1/tag/"), "{:?}", pairs(&rs));
}

#[test]
fn test_literal_verb_inside_draw_block_is_reported_once() {
    // The generic `queries.scm` matcher fires on the same line, so the
    // expansion must replace that capture rather than add to it.
    let rs = routes(&draw(r#"  get "/health", to: "health#show""#));
    assert_eq!(pairs(&rs), vec!["GET /health"]);
}

#[test]
fn test_deeply_nested_namespaces_accumulate_every_segment() {
    let rs = routes(&draw(
        "  namespace :api do\n    namespace :v1 do\n      namespace :admin do\n        resources :posts, only: [:show]\n      end\n    end\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /api/v1/admin/posts/:id"]);
    assert_eq!(rs[0].handler.as_deref(), Some("api/v1/admin/posts#show"));
}

#[test]
fn test_file_without_draw_block_yields_no_rails_routes() {
    let rs = routes("class Foo\n  def bar; end\nend\n");
    assert!(rs.is_empty(), "{:?}", pairs(&rs));
}

#[test]
fn test_multiple_resources_on_one_line_each_expand() {
    let rs = routes(&draw("  resources :posts, :comments, only: [:index]"));
    assert_eq!(pairs(&rs), vec!["GET /comments", "GET /posts"]);
}

#[test]
fn test_gem_scope_block_keeps_the_routes_it_wraps() {
    // `devise_scope` contributes no path segment but holds real declarations.
    let rs = routes(&draw(
        "  devise_scope :customer do\n    post \"/customers/verify\", to: \"customers/registrations#verify\"\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["POST /customers/verify"]);
}

#[test]
fn test_concern_definition_declares_no_route() {
    // A concern serves a path only where `concerns:` mounts it.
    let rs = routes(&draw(
        "  concern :commentable do\n    resources :comments\n  end",
    ));
    assert!(rs.is_empty(), "{:?}", pairs(&rs));
}

#[test]
fn test_on_member_inlines_the_member_block() {
    let rs = routes(&draw(
        "  resources :flows, only: [] do\n    get :node_template, on: :member\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /flows/:id/node_template"]);
}

#[test]
fn test_on_member_in_singular_resource_omits_the_id() {
    // A singular resource addresses one record, so there is no id to insert.
    let rs = routes(&draw(
        "  resource :knowledge_base, only: [] do\n    post :validate, on: :member\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["POST /knowledge_base/validate"]);
}

#[test]
fn test_path_option_renames_url_segment_but_not_controller() {
    let rs = routes(&draw(
        r#"  resources :api_keys, path: "api", only: [:index]"#,
    ));
    assert_eq!(pairs(&rs), vec!["GET /api"]);
    assert_eq!(rs[0].handler.as_deref(), Some("api_keys#index"));
}

#[test]
fn test_module_option_moves_controller_but_not_path() {
    let rs = routes(&draw(
        "  resources :awards, only: [:index], module: :achievements",
    ));
    assert_eq!(pairs(&rs), vec!["GET /awards"]);
    assert_eq!(rs[0].handler.as_deref(), Some("achievements/awards#index"));
}

#[test]
fn test_canonical_action_reinstates_the_rest_path() {
    // `only: []` drops destroy; `delete :destroy` puts it back at the
    // resource path, not at `/knowledge_base/destroy`.
    let rs = routes(&draw(
        "  resource :knowledge_base, only: [] do\n    delete :destroy\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["DELETE /knowledge_base"]);
}

#[test]
fn test_non_canonical_action_keeps_its_own_segment() {
    let rs = routes(&draw(
        "  resource :knowledge_base, only: [] do\n    get :confirm_delete\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /knowledge_base/confirm_delete"]);
}

#[test]
fn test_string_path_inside_member_block_hangs_off_the_record() {
    let rs = routes(&draw(
        "  resources :conversations, only: [] do\n    member do\n      get \"ticket_info/:ticket_id\", to: \"conversations#ticket_info\"\n    end\n  end",
    ));
    assert_eq!(
        pairs(&rs),
        vec!["GET /conversations/:id/ticket_info/:ticket_id"]
    );
}

#[test]
fn test_root_path_inside_collection_block_targets_the_collection() {
    let rs = routes(&draw(
        "  resources :shopline, only: [] do\n    collection do\n      delete \"/\", action: :destroy\n    end\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["DELETE /shopline"]);
}

#[test]
fn test_hashrocket_verb_names_path_and_handler() {
    let rs = routes(&draw(
        r#"  get "up" => "rails/health#show", as: :rails_health_check"#,
    ));
    assert_eq!(pairs(&rs), vec!["GET /up"]);
    assert_eq!(rs[0].handler.as_deref(), Some("rails/health#show"));
}

#[test]
fn test_match_declares_one_route_per_via_verb() {
    let rs = routes(&draw(
        "  resources :invites, only: [] do\n    member do\n      match :decline, via: [:get, :post]\n    end\n  end",
    ));
    assert_eq!(
        pairs(&rs),
        vec!["GET /invites/:id/decline", "POST /invites/:id/decline"]
    );
}

#[test]
fn test_match_without_via_declares_no_verb() {
    let rs = routes(&draw(r#"  match "/legacy", to: "legacy#show""#));
    assert!(rs.is_empty(), "{:?}", pairs(&rs));
}

#[test]
fn test_routes_declared_inside_a_helper_method_are_reported() {
    // GitLab wraps its route set in `def draw_all_routes` inside the block.
    let rs = routes(&draw(
        "  def draw_all_routes\n    resources :projects, only: [:index]\n  end\n  draw_all_routes",
    ));
    assert_eq!(pairs(&rs), vec!["GET /projects"]);
}

#[test]
fn test_routes_behind_a_conditional_are_reported() {
    let rs = routes(&draw(
        "  if Rails.env.development?\n    resources :letter_opener, only: [:index]\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /letter_opener"]);
}

#[test]
fn test_http_client_call_with_receiver_is_not_a_route() {
    // `client.get "/openapi/v1/tag"` names an endpoint this application
    // calls. A route DSL sends its verb to the mapper with no receiver.
    let rs = routes(
        r#"
        class ListTagsService
          def call
            @client.get "/openapi/v1/tag/"
            Faraday.post "/files.completeUploadExternal"
          end
        end
        "#,
    );
    assert!(rs.is_empty(), "{:?}", pairs(&rs));
}

#[test]
fn test_sinatra_bare_verb_outside_draw_block_is_still_a_route() {
    // Sinatra declares routes with a bare verb and no draw block, so the
    // receiver check must not cost it its routes.
    let rs = routes(
        r#"
        class App < Sinatra::Base
          get "/health" do
            "ok"
          end
        end
        "#,
    );
    assert!(has(&rs, "get", "/health"), "{:?}", pairs(&rs));
}

#[test]
fn test_faraday_connection_get_with_block_is_not_a_route() {
    // The exact shape in pluto: parenthesised argument plus a block.
    let rs = routes(
        r#"
        class ListTagsService
          private

          def fetch_tags
            connection.get("/openapi/v1/tag/") do |req|
              req.params["name"] = name
            end
          end
        end
        "#,
    );
    assert!(rs.is_empty(), "{:?}", pairs(&rs));
}

// ── Findings from the 2026-09-02 review, one test per fix ───────────────────

fn graph(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    RubyProvider::new()
        .unwrap()
        .parse_file("config/routes.rb".as_ref(), src.as_bytes())
        .unwrap()
}

#[test]
fn test_draw_on_a_non_routes_receiver_is_not_a_route_table() {
    // `canvas.draw do … end` in a report class is not Rails routing: nothing
    // inside it is expanded, and nothing inside it is suppressed. The bare
    // `get "/oops"` therefore reaches the generic matcher exactly as it did
    // before this walker existed, while `resources` stays inert.
    let rs = routes(
        "class C\n  def render\n    canvas.draw do\n      get \"/oops\"\n      resources :shapes\n    end\n  end\nend\n",
    );
    assert_eq!(pairs(&rs), vec!["get /oops"]);
}

#[test]
fn test_engine_routes_draw_is_a_route_table() {
    let rs = routes("Foo::Engine.routes.draw do\n  resources :widgets, only: [:index]\nend\n");
    assert_eq!(pairs(&rs), vec!["GET /widgets"]);
}

#[test]
fn test_receiver_call_inside_draw_block_is_not_a_route() {
    let rs = routes(&draw(
        "  x = client.get(\"/remote\")\n  admin.get \"/withrecv\"\n  get \"/real\", to: \"a#b\"",
    ));
    assert_eq!(pairs(&rs), vec!["GET /real"]);
}

#[test]
fn test_multi_name_resources_apply_the_block_to_every_name() {
    // Rails: Mapper#apply_common_behavior_for passes &block to each name.
    let rs = routes(&draw(
        "  resources :a, :b, only: [] do\n    resources :c, only: [:index]\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /a/:a_id/c", "GET /b/:b_id/c"]);
}

#[test]
fn test_scope_symbol_argument_prefixes_the_path() {
    // Mapper#scope: options[:path] = args.flatten.join("/") — symbols too.
    let rs = routes(&draw(
        "  scope :api do\n    resources :posts, only: [:index]\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /api/posts"]);
}

#[test]
fn test_scope_module_symbol_prefixes_the_controller() {
    let rs = routes(&draw(
        "  scope module: :api do\n    resources :posts, only: [:index]\n  end",
    ));
    assert_eq!(rs[0].handler.as_deref(), Some("api/posts#index"));
}

#[test]
fn test_mount_as_option_is_not_read_as_the_mount_path() {
    let rs = routes(&draw("  mount Foo::Engine, as: \"bar\""));
    assert!(rs.is_empty(), "{:?}", pairs(&rs));
}

#[test]
fn test_namespace_path_option_renames_the_segment_but_not_the_module() {
    let rs = routes(&draw(
        "  namespace :admin, path: \"adm\" do\n    resources :posts, only: [:index]\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /adm/posts"]);
    assert_eq!(rs[0].handler.as_deref(), Some("admin/posts#index"));
}

#[test]
fn test_resource_path_option_accepts_a_symbol() {
    let rs = routes(&draw("  resources :api_keys, path: :api, only: [:index]"));
    assert_eq!(pairs(&rs), vec!["GET /api"]);
}

#[test]
fn test_on_new_attaches_under_the_new_segment() {
    let rs = routes(&draw(
        "  resources :posts, only: [] do\n    get :preview, on: :new\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /posts/new/preview"]);
}

#[test]
fn test_concern_expands_where_concerns_option_mounts_it() {
    let rs = routes(&draw(
        "  concern :exportable do\n    get \"/export.csv\", to: \"exports#csv\"\n    post :bulk, on: :collection\n  end\n  resources :reports, only: [], concerns: :exportable",
    ));
    assert_eq!(
        pairs(&rs),
        vec!["GET /reports/:report_id/export.csv", "POST /reports/bulk"]
    );
}

#[test]
fn test_bare_concerns_call_inside_resource_block_mounts_each_name() {
    let rs = routes(&draw(
        "  concern :a do\n    get :alpha, on: :member\n  end\n  concern :b do\n    get :beta, on: :member\n  end\n  resources :items, only: [] do\n    concerns :a, :b\n  end",
    ));
    assert_eq!(
        pairs(&rs),
        vec!["GET /items/:id/alpha", "GET /items/:id/beta"]
    );
}

#[test]
fn test_concern_definition_alone_still_declares_no_route() {
    let rs = routes(&draw(
        "  concern :x do\n    get \"/foo\", to: \"y#z\"\n  end",
    ));
    assert!(rs.is_empty(), "{:?}", pairs(&rs));
}

#[test]
fn test_interpolated_path_becomes_a_dynamic_segment() {
    // GitLab config/routes.rb:177-183 declares routes inside a %w[].each.
    let rs = routes(&draw(
        "  %w[edit tree].each do |action|\n    get \"/#{action}/*branch\", to: \"ide#index\"\n  end",
    ));
    assert_eq!(pairs(&rs), vec!["GET /:action/*branch"]);
}

#[test]
fn test_router_object_receivers_outside_draw_keep_their_routes() {
    // Hanami sends the verb to a router object; Roda to the request `r`.
    let rs = routes(
        r#"
        router = Hanami::Router.new
        router.get "/hanami/ping", to: "health#ping"
        router.post "/hanami/enqueue", to: "jobs#create"
        class RodaApp < Roda
          route do |r|
            r.get "/roda/ping" do
              "ok"
            end
          end
        end
        connection.get("/openapi/v1/tag/")
        "#,
    );
    assert_eq!(
        pairs(&rs),
        vec!["get /hanami/ping", "get /roda/ping", "post /hanami/enqueue"]
    );
}

#[test]
fn test_block_past_the_depth_cap_keeps_generic_capture_and_records_a_blind_spot() {
    let open: String = (0..34)
        .map(|i| format!("{}namespace :n{i} do\n", " ".repeat(i)))
        .collect();
    let close: String = (0..34)
        .rev()
        .map(|i| format!("{}end\n", " ".repeat(i)))
        .collect();
    let src = draw(&format!("{open}  get \"/deep\", to: \"d#i\"\n{close}"));
    let g = graph(&src);
    // The literal survives as the generic matcher saw it — no prefix, but present.
    assert!(
        g.routes.iter().any(|r| r.path == "/deep"),
        "{:?}",
        pairs(&g.routes)
    );
    assert!(
        g.blind_spots
            .iter()
            .any(|b| b.kind == "rb-rails-routes-depth"),
        "blind spots: {:?}",
        g.blind_spots.iter().map(|b| &b.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_control_flow_does_not_consume_nesting_depth() {
    // 20 nested `if`s cost nothing; only routing blocks count toward the cap.
    let open: String = (0..20).map(|_| "if true\n".to_string()).collect();
    let close: String = (0..20).map(|_| "end\n".to_string()).collect();
    let rs = routes(&draw(&format!(
        "{open}resources :deep, only: [:index]\n{close}"
    )));
    assert_eq!(pairs(&rs), vec!["GET /deep"]);
}
