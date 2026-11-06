//! Snapshot regression tests for the three SFC parser fixtures.
//!
//! Fixtures live in `scripts/parity/sfc_fixtures/`. Node counts are pinned
//! against `scripts/parity/sfc_snapshot_baseline.txt`.
//!
//! Oracle asymmetry: ref-gitnexus supports Vue but NOT Svelte or Astro.
//! Vue counts can be cross-checked against ref-gitnexus; Svelte and Astro
//! are snapshot-only. Section/Folder/File/Document nodes are ecp-only
//! (excluded from ref-gitnexus A/B delta per the inclusive-emission policy).

use ecp_analyzer::{
    astro::parser::AstroProvider, svelte::parser::SvelteProvider, vue::parser::VueProvider,
};
use ecp_core::{analyzer::provider::LanguageProvider, graph::NodeKind};
use std::path::Path;

fn ws_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn fixture(name: &str) -> Vec<u8> {
    let path = ws_root().join("scripts/parity/sfc_fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|_| panic!("fixture missing: {path:?}"))
}

fn count_kind(graph: &ecp_core::analyzer::types::LocalGraph, kind: NodeKind) -> usize {
    graph.nodes.iter().filter(|n| n.kind == kind).count()
}

fn names_of(graph: &ecp_core::analyzer::types::LocalGraph, kind: NodeKind) -> Vec<&str> {
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.name.as_str())
        .collect()
}

fn import_sources(graph: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    graph.imports.iter().map(|i| i.source.as_str()).collect()
}

// ── Vue: Component.vue ───────────────────────────────────────────────────────
//
// Baseline (scripts/parity/sfc_snapshot_baseline.txt):
//   total_nodes: 6  imports: 2
//   Section: 3, Const: 1, Function: 2
//
// ref-gitnexus delta (Section excluded): Const:1 + Function:2 = 3 comparable nodes.
// Acceptable gap vs oracle: ≤15% — 3 nodes is at the low end; delta must be 0
// here because this is a tiny controlled fixture with no ambiguous constructs.

#[test]
fn vue_component_snapshot() {
    let src = fixture("Component.vue");
    let graph = VueProvider::new()
        .expect("VueProvider::new")
        .parse_file(Path::new("Component.vue"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        6,
        "Vue Component.vue: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        2,
        "Vue Component.vue: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        3,
        "Vue: Section count"
    );
    assert_eq!(count_kind(&graph, NodeKind::Const), 1, "Vue: Const count");
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        2,
        "Vue: Function count"
    );

    let fns = names_of(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"increment"),
        "Vue: increment function missing"
    );
    assert!(
        fns.contains(&"decrement"),
        "Vue: decrement function missing"
    );

    let sections = names_of(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"template"),
        "Vue: template section missing"
    );
    assert!(
        sections.contains(&"script setup"),
        "Vue: script setup section missing"
    );
    assert!(sections.contains(&"style"), "Vue: style section missing");

    let import_srcs = import_sources(&graph);
    assert!(
        import_srcs.contains(&"vue"),
        "Vue: import from 'vue' missing"
    );
    assert!(
        import_srcs.contains(&"../composables/useStore"),
        "Vue: useStore import missing"
    );
}

// ── Svelte: Counter.svelte (snapshot-only) ───────────────────────────────────
//
// Baseline (scripts/parity/sfc_snapshot_baseline.txt):
//   total_nodes: 4  imports: 1
//   Section: 2, Const: 1, Function: 1

#[test]
fn svelte_counter_snapshot() {
    let src = fixture("Counter.svelte");
    let graph = SvelteProvider::new()
        .expect("SvelteProvider::new")
        .parse_file(Path::new("Counter.svelte"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        4,
        "Svelte Counter.svelte: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        1,
        "Svelte Counter.svelte: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        2,
        "Svelte: Section count"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        1,
        "Svelte: Const count"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        1,
        "Svelte: Function count"
    );

    let fns = names_of(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"increment"),
        "Svelte: increment function missing"
    );

    let import_srcs = import_sources(&graph);
    assert!(
        import_srcs.contains(&"svelte"),
        "Svelte: import from 'svelte' missing"
    );
}

// ── Astro: Page.astro (snapshot-only) ────────────────────────────────────────
//
// Baseline (scripts/parity/sfc_snapshot_baseline.txt):
//   total_nodes: 6  imports: 2
//   Section: 2, Interface: 1, Property: 2, Const: 1

