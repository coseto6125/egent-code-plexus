use graph_nexus_core::graph::ZeroCopyGraph;
use std::fs;
use std::path::Path;
use tantivy::schema::*;
use tantivy::{collector::TopDocs, query::QueryParser, Index, IndexWriter, ReloadPolicy};

pub struct TantivyEngine;

/// Split a code identifier into subword tokens so a query like `config`
/// can match `parseConfig`, `configParser`, `parse_config_file`, etc.
/// Returns the original identifier followed by its subwords, space-
/// separated, so tantivy's default tokenizer indexes both forms — exact
/// matches keep boosted via the original token, and substring intent
/// hits via the subwords. Splits on:
///   - non-alphanumeric boundaries (`_`, `-`, `.`, `/`, ...)
///   - CamelCase transitions (`HTTPServer` → `HTTP Server`,
///     `parseHTML` → `parse HTML`, `parseConfig` → `parse Config`)
///   - letter↔digit boundaries (`utf8` → `utf 8`)
fn tokenize_identifier(name: &str) -> String {
    let mut tokens: Vec<String> = Vec::with_capacity(4);
    tokens.push(name.to_string());

    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();
    for i in 0..chars.len() {
        let c = chars[i];
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() {
            let prev = chars[i - 1];
            // lower→Upper (parseConfig → parse | Config)
            let camel_boundary = prev.is_lowercase() && c.is_uppercase();
            // Upper→Upper→lower (HTTPServer → HTTP | Server): split between
            // the trailing capital and the new word's leading capital.
            let acronym_boundary = prev.is_uppercase()
                && c.is_uppercase()
                && i + 1 < chars.len()
                && chars[i + 1].is_lowercase();
            // letter↔digit boundary (utf8 → utf | 8, h2 → h | 2)
            let digit_boundary = prev.is_alphabetic() != c.is_alphabetic()
                && (prev.is_ascii_digit() || c.is_ascii_digit());
            if camel_boundary || acronym_boundary || digit_boundary {
                tokens.push(std::mem::take(&mut current));
            }
        }
        current.push(c);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens.join(" ")
}

impl TantivyEngine {
    /// Build the Tantivy full-text index from the graph. Returns
    /// `Err` instead of panicking so the caller can degrade gracefully
    /// — `graph.bin` is the primary artifact; if BM25 fails to build
    /// (writer lock held by zombie, prior commit corrupt, FS full)
    /// exact-name resolution still works and the next `gnx analyze`
    /// rebuilds from scratch via the `remove_dir_all` step below.
    pub fn build_index(repo_path: &Path, graph: &ZeroCopyGraph) -> Result<(), String> {
        let index_dir = repo_path.join(".gitnexus-rs").join("tantivy");
        if index_dir.exists() {
            // Best-effort wipe: clears any stale `.tantivy-writer.lock`
            // or half-committed segments left by a killed prior run.
            // If this fails (Windows file lock held by zombie), the
            // `create_in_dir` below surfaces a clear error.
            let _ = fs::remove_dir_all(&index_dir);
        }
        fs::create_dir_all(&index_dir).map_err(|e| format!("create tantivy dir: {e}"))?;

        let mut schema_builder = Schema::builder();
        let uid_field = schema_builder.add_text_field("uid", STRING | STORED);
        let name_field = schema_builder.add_text_field("name", TEXT | STORED);
        let schema = schema_builder.build();

        let index = Index::create_in_dir(&index_dir, schema.clone())
            .map_err(|e| format!("create tantivy index: {e}"))?;
        // 2 worker threads × 30MB each: the sweet spot for our corpus
        // shape (10k-150k tiny `(uid, name)` docs). Empirically 2t × 30MB
        // beats 1t × 50MB (~350ms → ~240ms on a 150k-symbol corpus,
        // measured on .sample_repo). 4 threads regresses (~290-370ms) —
        // overhead of coordinating 4 workers exceeds the gain when each
        // doc is only a few dozen bytes. Per-thread budget must stay
        // above tantivy's `MEMORY_BUDGET_NUM_BYTES_MIN` (15MB) or
        // `writer_with_num_threads` errors out and analyze.rs's
        // best-effort `if let Err = ...` silently leaves an empty index.
        let mut index_writer: IndexWriter = index
            .writer_with_num_threads(2, 60_000_000)
            .map_err(|e| format!("acquire tantivy writer (lock held?): {e}"))?;

        for node in graph.nodes.iter() {
            let uid_start = node.uid.offset as usize;
            let uid_end = uid_start + node.uid.len as usize;
            let uid = std::str::from_utf8(&graph.string_pool[uid_start..uid_end]).unwrap_or("");

            let name_start = node.name.offset as usize;
            let name_end = name_start + node.name.len as usize;
            let name = std::str::from_utf8(&graph.string_pool[name_start..name_end]).unwrap_or("");

            let mut doc = tantivy::TantivyDocument::default();
            doc.add_text(uid_field, uid);
            doc.add_text(name_field, tokenize_identifier(name));
            index_writer
                .add_document(doc)
                .map_err(|e| format!("tantivy add_document: {e}"))?;
        }

        index_writer
            .commit()
            .map_err(|e| format!("tantivy commit: {e}"))?;
        Ok(())
    }

    pub fn search(repo_path: &Path, query_str: &str) -> Vec<(f32, String)> {
        let index_dir = repo_path.join(".gitnexus-rs").join("tantivy");
        if !index_dir.exists() {
            return vec![];
        }

        let index = match Index::open_in_dir(&index_dir) {
            Ok(idx) => idx,
            Err(_) => return vec![],
        };

        let reader = match index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
        {
            Ok(r) => r,
            Err(_) => return vec![],
        };

        let searcher = reader.searcher();
        let schema = index.schema();
        let name_field = schema.get_field("name").unwrap();
        let uid_field = schema.get_field("uid").unwrap();

        let query_parser = QueryParser::for_index(&index, vec![name_field]);
        let expanded = tokenize_identifier(query_str);
        let query = match query_parser.parse_query(&expanded) {
            Ok(q) => q,
            Err(_) => return vec![],
        };

        let top_docs = match searcher.search(&query, &TopDocs::with_limit(20).order_by_score()) {
            Ok(docs) => docs,
            Err(_) => return vec![],
        };

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            if let Ok(retrieved_doc) = searcher.doc::<tantivy::TantivyDocument>(doc_address) {
                if let Some(uid_val) = retrieved_doc.get_first(uid_field) {
                    if let Some(uid_str) = uid_val.as_str() {
                        results.push((score, uid_str.to_string()));
                    }
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::tokenize_identifier;

    #[test]
    fn snake_case_splits_on_underscore() {
        assert_eq!(
            tokenize_identifier("parse_config_file"),
            "parse_config_file parse config file"
        );
    }

    #[test]
    fn camel_case_splits_on_capital_transition() {
        assert_eq!(
            tokenize_identifier("parseConfig"),
            "parseConfig parse Config"
        );
    }

    #[test]
    fn pascal_case_splits_each_word() {
        assert_eq!(
            tokenize_identifier("ParseConfigFile"),
            "ParseConfigFile Parse Config File"
        );
    }

    #[test]
    fn acronym_followed_by_word_splits_cleanly() {
        // HTTPServer → HTTP | Server, not H | T | T | P | Server
        assert_eq!(tokenize_identifier("HTTPServer"), "HTTPServer HTTP Server");
    }

    #[test]
    fn letter_digit_boundary_splits() {
        assert_eq!(tokenize_identifier("utf8"), "utf8 utf 8");
        assert_eq!(
            tokenize_identifier("base64Decode"),
            "base64Decode base 64 Decode"
        );
    }

    #[test]
    fn mixed_separator_strips_punctuation() {
        assert_eq!(
            tokenize_identifier("foo.bar-baz/qux"),
            "foo.bar-baz/qux foo bar baz qux"
        );
    }

    #[test]
    fn single_lowercase_word_passes_through() {
        assert_eq!(tokenize_identifier("config"), "config config");
    }

    #[test]
    fn empty_string_yields_empty() {
        assert_eq!(tokenize_identifier(""), "");
    }
}
