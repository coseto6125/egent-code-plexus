use gnx_core::graph::ZeroCopyGraph;
use std::fs;
use std::path::Path;
use tantivy::schema::*;
use tantivy::{collector::TopDocs, query::QueryParser, Index, IndexWriter, ReloadPolicy};

pub struct TantivyEngine;

impl TantivyEngine {
    pub fn build_index(repo_path: &Path, graph: &ZeroCopyGraph) {
        let index_dir = repo_path.join(".gitnexus-rs").join("tantivy");
        if index_dir.exists() {
            let _ = fs::remove_dir_all(&index_dir);
        }
        fs::create_dir_all(&index_dir).unwrap();

        let mut schema_builder = Schema::builder();
        let uid_field = schema_builder.add_text_field("uid", STRING | STORED);
        let name_field = schema_builder.add_text_field("name", TEXT | STORED);
        let schema = schema_builder.build();

        let index = Index::create_in_dir(&index_dir, schema.clone()).unwrap();
        let mut index_writer: IndexWriter = index.writer(50_000_000).unwrap();

        for node in graph.nodes.iter() {
            let uid_start = node.uid.offset as usize;
            let uid_end = uid_start + node.uid.len as usize;
            let uid = std::str::from_utf8(&graph.string_pool[uid_start..uid_end]).unwrap_or("");

            let name_start = node.name.offset as usize;
            let name_end = name_start + node.name.len as usize;
            let name = std::str::from_utf8(&graph.string_pool[name_start..name_end]).unwrap_or("");

            let mut doc = tantivy::TantivyDocument::default();
            doc.add_text(uid_field, uid);
            doc.add_text(name_field, name);
            index_writer.add_document(doc).unwrap();
        }

        index_writer.commit().unwrap();
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
        let query = match query_parser.parse_query(query_str) {
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
