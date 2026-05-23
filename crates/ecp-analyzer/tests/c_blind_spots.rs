use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_c(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = CProvider::new().expect("CProvider::new");
    provider
        .parse_file(Path::new("test.c"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── dlsym: runtime symbol resolution ──

#[test]
fn c_dlsym_emits_blind_spot() {
    let src = r#"
#include <dlfcn.h>

void load(void *handle, const char *name) {
    void *sym = dlsym(handle, name);
    (void)sym;
}
"#;
    let g = parse_c(src);
    assert!(
        kinds(&g).contains(&"c-dlsym"),
        "expected c-dlsym; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn c_dlsym_with_literal_name_still_emits_blind_spot() {
    // Even a literal symbol name produces a dlsym record — the LOADED
    // function body is unknown to the static graph regardless of how
    // the name was supplied (mirrors Java Class.forName convention).
    let src = r#"
#include <dlfcn.h>

int main() {
    void *handle = NULL;
    void *fn = dlsym(handle, "init");
    return 0;
}
"#;
    let g = parse_c(src);
    assert!(
        kinds(&g).contains(&"c-dlsym"),
        "expected c-dlsym for literal name; got: {:?}",
        kinds(&g)
    );
}

// ── negative ──

#[test]
fn c_ordinary_call_emits_no_blind_spot() {
    let src = r#"
int add(int a, int b) { return a + b; }
int main(void) { return add(1, 2); }
"#;
    let g = parse_c(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary call must not emit; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn c_function_pointer_call_is_callmeta_not_blindspot() {
    // Function-pointer dispatch is graph-traversable via CallMeta
    // (indirect_dispatch.rs) — must NOT also fire as BlindSpot.
    let src = r#"
typedef int (*op_t)(int, int);
int dispatch(op_t op, int a, int b) {
    return op(a, b);
}
"#;
    let g = parse_c(src);
    assert!(
        !kinds(&g).contains(&"c-dlsym"),
        "function pointer dispatch must not match dlsym; got: {:?}",
        kinds(&g)
    );
}
