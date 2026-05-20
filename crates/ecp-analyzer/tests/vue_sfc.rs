//! Vue SFC parser tests — `vue_sfc` module.
//!
//! Covers: basic SFC structure, `<script setup lang="ts">`, line-number
//! remapping, import extraction, and multi-script (regular + setup) SFCs.

use ecp_analyzer::vue::parser::VueProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    VueProvider::new()
        .expect("VueProvider::new")
        .parse_file(Path::new("Comp.vue"), src.as_bytes())
        .expect("parse_file")
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn node_names_by_kind(graph: &ecp_core::analyzer::types::LocalGraph, kind: NodeKind) -> Vec<&str> {
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.name.as_str())
        .collect()
}

fn find_node<'a>(
    graph: &'a ecp_core::analyzer::types::LocalGraph,
    name: &str,
) -> &'a ecp_core::analyzer::types::RawNode {
    graph
        .nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| {
            let names: Vec<_> = graph.nodes.iter().map(|n| &n.name).collect();
            panic!("node `{name}` not found; graph contains: {names:#?}")
        })
}

fn import_sources(graph: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    graph.imports.iter().map(|i| i.source.as_str()).collect()
}

// ── Test 1: Basic SFC — template + script + style → Section nodes + Function ─

#[test]
fn basic_sfc_emits_sections_and_function() {
    let src = r#"<template>
  <button @click="handleClick">{{ message }}</button>
</template>

<script setup lang="ts">
import { ref } from 'vue'
const message = ref('Hello')
function handleClick() {
  console.log(message.value)
}
</script>

<style>
button { color: red; }
</style>
"#;
    let graph = parse(src);

    // Section nodes: template, script setup, style.
    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"template"),
        "expected 'template' Section; got {sections:?}"
    );
    assert!(
        sections.contains(&"script setup"),
        "expected 'script setup' Section; got {sections:?}"
    );
    assert!(
        sections.contains(&"style"),
        "expected 'style' Section; got {sections:?}"
    );

    // Function declared in <script setup>.
    let fns = node_names_by_kind(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"handleClick"),
        "expected Function 'handleClick'; got {fns:?}"
    );
}

// ── Test 2: <script setup lang="ts"> triggers TS parsing ─────────────────────

#[test]
fn script_setup_ts_parses_typescript_nodes() {
    let src = r#"<template><div /></template>

<script setup lang="ts">
import { defineProps } from 'vue'
interface Props {
  title: string
}
defineProps<Props>()
const count = ref<number>(0)
</script>
"#;
    let graph = parse(src);

    // Interface should be captured by TS parser (not in JS grammar).
    let interfaces = node_names_by_kind(&graph, NodeKind::Interface);
    assert!(
        interfaces.contains(&"Props"),
        "expected Interface 'Props' from TS parse; got {interfaces:?}"
    );

    // Import from 'vue' should be captured (import.source captures string_fragment, no quotes).
    let srcs = import_sources(&graph);
    assert!(
        srcs.contains(&"vue"),
        "expected import from 'vue'; got {srcs:?}"
    );
}

// ── Test 3: Line-number remapping ────────────────────────────────────────────
//
// The SFC below has:
//   line 0: <template>...</template>
//   line 1: (empty)
//   line 2: <script setup lang="ts">
//   line 3: import { ref } from 'vue'  ← raw_text starts here (row 3 in .vue)
//   line 4: function doThing() {
//   line 5:   return 1
//   line 6: }
//   line 7: </script>
//
// `doThing` is at script-local row 1 (0-indexed after the raw_text start),
// which maps to .vue file row 4 after adding the block's start row (3).