#[test]
fn astro_page_snapshot() {
    let src = fixture("Page.astro");
    let graph = AstroProvider::new()
        .expect("AstroProvider::new")
        .parse_file(Path::new("Page.astro"), &src)
        .expect("parse_file");

    // Interface property signatures are Property nodes: +2 (User.id, User.name).
    assert_eq!(
        graph.nodes.len(),
        6,
        "Astro Page.astro: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        2,
        "Astro Page.astro: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        2,
        "Astro: Section count"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Interface),
        1,
        "Astro: Interface count"
    );
    assert_eq!(count_kind(&graph, NodeKind::Const), 1, "Astro: Const count");

    let sections = names_of(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"frontmatter"),
        "Astro: frontmatter section missing"
    );
    assert!(
        sections.contains(&"template"),
        "Astro: template section missing"
    );

    let interfaces = names_of(&graph, NodeKind::Interface);
    assert!(
        interfaces.contains(&"User"),
        "Astro: User interface missing"
    );

    let import_srcs = import_sources(&graph);
    assert!(
        import_srcs.contains(&"../layouts/Base.astro"),
        "Astro: Layout import missing"
    );
    assert!(
        import_srcs.contains(&"../api/users"),
        "Astro: fetchUsers import missing"
    );
}

// ── Vue: OptionsApi.vue ──────────────────────────────────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 8  imports: 2
//   Section: 3, Method: 5
//
// Options API object methods (data, computed, methods, lifecycle hooks) are
// captured as Method nodes — no Function nodes expected from this pattern.

#[test]
fn vue_options_api_snapshot_match() {
    let src = fixture("OptionsApi.vue");
    let graph = VueProvider::new()
        .expect("VueProvider::new")
        .parse_file(Path::new("OptionsApi.vue"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        8,
        "Vue OptionsApi.vue: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        2,
        "Vue OptionsApi.vue: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        3,
        "Vue OptionsApi: Section count"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Method),
        5,
        "Vue OptionsApi: Method count (data, doubled, increment, reset, mounted)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        0,
        "Vue OptionsApi: no top-level Function nodes expected in Options API"
    );

    let methods = names_of(&graph, NodeKind::Method);
    for name in &["data", "doubled", "increment", "reset", "mounted"] {
        assert!(
            methods.contains(name),
            "Vue OptionsApi: method '{name}' missing"
        );
    }

    let import_srcs = import_sources(&graph);
    assert!(
        import_srcs.contains(&"vue"),
        "Vue OptionsApi: import from 'vue' missing"
    );
    assert!(
        import_srcs.contains(&"../utils/format"),
        "Vue OptionsApi: format import missing"
    );
}

// ── Vue: PropsAndEmits.vue ───────────────────────────────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 10  imports: 1
//   Section: 2, Interface: 1, Property: 2, Const: 3, Function: 2
//
// Vue 3 macros (defineProps, defineEmits, defineExpose) are captured as
// Const nodes (variable declarations). Interface node from ButtonProps TS type.

#[test]
fn vue_props_and_emits_snapshot_match() {
    let src = fixture("PropsAndEmits.vue");
    let graph = VueProvider::new()
        .expect("VueProvider::new")
        .parse_file(Path::new("PropsAndEmits.vue"), &src)
        .expect("parse_file");

    // Interface property signatures are Property nodes: +2 (ButtonProps.label, disabled).
    assert_eq!(
        graph.nodes.len(),
        10,
        "Vue PropsAndEmits.vue: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        1,
        "Vue PropsAndEmits.vue: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        2,
        "Vue PropsAndEmits: Section count"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Interface),
        1,
        "Vue PropsAndEmits: Interface count (ButtonProps)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        3,
        "Vue PropsAndEmits: Const count (props, emit, isDisabled)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        2,
        "Vue PropsAndEmits: Function count (onClick, reset)"
    );

    let interfaces = names_of(&graph, NodeKind::Interface);
    assert!(
        interfaces.contains(&"ButtonProps"),
        "Vue PropsAndEmits: ButtonProps interface missing"
    );

    let fns = names_of(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"onClick"),
        "Vue PropsAndEmits: onClick missing"
    );
    assert!(fns.contains(&"reset"), "Vue PropsAndEmits: reset missing");

    let sections = names_of(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"script setup"),
        "Vue PropsAndEmits: script setup section missing"
    );
}

// ── Vue: MultiBlock.vue ──────────────────────────────────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 6  imports: 2
//   Section: 3, Const: 1, Function: 2
//
// Both <script> and <script setup> blocks are parsed independently.
// Parser emits a separate Section node for each, so 3 Sections total
// (template + script + script setup). Symbols from both blocks are merged.

