//! Kubernetes manifest detector tests.
//!
//! All tests call `KubernetesProvider::parse_file` directly and inspect the
//! emitted `nodes` (resource → Class) and `imports` (container image →
//! RawImport — the load-bearing edge linking a manifest to its Dockerfile /
//! registry artifact).
//!
//! Scope: this is a config/IaC detector reusing `tree-sitter-yaml`, NOT a
//! change to the 14-mainstream-language parsers, so the "14-language coverage"
//! rule does not apply — the analysis target here is a single schema (k8s
//! manifests), the way `docker_compose` and `openapi` are single-schema.

use ecp_analyzer::kubernetes::parser::{has_k8s_markers, KubernetesProvider};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;

fn parse(src: &str) -> LocalGraph {
    let provider = KubernetesProvider::new().expect("provider init");
    provider
        .parse_file("manifest.yaml".as_ref(), src.as_bytes())
        .expect("parse_file")
}

fn images(g: &LocalGraph) -> Vec<&RawImport> {
    g.imports.iter().collect()
}

fn classes(g: &LocalGraph) -> Vec<&RawNode> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Class)
        .collect()
}

// ── Positive: Deployment with container → image RawImport edge ────────────────

#[test]
fn deployment_emits_class_and_image_import() {
    let src = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: web-app
spec:
  replicas: 3
  template:
    spec:
      containers:
        - name: web
          image: ghcr.io/acme/web:1.4.2
"#;
    let g = parse(src);

    let cls = classes(&g);
    assert_eq!(cls.len(), 1, "expected one resource Class; got {cls:?}");
    assert_eq!(cls[0].name, "web-app");
    assert_eq!(
        cls[0].type_annotation.as_deref(),
        Some("Deployment"),
        "kind must land in type_annotation for LLM disambiguation"
    );

    let imgs = images(&g);
    assert_eq!(imgs.len(), 1, "expected one image import; got {imgs:?}");
    assert_eq!(imgs[0].imported_name, "ghcr.io/acme/web:1.4.2");
    assert_eq!(imgs[0].source, "ghcr.io/acme/web:1.4.2");
    assert_eq!(
        imgs[0].alias.as_deref(),
        Some("web-app"),
        "image import must be aliased to its owning resource"
    );
}

// ── Positive: bare Pod (containers directly under spec) ───────────────────────

#[test]
fn pod_containers_directly_under_spec() {
    let src = r#"apiVersion: v1
kind: Pod
metadata:
  name: debug-pod
spec:
  containers:
    - name: shell
      image: busybox:latest
"#;
    let g = parse(src);
    assert_eq!(classes(&g).len(), 1);
    let imgs = images(&g);
    assert_eq!(imgs.len(), 1);
    assert_eq!(imgs[0].imported_name, "busybox:latest");
}

// ── Positive: CronJob (deeply nested jobTemplate → containers) ────────────────

#[test]
fn cronjob_deeply_nested_containers_reached() {
    let src = r#"apiVersion: batch/v1
kind: CronJob
metadata:
  name: nightly
spec:
  jobTemplate:
    spec:
      template:
        spec:
          containers:
            - name: job
              image: registry.local/nightly:v2
"#;
    let g = parse(src);
    let imgs = images(&g);
    assert_eq!(
        imgs.len(),
        1,
        "deeply-nested container image must still be found; got {imgs:?}"
    );
    assert_eq!(imgs[0].imported_name, "registry.local/nightly:v2");
}

// ── Positive: initContainers also captured ────────────────────────────────────

#[test]
fn init_containers_image_captured() {
    let src = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: migrator
spec:
  template:
    spec:
      initContainers:
        - name: migrate
          image: acme/migrate:3
      containers:
        - name: app
          image: acme/app:3
"#;
    let g = parse(src);
    let names: Vec<&str> = g.imports.iter().map(|i| i.imported_name.as_str()).collect();
    assert!(
        names.contains(&"acme/migrate:3"),
        "init container image missing: {names:?}"
    );
    assert!(
        names.contains(&"acme/app:3"),
        "app container image missing: {names:?}"
    );
    assert_eq!(names.len(), 2);
}

// ── Positive: multi-document file (Service + Deployment bundled) ──────────────