#[test]
fn line_numbers_remapped_to_vue_file_rows() {
    let src = "<template><div /></template>\n\n<script setup lang=\"ts\">\nimport { ref } from 'vue'\nfunction doThing() {\n  return 1\n}\n</script>\n";
    // Line positions (0-indexed):
    // 0: <template>...</template>
    // 1: (empty)
    // 2: <script setup lang="ts">
    // 3: import { ref } from 'vue'
    // 4: function doThing() {
    // 5:   return 1
    // 6: }
    // 7: </script>
    let graph = parse(src);
    let node = find_node(&graph, "doThing");
    // span.0 is start_row (0-indexed); doThing starts at line 4 of the .vue file.
    assert_eq!(
        node.span.0, 4,
        "doThing start row should be 4 (vue file line 4, 0-indexed); got {}",
        node.span.0
    );
}

// ── Test 4: Import resolution ────────────────────────────────────────────────

#[test]
fn imports_from_script_are_captured() {
    let src = r#"<template><div /></template>

<script setup lang="ts">
import { ref, computed } from 'vue'
import type { Ref } from 'vue'
import axios from 'axios'
</script>
"#;
    let graph = parse(src);
    let srcs = import_sources(&graph);

    assert!(
        srcs.contains(&"vue"),
        "expected import from 'vue'; got {srcs:?}"
    );
    assert!(
        srcs.contains(&"axios"),
        "expected import from 'axios'; got {srcs:?}"
    );

    let imported_names: Vec<&str> = graph
        .imports
        .iter()
        .map(|i| i.imported_name.as_str())
        .collect();
    assert!(
        imported_names.contains(&"ref"),
        "expected 'ref' in imports; got {imported_names:?}"
    );
    assert!(
        imported_names.contains(&"computed"),
        "expected 'computed' in imports; got {imported_names:?}"
    );
}

// ── Test 5: Multi-script (regular + setup) — Vue 3 idiom ─────────────────────
//
// A SFC can have both `<script>` (for type-only / Options API helpers) and
// `<script setup>`. Both must be parsed; symbols from both blocks appear in
// the graph.

#[test]
fn multi_script_both_blocks_parsed() {
    let src = r#"<template><div /></template>

<script lang="ts">
export default {
  name: 'MyComp',
}
</script>

<script setup lang="ts">
import { ref } from 'vue'
function helperFn() {}
</script>
"#;
    let graph = parse(src);

    // Section nodes: one for <script> and one for <script setup>.
    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"script"),
        "expected 'script' Section; got {sections:?}"
    );
    assert!(
        sections.contains(&"script setup"),
        "expected 'script setup' Section; got {sections:?}"
    );

    // helperFn from <script setup>.
    let fns = node_names_by_kind(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"helperFn"),
        "expected Function 'helperFn' from script setup; got {fns:?}"
    );

    // Import from <script setup>.
    let srcs = import_sources(&graph);
    assert!(
        srcs.contains(&"vue"),
        "expected import from 'vue' in multi-script SFC; got {srcs:?}"
    );
}

// ── Test 6: Plain JS script (lang="js" explicit) ─────────────────────────────

#[test]
fn script_lang_js_parses_javascript() {
    let src = r#"<template><div /></template>

<script lang="js">
import { createApp } from 'vue'
function setup() {
  return {}
}
export default { setup }
</script>
"#;
    let graph = parse(src);

    let fns = node_names_by_kind(&graph, NodeKind::Function);
    assert!(
        fns.contains(&"setup"),
        "expected Function 'setup' from JS script; got {fns:?}"
    );

    let srcs = import_sources(&graph);
    assert!(
        srcs.contains(&"vue"),
        "expected import from 'vue' in JS SFC; got {srcs:?}"
    );
}

// ── Test 7: Empty SFC (no content in blocks) — no panic ──────────────────────

#[test]
fn empty_script_block_no_panic() {
    let src = "<template></template>\n<script setup lang=\"ts\"></script>\n";
    let graph = parse(src);

    // Should have Section nodes but no function/import nodes.
    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"script setup"),
        "expected script setup Section in empty SFC; got {sections:?}"
    );
    assert!(
        graph.imports.is_empty(),
        "expected no imports in empty script block"
    );
}
