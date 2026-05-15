//! Framework detection (Express / Hapi) for the JavaScript parser.
//!
//! Each test parses a small source snippet and asserts that `framework_refs`
//! contains (or doesn't contain) the expected `(target_name, reason)` pair.
//! Detection is gated by the matching `import` statement so the "no import"
//! cases double-check the gate.

use graph_nexus_analyzer::javascript::parser::JavaScriptProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawFrameworkRef;

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
