//! Centralized confidence values for framework-aware edges.
//!
//! Rationale for the values (Tier 1+2 + Phase 2 design):
//!   - `0.5` — reflection fan-out base (divided by sqrt(N) downstream)
//!   - `0.6` — FastAPI Depends() — identifier could shadow if function named "Depends"
//!   - `0.8` — Axum Router/Express handler / `--high-trust-only` threshold default
//!   - `0.9` — explicit decorator+function bind (route decorator, attribute macros, NestJS, Spring routes, Django urls/signals/Celery tasks)

pub const FANOUT_BASE: f32 = 0.5;
pub const FASTAPI_DEPENDS: f32 = 0.6;
pub const AXUM_ROUTE: f32 = 0.8;
pub const EXPRESS_ROUTE: f32 = 0.8;
pub const HAPI_ROUTE: f32 = 0.8;
pub const SPRING_AUTOWIRED: f32 = 0.8;
pub const NESTJS_ROUTE: f32 = 0.9;
pub const SPRING_ROUTE: f32 = 0.9;
pub const FASTAPI_ROUTE: f32 = 0.9;
pub const ACTIX_ROUTE: f32 = 0.9;
pub const DJANGO_URL: f32 = 0.9;
pub const CELERY_TASK: f32 = 0.9;
pub const DJANGO_SIGNAL: f32 = 0.9;
pub const KTOR_ROUTE: f32 = 0.9;
pub const ASPNET_ROUTE_ATTR: f32 = 0.9;
pub const ASPNET_MINIMAL_API: f32 = 0.9;
pub const LARAVEL_ROUTE: f32 = 0.9;
pub const RAILS_ROUTE: f32 = 0.9;
pub const GIN_ROUTE: f32 = 0.9;
pub const ECHO_ROUTE: f32 = 0.9;

// ── Wave 3: file-level AST-pattern framework hints (per upstream
// `astFrameworkPatterns`). Lower than route-binding consts because these
// are substring presence checks, not call-site bindings ──
pub const ASPNET_HINT: f32 = 0.8;
pub const SIGNALR_HINT: f32 = 0.8;
pub const BLAZOR_HINT: f32 = 0.7;
pub const EFCORE_HINT: f32 = 0.7;
pub const RAILS_HINT: f32 = 0.8;
pub const SINATRA_HINT: f32 = 0.8;
pub const UIKIT_HINT: f32 = 0.8;
pub const SWIFTUI_HINT: f32 = 0.8;
pub const VAPOR_HINT: f32 = 0.8;
pub const QT_HINT: f32 = 0.8;
pub const FLUTTER_HINT: f32 = 0.8;
pub const RIVERPOD_HINT: f32 = 0.8;
