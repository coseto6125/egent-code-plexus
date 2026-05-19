//! Ktor (Kotlin) framework detection — Wave 2 task B2.
//!
//! Verifies the Kotlin parser emits `RawFrameworkRef` entries for
//! `get/post/put/delete/patch(...) { ... }` route DSL calls when the file
//! actually imports `io.ktor.*`. The import gate prevents over-claiming on
//! unrelated `get(...)` / `post(...)` calls (those tokens are common
//! identifiers outside Ktor).

use cgn_analyzer::kotlin::parser::KotlinProvider;
use cgn_core::analyzer::provider::LanguageProvider;

fn parse(src: &str) -> cgn_core::analyzer::types::LocalGraph {
    let provider = KotlinProvider::new().expect("KotlinProvider::new");
    provider
        .parse_file("Test.kt".as_ref(), src.as_bytes())
        .expect("parse_file")
}

#[test]
fn ktor_get_route() {
    let src = r#"import io.ktor.server.routing.*

fun Application.module() {
    routing {
        get("/users") {
            call.respondText("ok")
        }
    }
}
"#;
    let graph = parse(src);
    let refs: Vec<_> = graph
        .framework_refs
        .iter()
        .filter(|r| r.reason.starts_with("ktor-route"))
        .collect();
    assert_eq!(refs.len(), 1, "framework_refs: {:?}", graph.framework_refs);
    let r = refs[0];
    assert_eq!(r.target_name, "/users");
    assert_eq!(r.reason, "ktor-route-get");
}

#[test]
fn ktor_post_route() {
    let src = r#"import io.ktor.server.routing.*

fun Application.module() {
    routing {
        post("/x") {
            call.respondText("done")
        }
    }
}
"#;
    let graph = parse(src);
    let refs: Vec<_> = graph
        .framework_refs
        .iter()
        .filter(|r| r.reason == "ktor-route-post")
        .collect();
    assert_eq!(refs.len(), 1, "framework_refs: {:?}", graph.framework_refs);
    assert_eq!(refs[0].target_name, "/x");
}

#[test]
fn ktor_no_import_no_ref() {
    // Same route DSL shape but no `io.ktor` import — gate must suppress refs.
    let src = r#"
fun Application.module() {
    routing {
        get("/users") {
            call.respondText("ok")
        }
    }
}
"#;
    let graph = parse(src);
    assert!(
        graph.framework_refs.is_empty(),
        "expected no framework_refs without io.ktor import, got: {:?}",
        graph.framework_refs
    );
}