#[test]
fn vue_multi_block_snapshot_match() {
    let src = fixture("MultiBlock.vue");
    let graph = VueProvider::new()
        .expect("VueProvider::new")
        .parse_file(Path::new("MultiBlock.vue"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        6,
        "Vue MultiBlock.vue: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        2,
        "Vue MultiBlock.vue: import count changed (one per block)"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        3,
        "Vue MultiBlock: Section count (template, script, script setup)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        1,
        "Vue MultiBlock: Const count (message)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        2,
        "Vue MultiBlock: Function count (sharedHelper, greet)"
    );

    let sections = names_of(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"script"),
        "Vue MultiBlock: plain script section missing"
    );
    assert!(
        sections.contains(&"script setup"),
        "Vue MultiBlock: script setup section missing"
    );
    assert!(
        sections.contains(&"template"),
        "Vue MultiBlock: template section missing"
    );

    let fns = names_of(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"sharedHelper"),
        "Vue MultiBlock: sharedHelper (from plain <script>) missing"
    );
    assert!(
        fns.contains(&"greet"),
        "Vue MultiBlock: greet (from <script setup>) missing"
    );
}

// ── Svelte: ContextModule.svelte (snapshot-only) ─────────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 7  imports: 2
//   Section: 3, Const: 2, Function: 2
//
// <script context="module"> emits Section named "script module".
// Both blocks parsed independently; symbols from both are merged into graph.

#[test]
fn svelte_context_module_snapshot_match() {
    let src = fixture("ContextModule.svelte");
    let graph = SvelteProvider::new()
        .expect("SvelteProvider::new")
        .parse_file(Path::new("ContextModule.svelte"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        7,
        "Svelte ContextModule.svelte: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        2,
        "Svelte ContextModule.svelte: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        3,
        "Svelte ContextModule: Section count (style, script module, script)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        2,
        "Svelte ContextModule: Const count (globalCount, name)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        2,
        "Svelte ContextModule: Function count (formatCount, greet)"
    );

    let sections = names_of(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"script module"),
        "Svelte ContextModule: 'script module' section missing"
    );
    assert!(
        sections.contains(&"script"),
        "Svelte ContextModule: instance 'script' section missing"
    );

    let fns = names_of(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"formatCount"),
        "Svelte ContextModule: formatCount (from module block) missing"
    );
    assert!(
        fns.contains(&"greet"),
        "Svelte ContextModule: greet (from instance block) missing"
    );

    let import_srcs = import_sources(&graph);
    assert!(
        import_srcs.contains(&"svelte/store"),
        "Svelte ContextModule: svelte/store import missing"
    );
}

// ── Svelte: Runes.svelte (snapshot-only) ─────────────────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 4  imports: 0
//   Section: 1, Const: 2, Function: 1
//
// Svelte 5 runes: $state/$derived LHS captured as Const; $effect (no LHS)
// not captured. No imports, no style block.

#[test]
fn svelte_runes_snapshot_match() {
    let src = fixture("Runes.svelte");
    let graph = SvelteProvider::new()
        .expect("SvelteProvider::new")
        .parse_file(Path::new("Runes.svelte"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        4,
        "Svelte Runes.svelte: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        0,
        "Svelte Runes.svelte: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        1,
        "Svelte Runes: Section count (script only)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        2,
        "Svelte Runes: Const count ($state/$derived vars)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        1,
        "Svelte Runes: Function count (increment)"
    );

    let consts = names_of(&graph, NodeKind::Const);
    assert!(
        consts.contains(&"count"),
        "Svelte Runes: count ($state) missing"
    );
    assert!(
        consts.contains(&"doubled"),
        "Svelte Runes: doubled ($derived) missing"
    );

    let fns = names_of(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"increment"),
        "Svelte Runes: increment missing"
    );
}

// ── Svelte: TemplateDirectives.svelte (snapshot-only) ────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 5  imports: 0
//   Section: 1, Const: 3, Function: 1
//
// {#if}/{#each}/{#await} template directives are NOT parsed as JS and must
// NOT produce any Function/Const nodes — only script block symbols appear.

#[test]
fn svelte_template_directives_snapshot_match() {
    let src = fixture("TemplateDirectives.svelte");
    let graph = SvelteProvider::new()
        .expect("SvelteProvider::new")
        .parse_file(Path::new("TemplateDirectives.svelte"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        5,
        "Svelte TemplateDirectives.svelte: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        0,
        "Svelte TemplateDirectives.svelte: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        1,
        "Svelte TemplateDirectives: Section count (script only — no template Section)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        3,
        "Svelte TemplateDirectives: Const count (items, show, promise)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        1,
        "Svelte TemplateDirectives: Function count (loadData only — template directives do not leak)"
    );

    let consts = names_of(&graph, NodeKind::Const);
    for name in &["items", "show", "promise"] {
        assert!(
            consts.contains(name),
            "Svelte TemplateDirectives: '{name}' missing"
        );
    }

    let fns = names_of(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"loadData"),
        "Svelte TemplateDirectives: loadData missing"
    );
}

