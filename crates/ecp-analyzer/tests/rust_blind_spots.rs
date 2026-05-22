use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_rs(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    provider
        .parse_file(Path::new("test.rs"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── transmute to fn pointer: blind regardless of arg form ──

#[test]
fn rust_transmute_to_fn_pointer_emits_blind_spot() {
    let src = r#"
fn dispatch(ptr: *const u8) {
    unsafe {
        let f: fn(i32) -> i32 = std::mem::transmute::<_, fn(i32) -> i32>(ptr);
        f(42);
    }
}
"#;
    let g = parse_rs(src);
    assert!(
        kinds(&g).contains(&"rs-transmute-fn"),
        "expected rs-transmute-fn for transmute::<_, fn(...)>(...); got: {:?}",
        kinds(&g)
    );
}

#[test]
fn rust_transmute_unqualified_to_fn_pointer_emits_blind_spot() {
    let src = r#"
use std::mem::transmute;

fn dispatch(ptr: *const u8) {
    unsafe {
        let f: fn() = transmute::<_, fn()>(ptr);
        f();
    }
}
"#;
    let g = parse_rs(src);
    assert!(
        kinds(&g).contains(&"rs-transmute-fn"),
        "expected rs-transmute-fn for imported transmute; got: {:?}",
        kinds(&g)
    );
}

// ── transmute to NON-fn type: NOT blind (typed value conversion) ──

#[test]
fn rust_transmute_to_non_fn_skipped() {
    // transmute::<u64, i64>(x) is a numeric reinterpret, not dispatch.
    let src = r#"
fn convert(x: u64) -> i64 {
    unsafe { std::mem::transmute::<u64, i64>(x) }
}
"#;
    let g = parse_rs(src);
    assert!(
        !kinds(&g).contains(&"rs-transmute-fn"),
        "numeric transmute must NOT emit; got: {:?}",
        kinds(&g)
    );
}

// ── libloading::Library::get ──

#[test]
fn rust_libloading_library_get_emits_blind_spot() {
    let src = r#"
fn load_plugin(path: &str) {
    let lib = libloading::Library::new(path).unwrap();
    let _sym = libloading::Library::get(&lib, b"hook").unwrap();
}
"#;
    let g = parse_rs(src);
    assert!(
        kinds(&g).contains(&"rs-libloading-get"),
        "expected rs-libloading-get; got: {:?}",
        kinds(&g)
    );
}

// ── trait object dispatch: NOT blind (CallMeta path covers it) ──

#[test]
fn rust_trait_object_dispatch_emits_no_blind_spot() {
    let src = r#"
trait Handler { fn handle(&self); }

fn run(h: Box<dyn Handler>) {
    h.handle();
}
"#;
    let g = parse_rs(src);
    assert!(
        !kinds(&g).contains(&"rs-transmute-fn"),
        "dyn Trait dispatch belongs to CallMeta, not BlindSpot; got: {:?}",
        kinds(&g)
    );
    assert!(
        !kinds(&g).contains(&"rs-libloading-get"),
        "dyn Trait dispatch must not be misclassified as libloading; got: {:?}",
        kinds(&g)
    );
}

// ── span shape: outermost call_expression ──

#[test]
fn rust_transmute_span_covers_full_call() {
    let src = "unsafe fn x(p: *const u8) { let _: fn() = std::mem::transmute::<_, fn()>(p); }";
    let g = parse_rs(src);
    let bs = g
        .blind_spots
        .iter()
        .find(|b| b.kind == "rs-transmute-fn")
        .expect("rs-transmute-fn BlindSpot");
    let (sr, _sc, er, _ec) = bs.span;
    // Single-line input — start row == end row.
    assert_eq!(sr, er, "single-line transmute span must stay on one row");
}

// ── negative: ordinary calls produce nothing ──

#[test]
fn rust_ordinary_call_produces_no_blind_spot() {
    let src = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() { let _ = add(1, 2); }";
    let g = parse_rs(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary call must not emit; got: {:?}",
        g.blind_spots
    );
}
