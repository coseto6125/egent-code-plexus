//! Astro SFC parser tests — `astro_sfc` module.
//!
//! Covers: basic frontmatter extraction, imports, const/variable declarations,
//! TS interface, line-number remapping, no-frontmatter file, style and client
//! script Section nodes, and template expression non-capture.

use ecp_analyzer::astro::parser::AstroProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    AstroProvider::new()
        .expect("AstroProvider::new")
        .parse_file(Path::new("Comp.astro"), src.as_bytes())
        .expect("parse_file")
}

// ── helpers ───────────────────────────────────────────────────────────────────

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

// ── Test 1: Basic frontmatter — imports + const → Import + Const nodes ────────

#[test]
fn astro_basic_frontmatter_imports_and_const() {
    let src = r#"---
import Layout from '../layouts/Base.astro';
import { fetchUsers } from '../api';
const greeting = 'Hello';
---
<Layout>
  <h1>{greeting}</h1>
</Layout>
"#;
    let graph = parse(src);

    // Imports are extracted.
    let sources = import_sources(&graph);
    assert!(
        sources.contains(&"../layouts/Base.astro"),
        "expected Layout import; got {sources:?}"
    );
    assert!(
        sources.contains(&"../api"),
        "expected fetchUsers import; got {sources:?}"
    );

    // Default import name.
    let layout_import = graph
        .imports
        .iter()
        .find(|i| i.source == "../layouts/Base.astro")
        .expect("Layout import");
    assert_eq!(layout_import.imported_name, "Layout");

    // Named import.
    let fetch_import = graph
        .imports
        .iter()
        .find(|i| i.imported_name == "fetchUsers")
        .expect("fetchUsers import");
    assert_eq!(fetch_import.source, "../api");

    // const greeting → Const node.
    let consts = node_names_by_kind(&graph, NodeKind::Const);
    assert!(
        consts.contains(&"greeting"),
        "expected 'greeting' Const; got {consts:?}"
    );
}

// ── Test 2: await at top-level → Variable node ────────────────────────────────

#[test]
fn astro_await_toplevel_variable() {
    let src = r#"---
import { fetchUsers } from '../api';
const users = await fetchUsers();
---
<ul>{users.map(u => <li>{u.name}</li>)}</ul>
"#;
    let graph = parse(src);

    // `const users = await fetchUsers()` → Const node named `users`.
    let consts = node_names_by_kind(&graph, NodeKind::Const);
    assert!(
        consts.contains(&"users"),
        "expected 'users' Const from await expr; got {consts:?}"
    );
}

// ── Test 3: Line-number remapping ─────────────────────────────────────────────
//
// File layout:
//   line 0: ---
//   line 1: import Layout from '../layouts/Base.astro';
//   line 2: import { fetchUsers } from '../api';
//   line 3: <blank>
//   line 4: interface Props { greeting: string }
//   line 5: const { greeting } = Astro.props;
//   line 6: const users = await fetchUsers();
//   line 7: ---
//
// The `const users` declarator is at frontmatter line 6 (0-indexed),
// which should match row 6 in the .astro file.

#[test]
fn astro_line_number_remapping() {
    let src = "---\nimport Layout from '../layouts/Base.astro';\nimport { fetchUsers } from '../api';\n\ninterface Props { greeting: string }\nconst { greeting } = Astro.props;\nconst users = await fetchUsers();\n---\n<Layout />\n";
    let graph = parse(src);

    // `const users` is declared at .astro row 6 (0-based).
    let users_node = find_node(&graph, "users");
    assert_eq!(
        users_node.span.0, 6,
        "expected `users` at .astro row 6 (after frontmatter offset); got row {}",
        users_node.span.0
    );
}

// ── Test 4: No frontmatter → just File + template Section, no crash ───────────

#[test]
fn astro_no_frontmatter_no_crash() {
    let src = r#"<html>
  <body>
    <h1>Hello</h1>
  </body>
</html>
"#;
    let graph = parse(src);

    // Must not produce any imports or non-Section nodes.
    assert!(
        graph.imports.is_empty(),
        "expected no imports from template-only file; got {:?}",
        graph.imports
    );
    let non_section: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::Section)
        .collect();
    assert!(
        non_section.is_empty(),
        "expected no non-Section nodes from template-only file; got {non_section:#?}"
    );
    // Template Section must be present.
    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"template"),
        "expected 'template' Section; got {sections:?}"
    );
}

