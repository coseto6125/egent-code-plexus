use super::provider::LanguageProvider;
use super::types::LocalGraph;
use crossbeam_channel::bounded;
use rayon::prelude::*;
use std::path::PathBuf;

#[derive(Default)]
pub struct AnalyzerPipeline {
    providers: Vec<Box<dyn LanguageProvider>>,
}

impl AnalyzerPipeline {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn register_provider(&mut self, provider: Box<dyn LanguageProvider>) {
        self.providers.push(provider);
    }

    /// Parse a single file from in-memory bytes without touching disk.
    /// The `rel_path` is used only for provider selection (extension lookup)
    /// and is stored verbatim in the returned `LocalGraph.file_path`.
    pub fn parse_file_raw(
        &self,
        rel_path: &std::path::Path,
        source: &[u8],
    ) -> anyhow::Result<LocalGraph> {
        let provider = self
            .find_provider(rel_path)
            .ok_or_else(|| anyhow::anyhow!("no provider for {:?}", rel_path))?;
        provider.parse_file(rel_path, source)
    }

    fn find_provider(&self, path: &std::path::Path) -> Option<&dyn LanguageProvider> {
        eprintln!("[PIPELINE DEBUG] find_provider for {:?}", path);
        // Check basename for extension-less files like `Dockerfile` before extension lookup.
        let file_name = path.file_name()?.to_str()?;
        if matches!(file_name, "Dockerfile" | "dockerfile") {
            return self
                .providers
                .iter()
                .find(|p| p.name() == "dockerfile")
                .map(|p| p.as_ref());
        }

        // GitHub Actions: path-based routing — .github/workflows/*.yml|yaml
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            eprintln!("[PIPELINE DEBUG] ext={:?}", ext);
            if matches!(ext, "yml" | "yaml") {
                let components: Vec<_> = path.components().collect();
                eprintln!("[PIPELINE DEBUG] components={:?}", components);
                let is_gha = components.windows(2).any(|w| {
                    let a = w[0].as_os_str();
                    let b = w[1].as_os_str();
                    eprintln!("[PIPELINE DEBUG] window: {:?}/{:?}", a, b);
                    a == ".github" && b == "workflows"
                });
                eprintln!("[PIPELINE DEBUG] is_gha={}", is_gha);
                if is_gha {
                    let found = self.providers.iter().find(|p| p.name() == "github-actions");
                    eprintln!("[PIPELINE DEBUG] provider found={}", found.is_some());
                    return found.map(|p| p.as_ref());
                }
            }
        }

