//! HTTP surface: `/api/meta` (tools, limits), `/api/repos` (list / add a
//! GitHub repository), `/api/run` (one spawn), `/healthz`, and the embedded
//! UI for everything else.

use crate::ratelimit::RateLimiter;
use crate::repos::{parse_github_repo, Programs, RepoStore, Status, StoreConfig};
use crate::runner::{RunError, Runner};
use crate::tools::{demo_tools, DemoTool};
use crate::Config;
use axum::body::Body;
use axum::extract::{FromRequestParts, Path as UrlPath, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use clap::CommandFactory;
use ecp_cli::cli::Cli;
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

static UI: Dir = include_dir!("$CARGO_MANIFEST_DIR/ui");

pub struct AppState {
    tools: Vec<DemoTool>,
    tool_index: HashMap<String, usize>,
    pub repos: Arc<RepoStore>,
    runner: Runner,
    run_limiter: RateLimiter,
    add_limiter: RateLimiter,
    trusted_hops: usize,
}

impl AppState {
    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&config.repos_dir)
            .map_err(|e| anyhow::anyhow!("creating {}: {e}", config.repos_dir.display()))?;
        let store = RepoStore::new(StoreConfig {
            dir: config.repos_dir.clone(),
            programs: Programs {
                ecp: config.bin.clone(),
                git: config.git.clone(),
                curl: config.curl.clone(),
            },
            max_repo_kb: config.max_repo_kb,
            max_repos: config.max_repos,
            queue_limit: config.queue_limit,
            clone_timeout: config.clone_timeout,
            index_timeout: config.index_timeout,
            github_token: config.github_token.clone(),
        });
        let runner = Runner::new(
            config.bin.clone(),
            config.timeout,
            config.queue_wait,
            config.max_output_bytes,
            config.concurrency,
        );
        Ok(Self::new(
            demo_tools(&Cli::command()),
            Arc::new(store),
            runner,
            RateLimiter::per_minute(config.rate_per_min),
            RateLimiter::new(config.add_rate_per_hour, Duration::from_secs(3600)),
            config.trusted_hops,
        ))
    }

    pub fn new(
        tools: Vec<DemoTool>,
        repos: Arc<RepoStore>,
        runner: Runner,
        run_limiter: RateLimiter,
        add_limiter: RateLimiter,
        trusted_hops: usize,
    ) -> Self {
        let tool_index = tools
            .iter()
            .enumerate()
            .map(|(i, t)| (t.inner.subcommand.clone(), i))
            .collect();
        Self {
            tools,
            tool_index,
            repos,
            runner,
            run_limiter,
            add_limiter,
            trusted_hops,
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/meta", get(meta))
        .route("/api/repos", get(list_repos).post(add_repo))
        .route("/api/run", post(run))
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(|| async { serve_ui("index.html") }))
        .route(
            "/{*path}",
            get(|UrlPath(path): UrlPath<String>| async move { serve_ui(&path) }),
        )
        .layer(middleware::from_fn(cors))
        .with_state(state)
}

/// Public read-only API: any origin may call it, so a static page hosted
/// elsewhere (GitHub Pages, a tunnel front) can use the same backend.
async fn cors(req: Request<Body>, next: Next) -> Response<Body> {
    let mut resp = if req.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };
    let headers = resp.headers_mut();
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    headers.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        "access-control-allow-headers",
        HeaderValue::from_static("content-type"),
    );
    resp
}

fn serve_ui(path: &str) -> Response<Body> {
    let Some(file) = UI.get_file(path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let mime = match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    };
    (
        [(CONTENT_TYPE, mime), (CACHE_CONTROL, "public, max-age=300")],
        file.contents(),
    )
        .into_response()
}

async fn meta(State(state): State<Arc<AppState>>) -> Json<Value> {
    let store = state.repos.config();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "tools": state.tools.iter().map(DemoTool::listing).collect::<Vec<_>>(),
        "limits": {
            "timeout_secs": state.runner.timeout().as_secs(),
            "max_output_bytes": state.runner.max_output_bytes(),
            "max_repo_mb": store.max_repo_kb / 1024,
            "max_repos": store.max_repos,
            "index_timeout_secs": store.index_timeout.as_secs(),
        },
    }))
}

type ApiError = (StatusCode, Json<Value>);

fn api_error(status: StatusCode, msg: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": msg.into() })))
}

fn rate_limited() -> ApiError {
    api_error(StatusCode::TOO_MANY_REQUESTS, "rate limit: wait a minute")
}

