use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_cpp(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = CppProvider::new().expect("CppProvider::new");
    provider
        .parse_file(Path::new("test.cpp"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── dlsym: runtime symbol resolution ──

#[test]
fn cpp_dlsym_emits_blind_spot() {
    let src = r#"
#include <dlfcn.h>

void load(void *handle, const char *name) {
    auto sym = dlsym(handle, name);
    (void)sym;
}
"#;
    let g = parse_cpp(src);
    assert!(
        kinds(&g).contains(&"cpp-dlsym"),
        "expected cpp-dlsym; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn cpp_dlsym_with_literal_name_still_emits_blind_spot() {
    let src = r#"
#include <dlfcn.h>

int main() {
    auto fn = dlsym(nullptr, "init");
    return 0;
}
"#;
    let g = parse_cpp(src);
    assert!(
        kinds(&g).contains(&"cpp-dlsym"),
        "expected cpp-dlsym for literal name; got: {:?}",
        kinds(&g)
    );
}

// ── negative ──

#[test]
fn cpp_ordinary_call_emits_no_blind_spot() {
    let src = r#"
int add(int a, int b) { return a + b; }
int main() { return add(1, 2); }
"#;
    let g = parse_cpp(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary call must not emit; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn cpp_virtual_method_dispatch_is_callmeta_not_blindspot() {
    // Virtual method dispatch is graph-traversable via CallMeta — must
    // NOT also fire as BlindSpot.
    let src = r#"
class Base {
public:
    virtual void handle() = 0;
};
void run(Base *b) { b->handle(); }
"#;
    let g = parse_cpp(src);
    assert!(
        !kinds(&g).contains(&"cpp-dlsym"),
        "virtual dispatch must not match dlsym; got: {:?}",
        kinds(&g)
    );
}