// ── Astro: MultipleImports.astro (snapshot-only) ─────────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 4  imports: 5
//   Section: 2, Const: 2
//
// Five imports (3 Astro components + 2 utility named imports) all captured.
// Only top-level const declarations captured (no template JSX leakage).

#[test]
fn astro_multiple_imports_snapshot_match() {
    let src = fixture("MultipleImports.astro");
    let graph = AstroProvider::new()
        .expect("AstroProvider::new")
        .parse_file(Path::new("MultipleImports.astro"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        4,
        "Astro MultipleImports.astro: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        5,
        "Astro MultipleImports.astro: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        2,
        "Astro MultipleImports: Section count (frontmatter, template)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        2,
        "Astro MultipleImports: Const count (title, posts)"
    );

    let consts = names_of(&graph, NodeKind::Const);
    assert!(
        consts.contains(&"title"),
        "Astro MultipleImports: title const missing"
    );
    assert!(
        consts.contains(&"posts"),
        "Astro MultipleImports: posts const missing"
    );

    let import_srcs = import_sources(&graph);
    for src in &[
        "../components/Header.astro",
        "../components/Footer.astro",
        "../components/Card.astro",
        "../utils/slug",
        "../utils/date",
    ] {
        assert!(
            import_srcs.contains(src),
            "Astro MultipleImports: import from '{src}' missing"
        );
    }
}

// ── Astro: ConditionalRender.astro (snapshot-only) ───────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 4  imports: 1
//   Section: 2, Const: 2
//
// Template JSX expressions ({cond && <X />}, {arr.map(...)}) are NOT parsed
// and must NOT produce Function nodes — only frontmatter symbols appear.

#[test]
fn astro_conditional_render_snapshot_match() {
    let src = fixture("ConditionalRender.astro");
    let graph = AstroProvider::new()
        .expect("AstroProvider::new")
        .parse_file(Path::new("ConditionalRender.astro"), &src)
        .expect("parse_file");

    assert_eq!(
        graph.nodes.len(),
        4,
        "Astro ConditionalRender.astro: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        1,
        "Astro ConditionalRender.astro: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        2,
        "Astro ConditionalRender: Section count (frontmatter, template)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        2,
        "Astro ConditionalRender: Const count (isLoggedIn, users)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Function),
        0,
        "Astro ConditionalRender: no Function nodes (template JSX must not leak)"
    );

    let consts = names_of(&graph, NodeKind::Const);
    assert!(
        consts.contains(&"isLoggedIn"),
        "Astro ConditionalRender: isLoggedIn missing"
    );
    assert!(
        consts.contains(&"users"),
        "Astro ConditionalRender: users missing"
    );

    let import_srcs = import_sources(&graph);
    assert!(
        import_srcs.contains(&"../components/Alert.astro"),
        "Astro ConditionalRender: Alert.astro import missing"
    );
}

// ── Astro: TypedProps.astro (snapshot-only) ──────────────────────────────────
//
// Baseline (sfc_snapshot_baseline.txt):
//   total_nodes: 7  imports: 1
//   Section: 2, Interface: 1, Property: 3, Const: 1
//
// TypeScript Interface node captured from Props interface.
// Destructure pattern `const { title, ... } = Astro.props` does NOT emit
// individual Const nodes (only the slug const is captured separately).

#[test]
fn astro_typed_props_snapshot_match() {
    let src = fixture("TypedProps.astro");
    let graph = AstroProvider::new()
        .expect("AstroProvider::new")
        .parse_file(Path::new("TypedProps.astro"), &src)
        .expect("parse_file");

    // Interface property signatures are Property nodes: +3 (Props.title, description, imageUrl).
    assert_eq!(
        graph.nodes.len(),
        7,
        "Astro TypedProps.astro: total node count changed"
    );
    assert_eq!(
        graph.imports.len(),
        1,
        "Astro TypedProps.astro: import count changed"
    );

    assert_eq!(
        count_kind(&graph, NodeKind::Section),
        2,
        "Astro TypedProps: Section count (frontmatter, template)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Interface),
        1,
        "Astro TypedProps: Interface count (Props)"
    );
    assert_eq!(
        count_kind(&graph, NodeKind::Const),
        1,
        "Astro TypedProps: Const count (slug)"
    );

    let interfaces = names_of(&graph, NodeKind::Interface);
    assert!(
        interfaces.contains(&"Props"),
        "Astro TypedProps: Props interface missing"
    );

    let consts = names_of(&graph, NodeKind::Const);
    assert!(
        consts.contains(&"slug"),
        "Astro TypedProps: slug const missing"
    );

    let import_srcs = import_sources(&graph);
    assert!(
        import_srcs.contains(&"astro:assets"),
        "Astro TypedProps: astro:assets import missing"
    );
}