/// The page polls this while a build runs, so it shares the run budget.
async fn list_repos(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
) -> Result<Json<Value>, ApiError> {
    if !state.run_limiter.allow(ip) {
        return Err(rate_limited());
    }
    Ok(Json(json!({ "repos": state.repos.list() })))
}

#[derive(Deserialize)]
struct AddRequest {
    url: String,
}

async fn add_repo(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    Json(req): Json<AddRequest>,
) -> Result<Response<Body>, ApiError> {
    let (owner, repo) =
        parse_github_repo(&req.url).map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
    if !state.add_limiter.allow(ip) {
        return Err(api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit: this address added too many repositories this hour",
        ));
    }
    let entry = state
        .repos
        .add(&owner, &repo)
        .map_err(|e| api_error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let status = if entry.status == Status::Ready {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((status, Json(json!({ "repo": entry }))).into_response())
}

#[derive(Deserialize)]
struct RunRequest {
    tool: String,
    repo: String,
    #[serde(default = "empty_object")]
    args: Value,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

async fn run(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    Json(req): Json<RunRequest>,
) -> Result<Response<Body>, ApiError> {
    if !state.run_limiter.allow(ip) {
        return Err(rate_limited());
    }
    let tool = state
        .tool_index
        .get(&req.tool)
        .map(|&i| &state.tools[i])
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                format!("unknown tool {:?}", req.tool),
            )
        })?;
    let (corpus, _lease) = state
        .repos
        .ready_path(&req.repo)
        .map_err(|status| match status {
            None => api_error(
                StatusCode::BAD_REQUEST,
                format!("unknown repo {:?}; add it first", req.repo),
            ),
            Some(Status::Failed) => api_error(
                StatusCode::CONFLICT,
                format!("{} failed to index; add it again to retry", req.repo),
            ),
            Some(_) => api_error(
                StatusCode::CONFLICT,
                format!("{} is still being indexed", req.repo),
            ),
        })?;
    match state.runner.run(tool, &corpus, &req.args).await {
        Ok(outcome) if outcome.timed_out => {
            Ok((StatusCode::GATEWAY_TIMEOUT, Json(outcome)).into_response())
        }
        Ok(outcome) => Ok(Json(outcome).into_response()),
        Err(e @ (RunError::BadArgs(_) | RunError::Reserved(_))) => {
            Err(api_error(StatusCode::BAD_REQUEST, e.to_string()))
        }
        Err(e @ RunError::Busy) => Err(api_error(StatusCode::SERVICE_UNAVAILABLE, e.to_string())),
        Err(e @ RunError::Spawn(_)) => {
            Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
    }
}

/// Client address for rate limiting. A proxy appends the address it saw to
/// the right of `x-forwarded-for`, so the trusted value is counted from the
/// end; the leftmost entries are whatever the client chose to send.
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<Arc<AppState>> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(client_ip(
            &parts.headers,
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip()),
            state.trusted_hops,
        )))
    }
}

fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>, trusted_hops: usize) -> IpAddr {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .filter(|_| trusted_hops > 0)
        .and_then(|v| {
            let hops: Vec<&str> = v.split(',').map(str::trim).collect();
            hops.len()
                .checked_sub(trusted_hops)
                .and_then(|i| hops[i].parse().ok())
        });
    forwarded
        .or(peer)
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xff(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        h
    }

    const PEER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 9));

    #[test]
    fn client_ip_takes_the_hop_the_trusted_proxy_appended() {
        assert_eq!(
            client_ip(&xff("6.6.6.6, 203.0.113.5"), Some(PEER), 1),
            "203.0.113.5".parse::<IpAddr>().unwrap(),
            "the leftmost hop is client-supplied and must not win"
        );
        assert_eq!(
            client_ip(&xff("6.6.6.6, 203.0.113.5, 198.51.100.1"), Some(PEER), 2),
            "203.0.113.5".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn client_ip_falls_back_to_the_peer_when_the_header_is_short_or_untrusted() {
        assert_eq!(client_ip(&xff("6.6.6.6"), Some(PEER), 2), PEER);
        assert_eq!(client_ip(&xff("6.6.6.6"), Some(PEER), 0), PEER);
        assert_eq!(client_ip(&xff("not-an-ip"), Some(PEER), 1), PEER);
        assert_eq!(client_ip(&HeaderMap::new(), Some(PEER), 1), PEER);
    }
}
