use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let provider = CProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.c"), src.as_bytes())
        .expect("parse")
}

fn parse_h(src: &str) -> LocalGraph {
    let provider = CProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.h"), src.as_bytes())
        .expect("parse")
}

// --- Round 1: function declarations (prototypes) in .h files ---

#[test]
fn test_c_proto_simple_emits_function() {
    // `dict *dictCreate(dictType *type);` — plain prototype
    let graph = parse_h("dict *dictCreate(dictType *type);\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "dictCreate" && n.kind == NodeKind::Function);
    assert!(
        node.is_some(),
        "expected Function node `dictCreate`, got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_proto_void_return_emits_function() {
    let graph = parse_h("void sha256_init(SHA256_CTX *ctx);\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "sha256_init" && n.kind == NodeKind::Function);
    assert!(
        node.is_some(),
        "expected Function node `sha256_init`, got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_proto_size_t_return_emits_function() {
    let graph = parse_h("size_t hdr_get_memory_size(struct hdr_histogram *h);\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "hdr_get_memory_size" && n.kind == NodeKind::Function);
    assert!(
        node.is_some(),
        "expected Function node `hdr_get_memory_size`, got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_proto_pointer_return_emits_function() {
    let graph = parse_h("char *lpSeek(unsigned char *lp, long index);\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "lpSeek" && n.kind == NodeKind::Function);
    assert!(
        node.is_some(),
        "expected Function node `lpSeek`, got {:#?}",
        graph.nodes
    );
}

// --- Round 2: forward declarations in .c files ---

#[test]
fn test_c_forward_decl_in_c_file_emits_function() {
    // `int verifyClusterNodeId(const char *name, int length);` in cluster_legacy.c
    let graph = parse("int verifyClusterNodeId(const char *name, int length);\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "verifyClusterNodeId" && n.kind == NodeKind::Function);
    assert!(
        node.is_some(),
        "expected Function node `verifyClusterNodeId`, got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_forward_decl_void_emits_function() {
    let graph = parse("void rdbLoadProgressCallback(rio *r, const void *buf, size_t len);\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "rdbLoadProgressCallback" && n.kind == NodeKind::Function);
    assert!(
        node.is_some(),
        "expected Function node `rdbLoadProgressCallback`, got {:#?}",
        graph.nodes
    );
}

// --- Round 3: extern function declarations ---

#[test]
fn test_c_extern_func_proto_emits_function() {
    let graph = parse_h("extern void sz_boot(const sc_data_t *sc_data, bool cache_oblivious);\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "sz_boot" && n.kind == NodeKind::Function);
    assert!(
        node.is_some(),
        "expected Function node `sz_boot`, got {:#?}",
        graph.nodes
    );
}

// --- Rounds 4-7: union_specifier emits a Struct node ---

#[test]
fn test_c_plain_union_emits_struct_node() {
    // `union clusterMsgData { ... };`
    let graph = parse("union bio_job { int x; int y; };\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "bio_job" && n.kind == NodeKind::Struct);
    assert!(
        node.is_some(),
        "expected Struct node for union `bio_job`, got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_typedef_union_emits_struct_and_typedef() {
    // `typedef union typeData { ... } typeData;`
    let graph = parse("typedef union typeData { int i; float f; } typeData;\n");
    let struct_node = graph
        .nodes
        .iter()
        .find(|n| n.name == "typeData" && n.kind == NodeKind::Struct);
    let typedef_node = graph
        .nodes
        .iter()
        .find(|n| n.name == "typeData" && n.kind == NodeKind::Typedef);
    assert!(
        struct_node.is_some(),
        "expected Struct node for union `typeData`, got {:#?}",
        graph.nodes
    );
    assert!(
        typedef_node.is_some(),
        "expected Typedef node `typeData`, got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_anonymous_union_no_crash() {
    // anonymous union — no name, should not panic
    let graph = parse("typedef union { int i; float f; } MyUnion;\n");
    let typedef_node = graph
        .nodes
        .iter()
        .find(|n| n.name == "MyUnion" && n.kind == NodeKind::Typedef);
    assert!(
        typedef_node.is_some(),
        "expected Typedef node `MyUnion`, got {:#?}",
        graph.nodes
    );
}

// --- Regression: existing function_definition still captured ---

#[test]
fn test_c_function_definition_still_captured() {
    let graph = parse("int add(int a, int b) { return a + b; }\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "add" && n.kind == NodeKind::Function);
    assert!(
        node.is_some(),
        "expected Function node `add`, got {:#?}",
        graph.nodes
    );
}

// --- Regression: local variables inside functions not emitted ---

#[test]
fn test_c_local_variable_not_emitted() {
    // `for (int i = 0; ...)` — local var, should not appear as top-level Variable
    let graph = parse("void foo(void) { int i = 0; (void)i; }\n");
    let local = graph
        .nodes
        .iter()
        .find(|n| n.name == "i" && n.kind == NodeKind::Variable);
    assert!(
        local.is_none(),
        "local var `i` must not be emitted, got {:#?}",
        graph.nodes
    );
}

// --- Regression: prototype does not shadow file-scope static check ---

#[test]
fn test_c_static_function_not_exported() {
    let graph = parse("static int helper(void) { return 0; }\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "helper")
        .expect("helper must be emitted");
    assert!(!node.is_exported, "static function must not be exported");
}
