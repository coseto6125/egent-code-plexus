use anyhow::Result;
use fastembed::{InitOptionsUserDefined, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel};
use hf_hub::api::sync::ApiBuilder;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Symbol names rarely exceed ~30 tokens. 128 covers enriched text
/// (name + decorators + signature) with safety margin while cutting
/// per-sequence inference cost ~2-3x vs the fastembed default.
const EMBED_MAX_LENGTH: usize = 128;

/// Model output dimensionality (BGE-M3 native dim). Used to
/// build zero-vec placeholders for callers that opt out of embedding
/// specific positions. Must match the active EmbeddingModel below.
const EMBED_DIM_FALLBACK: usize = 1024;

/// Resolve the model cache directory using HuggingFace conventions so that
/// gnx shares the download with transformers / sentence-transformers /
/// any other HF-aware tool on the same machine. Precedence:
///   1. `GNX_MODEL_CACHE` (explicit override)
///   2. `HF_HUB_CACHE` (HuggingFace recommended)
///   3. `HF_HOME/hub` (legacy HF convention)
///   4. `$XDG_CACHE_HOME/huggingface/hub` (XDG fallback)
///   5. `$HOME/.cache/huggingface/hub` (final fallback)
fn resolve_cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("GNX_MODEL_CACHE") {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("HF_HUB_CACHE") {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("HF_HOME") {
        return PathBuf::from(p).join("hub");
    }
    let cache_root = std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string())).join(".cache")
        });
    cache_root.join("huggingface").join("hub")
}

pub struct Embedder {
    model: Mutex<TextEmbedding>,
}

impl Embedder {
    pub fn new() -> Result<Self> {
        let cache_dir = resolve_cache_dir();
        eprintln!("🧠 [gnx-rs] Initializing BGE-M3 INT8 Quantized Embedding Model...");
        eprintln!("   Cache dir: {}", cache_dir.display());
        eprintln!("   (If the model is not present, it will download ~1.2GB of weights from HuggingFace.)");

        let api = ApiBuilder::new()
            .with_cache_dir(cache_dir.clone())
            .with_progress(true)
            .build()?;

        let model_repo = api.repo(hf_hub::Repo::new(
            "MahradHosseini/bge-m3-onnx-int8".to_string(),
            hf_hub::RepoType::Model,
        ));
        let tok_repo = api.repo(hf_hub::Repo::new(
            "BAAI/bge-m3".to_string(),
            hf_hub::RepoType::Model,
        ));

        let model_file = model_repo.get("model_quantized.onnx")?;
        let tokenizer_file = tok_repo.get("tokenizer.json")?;
        let config_file = tok_repo.get("config.json")?;
        let special_tokens_map_file = tok_repo.get("special_tokens_map.json")?;
        let tokenizer_config_file = tok_repo.get("tokenizer_config.json")?;

        let model_def = UserDefinedEmbeddingModel::new(
            std::fs::read(model_file)?,
            TokenizerFiles {
                tokenizer_file: std::fs::read(tokenizer_file)?,
                config_file: std::fs::read(config_file)?,
                special_tokens_map_file: std::fs::read(special_tokens_map_file)?,
                tokenizer_config_file: std::fs::read(tokenizer_config_file)?,
            },
        );

        let model = TextEmbedding::try_new_from_user_defined(
            model_def,
            InitOptionsUserDefined::new().with_max_length(EMBED_MAX_LENGTH),
        )?;

        Ok(Self {
            model: Mutex::new(model),
        })
    }

    /// Deduplicates input strings before invoking the model, then broadcasts
    /// the unique embeddings back to the original positions.
    ///
    /// # Skip semantics
    ///
    /// Input strings of length 0 are treated as **skip markers** and emit a
    /// zero vector at that position. Cosine similarity to anything = 0, so
    /// skipped nodes are naturally excluded from query result rankings while
    /// preserving the `embeddings[i] ↔ nodes[i]` index alignment expected by
    /// the rest of the pipeline.
    ///
    /// Callers opting out of embedding a specific node MUST push
    /// `String::new()` — not `" "`, `"<skip>"`, or any other sentinel — so
    /// that the dedup dispatcher recognises and short-circuits it before
    /// reaching the model.
    pub fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Map keys borrow from `texts` (function parameter) for the entire body,
        // so we get hash lookups with no per-key allocation on cache hits.
        let mut text_to_unique_idx: HashMap<&str, usize> = HashMap::new();
        let mut unique_texts: Vec<String> = Vec::new();
        let mut position_map: Vec<Option<usize>> = Vec::with_capacity(texts.len());

        for t in &texts {
            if t.is_empty() {
                position_map.push(None);
                continue;
            }
            let idx = match text_to_unique_idx.get(t.as_str()) {
                Some(&i) => i,
                None => {
                    let next = unique_texts.len();
                    unique_texts.push(t.clone());
                    text_to_unique_idx.insert(t.as_str(), next);
                    next
                }
            };
            position_map.push(Some(idx));
        }

        let unique_embeddings = if unique_texts.is_empty() {
            Vec::new()
        } else {
            let mut model = self
                .model
                .lock()
                .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
            model.embed(unique_texts, None)?
        };

        let dim = unique_embeddings
            .first()
            .map(|v| v.len())
            .unwrap_or(EMBED_DIM_FALLBACK);
        let zero_vec: Vec<f32> = vec![0.0; dim];

        Ok(position_map
            .into_iter()
            .map(|opt| match opt {
                Some(i) => unique_embeddings[i].clone(),
                None => zero_vec.clone(),
            })
            .collect())
    }
}
