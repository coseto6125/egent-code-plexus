//! Svelte SFC parser tests — `svelte_sfc` module.
//!
//! Covers: basic SFC structure, `<script lang="ts">`, module-scope script,
//! Svelte 5 rune detection, line-number remapping, import extraction, and
//! template directives not leaking JS Function nodes.

use ecp_analyzer::svelte::parser::SvelteProvider;
use ecp_core::graph::NodeKind;

use crate::sfc_helpers;
use sfc_helpers::{find_node, import_sources, node_names_by_kind, parse_with};

fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    parse_with(
        SvelteProvider::new().expect("SvelteProvider::new"),
        "Comp.svelte",
        src,
    )
}

// ── Test 1: Basic SFC — script + template + style → Sections + Function ───────

#[test]
fn basic_sfc_emits_sections_and_function() {
    let src = r#"<script lang="ts">
  import { onMount } from 'svelte';
  let count = 0;
  function increment() { count++; }
  onMount(() => console.log('mounted'));
</script>

<button on:click={increment}>{count}</button>

<style>
  button { color: red; }
</style>
"#;
    let graph = parse(src);

    // Section nodes: script, style.
    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"script"),
        "expected 'script' Section; got {sections:?}"
    );
    assert!(
        sections.contains(&"style"),
        "expected 'style' Section; got {sections:?}"
    );

    // Function declared in <script>.
    let fns = node_names_by_kind(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"increment"),
        "expected Function 'increment'; got {fns:?}"
    );
}

// ── Test 2: <script lang="ts"> triggers TS parsing ───────────────────────────

#[test]
fn script_lang_ts_parses_typescript_interface() {
    let src = r#"<script lang="ts">
  interface User {
    name: string;
    age: number;
  }
  let user: User = { name: 'Alice', age: 30 };
</script>

<p>{user.name}</p>
"#;
    let graph = parse(src);

    // Interface should be captured by TS parser (not available in JS grammar).
    let interfaces = node_names_by_kind(&graph, NodeKind::Interface);
    assert!(
        interfaces.contains(&"User"),
        "expected Interface 'User' from TS parse; got {interfaces:?}"
    );
}

// ── Test 3: Module-scope script <script context="module"> ─────────────────────

#[test]
fn module_scope_script_emits_separate_section() {
    let src = r#"<script context="module" lang="ts">
  export function preload() {
    return {};
  }
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  function init() {}
</script>

<div>hello</div>
"#;
    let graph = parse(src);

    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"script module"),
        "expected 'script module' Section; got {sections:?}"
    );
    assert!(
        sections.contains(&"script"),
        "expected 'script' Section; got {sections:?}"
    );

    // Functions from both blocks should appear.
    let fns = node_names_by_kind(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"preload"),
        "expected Function 'preload' from module script; got {fns:?}"
    );
    assert!(
        fns.contains(&"init"),
        "expected Function 'init' from instance script; got {fns:?}"
    );
}

// ── Test 4: Svelte 5 rune detection — parser does not crash ───────────────────
//
// `$state()`, `$derived()`, `$effect()`, `$props()` are syntactically ordinary
// call expressions. The TS parser captures the LHS binding (variable/const) via
// its normal patterns. This test asserts the variable exists and no panic occurs.

#[test]
fn svelte5_rune_variable_captured_no_crash() {
    let src = r#"<script lang="ts">
  let count = $state(0);
  let doubled = $derived(count * 2);
  $effect(() => {
    console.log(count);
  });
  let { name } = $props();
</script>

<p>{count} {doubled}</p>
"#;
    // Must not panic.
    let graph = parse(src);

    // `count` and `doubled` are `let` bindings — tree-sitter-typescript represents
    // both `const` and `let` as `lexical_declaration`, so the TS queries capture
    // them under `NodeKind::Const`. Either Const or Variable is acceptable.
    let consts = node_names_by_kind(&graph, NodeKind::Const);
    let vars = node_names_by_kind(&graph, NodeKind::Variable);
    let all_bindings: Vec<&str> = consts.iter().chain(vars.iter()).copied().collect();
    assert!(
        all_bindings.contains(&"count"),
        "expected 'count' as Const or Variable (rune LHS); got consts={consts:?} vars={vars:?}"
    );
    assert!(
        all_bindings.contains(&"doubled"),
        "expected 'doubled' as Const or Variable (derived rune LHS); got consts={consts:?} vars={vars:?}"
    );
}

