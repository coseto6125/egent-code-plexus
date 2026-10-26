//! `Overrides` in TypeScript binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! The parser captures no `override` modifier, so the pair is a name match in the declared base.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, graph_of, overrides, Evidence};

#[test]
fn test_overrides_typescript_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        ("a/decoy.ts", "export class Base {\n  run(): void {}\n}\n"),
        ("b/child.ts", "export class Base {\n  run(): void {}\n}\nexport class Child extends Base {\n  run(): void {}\n}\n"),
        "run",
        Evidence::Heuristic,
    );
}

#[test]
fn test_overrides_typescript_base_named_twice_emits_one_edge() {
    // `extends Base implements Base` reaches the one `Base.run` twice.
    let graph = graph_of(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[(
            "child.ts",
            "export class Base {\n  run(): void {}\n}\nexport class Child extends Base implements Base {\n  run(): void {}\n}\n",
        )],
    );
    let edges = overrides(&graph);
    assert_eq!(edges.len(), 1, "one edge, not one per mention: {edges:?}");
    assert_eq!((edges[0].1.name.as_str(), edges[0].1.line), ("run", 1));
}

/// Stands for: an unmarked method binds the nearest declaration two levels
/// up, at the name-match confidence, never `Exact`.
#[test]
fn test_overrides_typescript_grandparent_emits_heuristic() {
    let graph = graph_of(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[(
            "c.ts",
            "export class A {\n  run(): void {}\n}\nexport class B extends A {}\nexport class C extends B {\n  run(): void {}\n}\n",
        )],
    );
    let edges = overrides(&graph);
    assert_eq!(edges.len(), 1, "C.run -> A.run; got {edges:?}");
    assert_eq!((edges[0].0.line, edges[0].1.line), (5, 1), "{edges:?}");
    overrides_support::assert_evidence(&edges[0], Evidence::Heuristic);
    assert_eq!(
        edges[0].2,
        ecp_analyzer::post_process::overrides::NAME_MATCH_CONFIDENCE
    );
}

/// Stands for: TypeScript has no overloading and its compiler checks the
/// override, so a parameter count that differs (with no rest parameter)
/// gets no edge, and the reader gets an `override-ambiguous` BlindSpot that
/// names both counts (codex ruling 2).
#[test]
fn test_overrides_typescript_arity_mismatch_emits_no_edge() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_AMBIGUOUS;
    let graph = graph_of(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[(
            "b.ts",
            "export class A {\n  run(x: number): void {}\n}\nexport class B extends A {\n  run(): void {}\n}\n",
        )],
    );
    let edges = overrides(&graph);
    assert!(edges.is_empty(), "{edges:?}");
    let spots = overrides_support::override_blind_spots(&graph);
    assert_eq!(spots.len(), 1, "{spots:?}");
    let (kind, file, row, hint) = &spots[0];
    assert_eq!(
        (kind.as_str(), file.as_str(), *row),
        (BS_KIND_OVERRIDE_AMBIGUOUS, "b.ts", 4),
        "{spots:?}"
    );
    assert!(
        hint.contains("`A` takes 1 parameters") && hint.contains("takes 0"),
        "{hint}"
    );
}

const VUE_CLASSES: &str =
    "export class Base {\n  save(): void {}\n}\nexport class Child extends Base {\n  save(): void {}\n}\n";

/// A Vue `<script lang="ts">` file is TypeScript by its stored language, not
/// by its `.vue` path: the pass reads the Inferred rules for it, as for the
/// same classes in a `.ts` file. Red on HEAD: the path named no language, so
/// the pass read the marked-only rule and emitted no edge.
#[test]
fn test_overrides_vue_ts_script_emits_edge_like_ts_file() {
    let vue = format!("<script lang=\"ts\">\n{VUE_CLASSES}</script>\n");
    let graph = graph_of(
        &ecp_analyzer::vue::parser::VueProvider::new().unwrap(),
        &[("Comp.vue", &vue)],
    );
    let edges = overrides(&graph);
    assert_eq!(edges.len(), 1, "one Overrides edge: {edges:?}");
    let (child, base, _, _) = &edges[0];
    assert_eq!(
        (child.owner.as_str(), child.name.as_str()),
        ("Child", "save")
    );
    assert_eq!((base.owner.as_str(), base.name.as_str()), ("Base", "save"));
    assert_eq!(base.file, "Comp.vue");
}

/// A `lang="js"` script is JavaScript: its rules are Inferred too.
#[test]
fn test_overrides_vue_js_script_emits_edge() {
    let vue = format!("<script>\n{}</script>\n", VUE_CLASSES.replace(": void", ""));
    let graph = graph_of(
        &ecp_analyzer::vue::parser::VueProvider::new().unwrap(),
        &[("Comp.vue", &vue)],
    );
    assert_eq!(overrides(&graph).len(), 1, "{:?}", overrides(&graph));
}

/// A Vue file with no script stores no language: no rules, no edge, no panic.
#[test]
fn test_overrides_vue_without_script_emits_nothing() {
    let graph = graph_of(
        &ecp_analyzer::vue::parser::VueProvider::new().unwrap(),
        &[("Comp.vue", "<template><p>hi</p></template>\n")],
    );
    assert!(overrides(&graph).is_empty());
}