#[test]
fn multi_document_service_and_deployment() {
    let src = r#"apiVersion: v1
kind: Service
metadata:
  name: web-svc
spec:
  selector:
    app: web
  ports:
    - port: 80
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web-deploy
spec:
  template:
    spec:
      containers:
        - name: web
          image: acme/web:5
"#;
    let g = parse(src);

    let cls = classes(&g);
    let names: Vec<&str> = cls.iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.contains(&"web-svc"),
        "Service resource missing: {names:?}"
    );
    assert!(
        names.contains(&"web-deploy"),
        "Deployment resource missing: {names:?}"
    );
    assert_eq!(cls.len(), 2, "expected both resources; got {names:?}");

    let kinds: Vec<&str> = cls
        .iter()
        .filter_map(|n| n.type_annotation.as_deref())
        .collect();
    assert!(kinds.contains(&"Service"));
    assert!(kinds.contains(&"Deployment"));

    // Only the Deployment carries an image; the Service has none.
    let imgs = images(&g);
    assert_eq!(imgs.len(), 1, "only Deployment has an image; got {imgs:?}");
    assert_eq!(imgs[0].imported_name, "acme/web:5");
    assert_eq!(imgs[0].alias.as_deref(), Some("web-deploy"));
}

// ── Negative: plain non-k8s YAML must NOT be classified as k8s ────────────────

#[test]
fn plain_app_config_yaml_emits_nothing() {
    // An ordinary application config — no apiVersion/kind. The sniff must not
    // false-positive.
    let src = r#"server:
  host: 0.0.0.0
  port: 8080
database:
  url: postgres://localhost/app
  pool: 10
logging:
  level: info
"#;
    let g = parse(src);
    assert!(
        g.nodes.is_empty(),
        "non-k8s yaml must emit no nodes; got {:?}",
        g.nodes
    );
    assert!(
        g.imports.is_empty(),
        "non-k8s yaml must emit no imports; got {:?}",
        g.imports
    );
}

#[test]
fn ci_config_with_image_key_not_misclassified() {
    // A CI config (GitLab-style) mentions `image:` but has no top-level
    // apiVersion+kind — must not be treated as a k8s manifest.
    let src = r#"stages:
  - build
  - test
build-job:
  stage: build
  image: node:20
  script:
    - npm ci
    - npm run build
"#;
    let g = parse(src);
    assert!(
        g.nodes.is_empty() && g.imports.is_empty(),
        "CI config with an image: key must not be classified as k8s"
    );
}

// ── Content-sniff correctness ─────────────────────────────────────────────────

#[test]
fn sniff_requires_both_apiversion_and_kind() {
    assert!(has_k8s_markers(b"apiVersion: v1\nkind: Pod\n"));
    assert!(
        has_k8s_markers(b"kind: Service\napiVersion: v1\n"),
        "order-independent"
    );
    assert!(!has_k8s_markers(b"apiVersion: v1\n"), "kind alone missing");
    assert!(!has_k8s_markers(b"kind: Pod\n"), "apiVersion alone missing");
    assert!(!has_k8s_markers(b"server:\n  port: 8080\n"));
}

#[test]
fn sniff_rejects_indented_keys() {
    // `kind:` / `apiVersion:` nested (column > 0) must not satisfy the gate —
    // arbitrary YAML can carry a nested `kind:` field.
    let probe = b"metadata:\n  apiVersion: x\n  kind: y\n";
    assert!(
        !has_k8s_markers(probe),
        "indented apiVersion/kind must not pass the top-level gate"
    );
}

#[test]
fn sniff_allows_keys_after_document_separator() {
    // Multi-doc files commonly lead with `---`; the first real resource's keys
    // are still column-0 after the separator newline.
    let src = b"---\napiVersion: v1\nkind: Pod\n";
    assert!(has_k8s_markers(src));
}

// ── Resource without containers (e.g. ConfigMap) still emits its Class ────────

#[test]
fn resource_without_containers_emits_class_no_imports() {
    let src = r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: app-config
data:
  LOG_LEVEL: info
"#;
    let g = parse(src);
    let cls = classes(&g);
    assert_eq!(cls.len(), 1);
    assert_eq!(cls[0].name, "app-config");
    assert_eq!(cls[0].type_annotation.as_deref(), Some("ConfigMap"));
    assert!(
        g.imports.is_empty(),
        "ConfigMap has no image; got {:?}",
        g.imports
    );
}