        let ext = path.extension()?.to_str()?;
        match ext {
            "ts" | "tsx" => self
                .providers
                .iter()
                .find(|p| p.name() == "typescript")
                .map(|p| p.as_ref()),
            "py" | "pyi" => self
                .providers
                .iter()
                .find(|p| p.name() == "python")
                .map(|p| p.as_ref()),
            "go" => self
                .providers
                .iter()
                .find(|p| p.name() == "go")
                .map(|p| p.as_ref()),
            "rs" => self
                .providers
                .iter()
                .find(|p| p.name() == "rust")
                .map(|p| p.as_ref()),
            "java" => self
                .providers
                .iter()
                .find(|p| p.name() == "java")
                .map(|p| p.as_ref()),
            "js" | "jsx" | "mjs" | "cjs" => self
                .providers
                .iter()
                .find(|p| p.name() == "javascript")
                .map(|p| p.as_ref()),
            "php" => self
                .providers
                .iter()
                .find(|p| p.name() == "php")
                .map(|p| p.as_ref()),
            "rb" => self
                .providers
                .iter()
                .find(|p| p.name() == "ruby")
                .map(|p| p.as_ref()),
            "kt" | "kts" => self
                .providers
                .iter()
                .find(|p| p.name() == "kotlin")
                .map(|p| p.as_ref()),
            "cs" => self
                .providers
                .iter()
                .find(|p| p.name() == "c_sharp")
                .map(|p| p.as_ref()),
            "c" | "h" => self
                .providers
                .iter()
                .find(|p| p.name() == "c")
                .map(|p| p.as_ref()),
            "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" => self
                .providers
                .iter()
                .find(|p| p.name() == "cpp")
                .map(|p| p.as_ref()),
            "swift" => self
                .providers
                .iter()
                .find(|p| p.name() == "swift")
                .map(|p| p.as_ref()),
            "dart" => self
                .providers
                .iter()
                .find(|p| p.name() == "dart")
                .map(|p| p.as_ref()),
            "sh" | "bash" => self
                .providers
                .iter()
                .find(|p| p.name() == "bash")
                .map(|p| p.as_ref()),
            "lua" | "luau" => self
                .providers
                .iter()
                .find(|p| p.name() == "lua")
                .map(|p| p.as_ref()),
            "dockerfile" => self
                .providers
                .iter()
                .find(|p| p.name() == "dockerfile")
                .map(|p| p.as_ref()),
            "cr" => self
                .providers
                .iter()
                .find(|p| p.name() == "crystal")
                .map(|p| p.as_ref()),
            "move" => self
                .providers
                .iter()
                .find(|p| p.name() == "move")
                .map(|p| p.as_ref()),
            "sol" => self
                .providers
                .iter()
                .find(|p| p.name() == "solidity")
                .map(|p| p.as_ref()),
            "tf" | "tfvars" | "hcl" => self
                .providers
                .iter()
                .find(|p| p.name() == "hcl")
                .map(|p| p.as_ref()),
            "nim" => self
                .providers
                .iter()
                .find(|p| p.name() == "nim")
                .map(|p| p.as_ref()),
            "sql" => self
                .providers
                .iter()
                .find(|p| p.name() == "sql")
                .map(|p| p.as_ref()),
            "vy" => self
                .providers
                .iter()
                .find(|p| p.name() == "vyper")
                .map(|p| p.as_ref()),
            "cairo" => self
                .providers
                .iter()
                .find(|p| p.name() == "cairo")
                .map(|p| p.as_ref()),
            "v" | "sv" | "vh" | "svh" => self
                .providers
                .iter()
                .find(|p| p.name() == "verilog")
                .map(|p| p.as_ref()),
            _ => None,
        }
    }

    /// Analyze files concurrently using a Multi-Producer Single-Consumer architecture
    pub fn analyze(&self, files: Vec<(PathBuf, PathBuf)>) -> Vec<LocalGraph> {
        let (tx, rx) = crossbeam_channel::unbounded::<LocalGraph>();

        // Producer (A): parse files concurrently
        rayon::scope(|s| {
            s.spawn(|_| {
                files.into_par_iter().for_each_with(tx, |sender, (abs_path, rel_path)| {
                    if let Some(provider) = self.find_provider(&rel_path) {
                        if let Ok(source) = std::fs::read(&abs_path) {
                            if let Ok(mut local_graph) = provider.parse_file(&rel_path, &source) {
                                use sha2::{Sha256, Digest};
                                let mut hasher = Sha256::new();
                                hasher.update(&source);
                                local_graph.content_hash = hasher.finalize().into();
                                let _ = sender.send(local_graph);
                            }
                        }
                    }
                });
            });
        });

        // Consumer (B): collect and build (Builder thread)
        let mut all_graphs = Vec::new();
        while let Ok(graph) = rx.recv() {
            all_graphs.push(graph);
        }

        all_graphs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::typescript::TypeScriptProvider;

    #[test]
    fn test_pipeline_execution() {
        // Pipeline calls std::fs::read on each abs_path, so the test must
        // materialise real files. Use a tempdir + emit empty `.ts`/`.txt`.
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.ts");
        let b = tmp.path().join("b.ts");
        let c = tmp.path().join("c.txt");
        std::fs::write(&a, b"").unwrap();
        std::fs::write(&b, b"").unwrap();
        std::fs::write(&c, b"").unwrap();

        let mut pipeline = AnalyzerPipeline::new();
        pipeline.register_provider(Box::new(TypeScriptProvider));

        let files = vec![
            (a.clone(), PathBuf::from("a.ts")),
            (b.clone(), PathBuf::from("b.ts")),
            (c.clone(), PathBuf::from("c.txt")), // Should be ignored based on extension
        ];

        let results = pipeline.analyze(files);

        // We expect only 2 .ts files to be processed
        assert_eq!(results.len(), 2);

        let paths: Vec<_> = results
            .iter()
            .map(|g| g.file_path.to_str().unwrap())
            .collect();
        assert!(paths.contains(&"a.ts"));
        assert!(paths.contains(&"b.ts"));
    }
}