// ── Test 5: Line-number remapping ────────────────────────────────────────────
//
// The SFC below has:
//   line 0: <script lang="ts">
//   line 1: import { onMount } from 'svelte';     ← raw_text starts here (row 1)
//   line 2: function doThing() {
//   line 3:   return 1;
//   line 4: }
//   line 5: </script>
//
// `doThing` is at script-local row 1 (0-indexed after raw_text start),
// which maps to .svelte file row 2 after adding the block's start row (1).

#[test]
fn line_numbers_remapped_to_svelte_file_rows() {
    let src = "<script lang=\"ts\">\nimport { onMount } from 'svelte';\nfunction doThing() {\n  return 1;\n}\n</script>\n<p>hi</p>\n";
    // Line positions (0-indexed):
    // 0: <script lang="ts">
    // 1: import { onMount } from 'svelte';
    // 2: function doThing() {
    // 3:   return 1;
    // 4: }
    // 5: </script>
    let graph = parse(src);
    let node = find_node(&graph, "doThing");
    // span.0 is start_row (0-indexed); doThing starts at line 2 of the .svelte file.
    assert_eq!(
        node.span.0, 2,
        "doThing start row should be 2 (svelte file line 2, 0-indexed); got {}",
        node.span.0
    );
}

// ── Test 6: Import resolution ─────────────────────────────────────────────────

#[test]
fn imports_from_script_are_captured() {
    let src = r#"<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { writable } from 'svelte/store';
  import MyComponent from './MyComponent.svelte';
</script>

<div></div>
"#;
    let graph = parse(src);
    let srcs = import_sources(&graph);

    assert!(
        srcs.contains(&"svelte"),
        "expected import from 'svelte'; got {srcs:?}"
    );
    assert!(
        srcs.contains(&"svelte/store"),
        "expected import from 'svelte/store'; got {srcs:?}"
    );

    let imported_names: Vec<&str> = graph
        .imports
        .iter()
        .map(|i| i.imported_name.as_str())
        .collect();
    assert!(
        imported_names.contains(&"onMount"),
        "expected 'onMount' in imports; got {imported_names:?}"
    );
}

// ── Test 7: Template directives do not produce JS Function nodes ──────────────
//
// `{#each}`, `{#if}`, `{#await}` template directives contain Svelte expressions
// (svelte_raw_text), not re-parsed JS/TS. The parser must NOT emit Function
// nodes for template content.

#[test]
fn template_each_block_does_not_produce_js_functions() {
    let src = r#"<script lang="ts">
  let items = [1, 2, 3];
  function renderItem(x: number) { return x; }
</script>

{#each items as item}
  <p>{item}</p>
{/each}

{#if items.length > 0}
  <span>has items</span>
{/if}
"#;
    let graph = parse(src);

    // Only `renderItem` should appear as a Function — template directives
    // must NOT produce additional Function nodes.
    let fns = node_names_by_kind(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"renderItem"),
        "expected Function 'renderItem'; got {fns:?}"
    );
    // The template raw expressions (items, item) are not JS AST — they must
    // not appear as nodes. Verify no spurious function-like names appear.
    for fn_name in &fns {
        assert!(
            !["items", "item", "each", "if"].contains(fn_name),
            "spurious Function node '{fn_name}' from template content"
        );
    }
}

// ── Test 8: Empty script block — no panic ─────────────────────────────────────

#[test]
fn empty_script_block_no_panic() {
    let src = "<script lang=\"ts\"></script>\n<p>hello</p>\n";
    let graph = parse(src);

    // Should have a script Section but no function/import nodes.
    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"script"),
        "expected script Section in empty SFC; got {sections:?}"
    );
    assert!(
        graph.imports.is_empty(),
        "expected no imports in empty script block"
    );
}
