//! AST-pattern framework detection for C# (ASP.NET / SignalR / Blazor / EFCore).
//!
//! Ported from upstream `_source_code/gitnexus/src/core/ingestion/languages/csharp.ts:153-187`
//! `astFrameworkPatterns`. The matcher is a case-insensitive substring scan
//! of file source — one `RawFrameworkRef` per detected framework, at
//! module level.

use cgn_analyzer::c_sharp::parser::CSharpProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawFrameworkRef;

fn parse(src: &str) -> Vec<RawFrameworkRef> {
    let provider = CSharpProvider::new().unwrap();
    let local = provider
        .parse_file("test.cs".as_ref(), src.as_bytes())
        .unwrap();
    local.framework_refs
}

fn has_framework(refs: &[RawFrameworkRef], framework: &str) -> bool {
    refs.iter().any(|r| r.target_name == framework)
}

#[test]
fn aspnet_httpget_attribute_emits_ref() {
    let src = r#"
        public class UsersController {
            [HttpGet]
            public IActionResult List() => Ok();
        }
    "#;
    assert!(has_framework(&parse(src), "aspnet"));
}

#[test]
fn aspnet_route_attribute_emits_ref() {
    let src = r#"
        [ApiController]
        [Route("api/[controller]")]
        public class UsersController { }
    "#;
    let refs = parse(src);
    assert!(has_framework(&refs, "aspnet"));
    // Two patterns matched — still emit only ONE aspnet ref (dedupe).
    assert_eq!(refs.iter().filter(|r| r.target_name == "aspnet").count(), 1);
}

#[test]
fn signalr_hub_class_emits_ref() {
    let src = r#"
        public class ChatHub : Hub {
            [HubMethodName("send")]
            public Task Send(string msg) => Clients.All.SendAsync(msg);
        }
    "#;
    assert!(has_framework(&parse(src), "signalr"));
}

#[test]
fn blazor_page_directive_emits_ref() {
    // Razor source — content does not have to be valid C# for substring scan.
    let src = r#"
        @page "/counter"
        @code {
            [Parameter] public int Initial { get; set; }
        }
    "#;
    assert!(has_framework(&parse(src), "blazor"));
}

#[test]
fn efcore_dbcontext_emits_ref() {
    let src = r#"
        public class AppDb : DbContext {
            public DbSet<User> Users { get; set; }
            protected override void OnModelCreating(ModelBuilder b) { }
        }
    "#;
    assert!(has_framework(&parse(src), "efcore"));
}

#[test]
fn no_framework_patterns_no_refs() {
    let src = r#"
        public class Plain {
            public int Add(int a, int b) => a + b;
        }
    "#;
    assert!(parse(src).is_empty());
}

#[test]
fn case_insensitive_match() {
    // Patterns are scanned case-insensitively (mirroring upstream).
    let src = r#"
        public class Hub : hub { } // lowercased base type
    "#;
    assert!(has_framework(&parse(src), "signalr"));
}

#[test]
fn multiple_frameworks_in_one_file() {
    let src = r#"
        [ApiController]
        public class Api : Hub {
            public DbSet<User> Users { get; set; }
        }
    "#;
    let refs = parse(src);
    assert!(has_framework(&refs, "aspnet"));
    assert!(has_framework(&refs, "signalr"));
    assert!(has_framework(&refs, "efcore"));
}
