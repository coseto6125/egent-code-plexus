//! Framework detection (Express / Hapi) for the JavaScript parser.
//!
//! Each test parses a small source snippet and asserts that `framework_refs`
//! contains (or doesn't contain) the expected `(target_name, reason)` pair.
//! Detection is gated by the matching `import` statement so the "no import"
//! cases double-check the gate.

use cgn_analyzer::javascript::parser::JavaScriptProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawFrameworkRef;

fn parse(src: &str) -> Vec<RawFrameworkRef> {
    let provider = JavaScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.js".as_ref(), src.as_bytes())
        .unwrap();
    local.framework_refs
}

fn has_ref(refs: &[RawFrameworkRef], target: &str, reason: &str) -> bool {
    refs.iter()
        .any(|r| r.target_name == target && r.reason == reason)
}

#[test]
fn express_get_emits_framework_ref() {
    let src = r#"
        import express from 'express';
        const app = express();
        app.get('/u', handleUsers);
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "handleUsers", "express-route"),
        "expected express-route ref for handleUsers, got: {:?}",
        refs
    );
}

#[test]
fn express_no_import_no_ref() {
    // Same code minus the `import express` — gate must suppress emission.
    let src = r#"
        const app = express();
        app.get('/u', handleUsers);
    "#;
    let refs = parse(src);
    assert!(
        !refs.iter().any(|r| r.reason == "express-route"),
        "expected no express-route refs without import, got: {:?}",
        refs
    );
}

#[test]
fn express_post_route() {
    let src = r#"
        import express from 'express';
        const app = express();
        app.post('/x', h);
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "h", "express-route"),
        "expected express-route ref for h, got: {:?}",
        refs
    );
}

#[test]
fn express_router_chain() {
    let src = r#"
        import express from 'express';
        const router = express.Router();
        router.get('/x', h);
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "h", "express-route"),
        "expected express-route ref for router.get handler, got: {:?}",
        refs
    );
}

#[test]
fn hapi_server_route() {
    let src = r#"
        import Hapi from '@hapi/hapi';
        const server = Hapi.server({});
        server.route({ method: 'GET', path: '/u', handler: getUsers });
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "getUsers", "hapi-route"),
        "expected hapi-route ref for getUsers, got: {:?}",
        refs
    );
}

#[test]
fn hapi_no_import_no_ref() {
    // Same shape minus the `@hapi/hapi` import — gate must suppress emission.
    let src = r#"
        const server = Hapi.server({});
        server.route({ method: 'GET', path: '/u', handler: getUsers });
    "#;
    let refs = parse(src);
    assert!(
        !refs.iter().any(|r| r.reason == "hapi-route"),
        "expected no hapi-route refs without import, got: {:?}",
        refs
    );
}

// ─── Express handler-shape regression (PR #2 review issue #2) ──────────
//
// Pre-fix, the Express query only captured `(identifier)` as the handler,
// so the dominant real-world shapes (arrow, function expr, member access)
// silently emitted ZERO framework_refs even when imports + routes were
// otherwise correct. These tests pin every shape.

#[test]
fn express_arrow_handler_emits_anonymous_ref() {
    let src = r#"
        import express from 'express';
        const app = express();
        app.get('/u', (req, res) => res.json({}));
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "<anonymous>", "express-route"),
        "expected express-route ref with <anonymous> target, got: {:?}",
        refs
    );
}

#[test]
fn express_function_expression_handler_emits_anonymous_ref() {
    let src = r#"
        import express from 'express';
        const app = express();
        app.get('/u', function (req, res) { res.send('ok'); });
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "<anonymous>", "express-route"),
        "expected express-route ref with <anonymous> target, got: {:?}",
        refs
    );
}

#[test]
fn express_member_expression_handler_emits_full_chain() {
    let src = r#"
        import express from 'express';
        const app = express();
        app.get('/u', userRoutes.list);
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "userRoutes.list", "express-route"),
        "expected express-route ref with userRoutes.list target, got: {:?}",
        refs
    );
}

// ─── Express `use` is NOT a route (PR #2 review issue #3) ──────────────
//
// `app.use('/api', router)` mounts middleware; treating it as a route
// would falsely surface `router` as an HTTP handler. The verb list must
// exclude `use`.

#[test]
fn express_use_is_not_a_route() {
    let src = r#"
        import express from 'express';
        const app = express();
        app.use('/api', apiRouter);
    "#;
    let refs = parse(src);
    assert!(
        !has_ref(&refs, "apiRouter", "express-route"),
        "app.use(...) must NOT emit an express-route ref, got: {:?}",
        refs
    );
}