// ── Test 5: Style and client script → Section nodes, contents not parsed ──────

#[test]
fn astro_style_and_script_sections() {
    let src = r#"---
const x = 1;
---
<h1>Hello</h1>

<style>
  h1 { font-size: 2rem; }
</style>

<script>
  console.log('client side');
</script>
"#;
    let graph = parse(src);

    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"style"),
        "expected 'style' Section; got {sections:?}"
    );
    assert!(
        sections.contains(&"script"),
        "expected 'script' Section; got {sections:?}"
    );

    // Contents of style / script blocks must NOT produce non-Section nodes
    // beyond what the frontmatter emits.
    let all_non_section: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::Section)
        .collect();
    // Only `x` from frontmatter should appear as a non-Section node.
    for node in &all_non_section {
        assert_eq!(
            node.name, "x",
            "unexpected non-Section node from style/script: {:?}",
            node
        );
    }
}

// ── Test 6: Template expressions NOT parsed as JS functions ───────────────────

#[test]
fn astro_template_expressions_not_parsed() {
    let src = r#"---
import { fetchUsers } from '../api';
const users = await fetchUsers();
---
<ul>{users.map(u => <li>{u.name}</li>)}</ul>
"#;
    let graph = parse(src);

    // `u => <li>…</li>` in the template must NOT produce a Function node.
    let functions = node_names_by_kind(&graph, NodeKind::Function);
    assert!(
        functions.is_empty(),
        "template arrow fn must not produce Function nodes; got {functions:?}"
    );
}

// ── Test 7: TS interface in frontmatter → Interface node ─────────────────────

#[test]
fn astro_interface_in_frontmatter() {
    let src = r#"---
interface Props {
  greeting: string;
  count: number;
}
const { greeting, count } = Astro.props;
---
<h1>{greeting}</h1>
"#;
    let graph = parse(src);

    let ifaces = node_names_by_kind(&graph, NodeKind::Interface);
    assert!(
        ifaces.contains(&"Props"),
        "expected 'Props' Interface from frontmatter; got {ifaces:?}"
    );
}

// ── Test 8: Full example from task spec ──────────────────────────────────────

#[test]
fn astro_full_example_spec() {
    let src = r#"---
import Layout from '../layouts/Base.astro';
import { fetchUsers } from '../api';

interface Props { greeting: string }
const { greeting } = Astro.props;
const users = await fetchUsers();
---

<Layout title="Users">
  <h1>{greeting}</h1>
  <ul>{users.map(u => <li>{u.name}</li>)}</ul>
</Layout>

<style>
  h1 { font-size: 2rem; }
</style>
"#;
    let graph = parse(src);

    // Import nodes.
    let sources = import_sources(&graph);
    assert!(sources.contains(&"../layouts/Base.astro"));
    assert!(sources.contains(&"../api"));

    // Interface.
    let ifaces = node_names_by_kind(&graph, NodeKind::Interface);
    assert!(ifaces.contains(&"Props"), "expected Props interface");

    // Const nodes from frontmatter.
    // Note: `const { greeting } = Astro.props` uses destructuring — tree-sitter-typescript
    // only captures simple `name: (identifier)` patterns for const/variable, so `greeting`
    // does not appear as a Const node (consistent with Vue SFC parser behaviour).
    let consts = node_names_by_kind(&graph, NodeKind::Const);
    assert!(consts.contains(&"users"), "expected users const");

    // Section nodes.
    let sections = node_names_by_kind(&graph, NodeKind::Section);
    assert!(
        sections.contains(&"frontmatter"),
        "expected frontmatter Section"
    );
    assert!(sections.contains(&"template"), "expected template Section");
    assert!(sections.contains(&"style"), "expected style Section");

    // Template arrow fn NOT parsed.
    let functions = node_names_by_kind(&graph, NodeKind::Function);
    assert!(functions.is_empty(), "template arrows must not be parsed");
}
