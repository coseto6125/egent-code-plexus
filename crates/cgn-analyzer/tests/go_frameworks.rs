//! Framework detection (gin / echo) for the Go parser.
//!
//! Both frameworks share `r.METHOD("/path", handler)` shape, so detection
//! is fully gated by the imported package — same source code with
//! different imports yields different `RawFrameworkRef`s. Ported from
//! upstream `gitnexus/src/core/group/extractors/http-patterns/go.ts`.

use cgn_analyzer::go::parser::GoProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawFrameworkRef;

fn parse(src: &str) -> Vec<RawFrameworkRef> {
    let provider = GoProvider::new().unwrap();
    let local = provider
        .parse_file("test.go".as_ref(), src.as_bytes())
        .unwrap();
    local.framework_refs
}

fn has_ref(refs: &[RawFrameworkRef], target: &str, reason: &str) -> bool {
    refs.iter()
        .any(|r| r.target_name == target && r.reason == reason)
}

#[test]
fn gin_get_route_emits_framework_ref() {
    let src = r#"
        package main
        import "github.com/gin-gonic/gin"
        func main() {
            r := gin.Default()
            r.GET("/users", listUsers)
        }
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "listUsers", "gin-route"),
        "expected gin-route ref for listUsers, got: {:?}",
        refs
    );
}

#[test]
fn gin_post_route_emits_framework_ref() {
    let src = r#"
        package main
        import "github.com/gin-gonic/gin"
        func main() {
            r := gin.Default()
            r.POST("/users", createUser)
        }
    "#;
    let refs = parse(src);
    assert!(has_ref(&refs, "createUser", "gin-route"));
}

#[test]
fn gin_no_import_no_ref() {
    // Same route shape but no gin import — must not emit a gin-route.
    let src = r#"
        package main
        func main() {
            r := router{}
            r.GET("/users", listUsers)
        }
    "#;
    let refs = parse(src);
    assert!(
        !refs.iter().any(|r| r.reason == "gin-route"),
        "must not emit gin-route without import, got: {:?}",
        refs
    );
}

#[test]
fn echo_get_route_emits_framework_ref() {
    let src = r#"
        package main
        import "github.com/labstack/echo/v4"
        func main() {
            e := echo.New()
            e.GET("/users", listUsers)
        }
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "listUsers", "echo-route"),
        "expected echo-route ref for listUsers, got: {:?}",
        refs
    );
}

#[test]
fn echo_no_import_no_ref() {
    let src = r#"
        package main
        func main() {
            e := server{}
            e.GET("/users", listUsers)
        }
    "#;
    let refs = parse(src);
    assert!(!refs.iter().any(|r| r.reason == "echo-route"));
}

#[test]
fn gin_and_echo_share_no_ambiguity() {
    // When gin is imported, an echo-shape call must NOT be tagged as echo.
    // The import gate is the discriminator.
    let src = r#"
        package main
        import "github.com/gin-gonic/gin"
        func main() {
            r := gin.Default()
            r.GET("/u", h)
        }
    "#;
    let refs = parse(src);
    assert!(has_ref(&refs, "h", "gin-route"));
    assert!(!refs.iter().any(|r| r.reason == "echo-route"));
}
