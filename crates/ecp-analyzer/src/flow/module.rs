use super::engine::Engine;
use super::{Boundary, Direction, FlowNode, FlowReport};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn normalize(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}
impl Engine<'_> {
    pub(super) fn import_target(&self, n: usize) -> Option<(usize, usize)> {
        let source = self
            .field(n, "source")
            .or_else(|| self.field(n, "module_name"))?;
        let file = self.ast[n].file;
        let module = self.ast[source].text.trim_matches(['\'', '"']);
        let parent = self.files[file]
            .path
            .rsplit_once('/')
            .map(|(p, _)| p)
            .unwrap_or("");
        let module = if self.files[file].path.ends_with(".py") {
            module.replace('.', "/")
        } else {
            module.into()
        };
        let path = normalize(&format!("{parent}/{module}"));
        let target = self.files.iter().position(|f| {
            let p = normalize(&f.path);
            p == path
                || ["js", "mjs", "ts", "tsx", "py", "php"].iter().any(|ext| {
                    p == format!("{path}.{ext}")
                        || p == format!("{path}/index.{ext}")
                        || p == format!("{path}/__init__.{ext}")
                })
        })?;
        Some((source, target))
    }
    pub(super) fn module(
        &mut self,
        file: usize,
        visiting: &mut BTreeSet<usize>,
        done: &mut BTreeSet<usize>,
    ) {
        if done.contains(&file) {
            return;
        }
        let Some(root) = self.roots[file] else {
            done.insert(file);
            return;
        };
        if !visiting.insert(file) {
            self.boundary(
                root,
                "import_cycle",
                "Cyclic module initialization has unresolved execution order.",
            );
            return;
        }
        let dependencies: Vec<_> = self
            .ast
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                a.file == file
                    && matches!(
                        a.kind.as_str(),
                        "import_statement" | "import_from_statement" | "export_statement"
                    )
            })
            .filter_map(|(n, _)| self.import_target(n).map(|(_, f)| f))
            .collect();
        for dependency in dependencies {
            self.links.insert((file, dependency));
            self.module(dependency, visiting, done);
        }
        self.imports(file);
        self.eval(root, &[file]);
        visiting.remove(&file);
        done.insert(file);
    }
    pub(super) fn imports(&mut self, source_file: usize) {
        let imports: Vec<usize> = self
            .ast
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                matches!(
                    a.kind.as_str(),
                    "import_statement" | "import_from_statement" | "export_statement"
                ) && a.file == source_file
            })
            .map(|(i, _)| i)
            .collect();
        for n in imports {
            let file = self.ast[n].file;
            if self.ast[n].kind == "export_statement" && self.field(n, "source").is_none() {
                continue;
            }
            let Some((source, target)) = self.import_target(n) else {
                self.boundary(n, "import", "Only static named imports are resolved.");
                continue;
            };
            let mut stack = self.ast[n].children.clone();
            while let Some(c) = stack.pop() {
                let a = self.ast[c].clone();
                if matches!(
                    a.kind.as_str(),
                    "import_specifier" | "aliased_import" | "export_specifier"
                ) {
                    let name = self
                        .field(c, "name")
                        .or_else(|| a.children.first().copied());
                    if let Some(name) = name {
                        let alias = self.field(c, "alias").unwrap_or(name);
                        let value = self.scopes[target].get(&self.ast[name].text).cloned();
                        if let Some(v) = value {
                            self.scopes[file].insert(self.ast[alias].text.clone(), v);
                        } else {
                            self.boundary(c, "import", "The imported binding is unresolved.");
                        }
                    }
                } else if self.files[file].path.ends_with(".py")
                    && a.kind == "dotted_name"
                    && c != source
                {
                    if let Some(v) = self.scopes[target].get(&a.text).cloned() {
                        self.scopes[file].insert(a.text, v);
                    }
                } else {
                    stack.extend(a.children);
                }
            }
        }
    }
    pub(super) fn report(self, anchors: Vec<usize>, direction: Direction) -> FlowReport {
        let mut selected: BTreeSet<usize> = anchors.iter().copied().collect();
        let mut adjacency: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for e in &self.edges {
            let (from, to) = if direction == Direction::Forward {
                (e.from, e.to)
            } else {
                (e.to, e.from)
            };
            adjacency.entry(from).or_default().push(to);
        }
        let mut pending = anchors.clone();
        while let Some(id) = pending.pop() {
            if let Some(next) = adjacency.get(&id) {
                for id in next {
                    if selected.insert(*id) {
                        pending.push(*id);
                    }
                }
            }
        }
        let nodes: Vec<FlowNode> = self
            .nodes
            .into_iter()
            .filter(|n| selected.contains(&n.id))
            .collect();
        // An empty slice keeps every boundary: nothing else explains why it is empty.
        let index: BTreeMap<&str, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.as_str(), i))
            .collect();
        let reached: Option<BTreeSet<usize>> = (!nodes.is_empty()).then(|| {
            let mut files: BTreeSet<usize> = nodes
                .iter()
                .filter_map(|n| index.get(n.file.as_str()).copied())
                .collect();
            // Direct module neighbours too: the unresolved import or spread that
            // stops a slice at a file edge lives on the other side of that edge.
            let neighbours: Vec<usize> = self
                .links
                .iter()
                .filter_map(|(importer, dependency)| {
                    if files.contains(importer) {
                        Some(*dependency)
                    } else if files.contains(dependency) {
                        Some(*importer)
                    } else {
                        None
                    }
                })
                .collect();
            files.extend(neighbours);
            files
        });
        let total = self.boundaries.len();
        let boundaries: Vec<Boundary> = self
            .boundaries
            .into_iter()
            .filter(|b| {
                reached
                    .as_ref()
                    .is_none_or(|r| index.get(b.file.as_str()).is_some_and(|i| r.contains(i)))
            })
            .collect();
        let boundaries_omitted = total - boundaries.len();
        let consumers = nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.kind.as_str(),
                    "argument" | "return" | "field_write" | "condition"
                )
            })
            .map(|n| n.id)
            .collect();
        FlowReport {
            source_hashes: self.files.iter().map(|file| {
                let hash = ecp_core::uid::xxh3_64_bytes(file.source.as_bytes());
                (file.path.clone(), format!("xxh3:{hash:016x}"))
            }).collect(),
            anchor: anchors,
            nodes,
            edges: self.edges.into_iter().filter(|edge| {
                selected.contains(&edge.from) && selected.contains(&edge.to)
            }).collect(),
            consumers,
            boundaries,
            boundaries_omitted,
            truncated: self.truncated,
            coverage: "Bounded, path-insensitive source analysis with call-site expansion. Boundaries cover the files the selected slice reaches; boundaries_omitted counts the rest. Empty slices do not prove absence beyond supported semantics.".into(),
        }
    }
}
