//! Exact + BM25 cascade for cross-link generation. BM25 reuses Tantivy
//! — no new search dependency.

use crate::commands::group::types::{
    ContractRole, CrossLink, CrossLinkEndpoint, MatchType, StoredContract,
};
use graph_nexus_core::config::GroupConfig;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::{collector::TopDocs, query::QueryParser, Index, IndexWriter, ReloadPolicy, Searcher};

pub fn match_contracts(
    contracts: &[StoredContract],
    group_dir: &Path,
    cfg: &GroupConfig,
    exact_only: bool,
) -> io::Result<(Vec<CrossLink>, Vec<StoredContract>)> {
    let kept: Vec<&StoredContract> = contracts
        .iter()
        .filter(|c| !is_excluded(&c.inner.contract_id, cfg))
        .collect();

    let mut providers_by_id: HashMap<&str, Vec<&StoredContract>> = HashMap::new();
    let mut consumers_by_id: HashMap<&str, Vec<&StoredContract>> = HashMap::new();
    for c in &kept {
        match c.inner.role {
            ContractRole::Provider => {
                providers_by_id
                    .entry(&c.inner.contract_id)
                    .or_default()
                    .push(c);
            }
            ContractRole::Consumer => {
                consumers_by_id
                    .entry(&c.inner.contract_id)
                    .or_default()
                    .push(c);
            }
        }
    }

    let mut links: Vec<CrossLink> = Vec::new();
    let mut matched_uids: HashSet<String> = HashSet::new();

    // Exact stage
    for (id, providers) in &providers_by_id {
        let Some(consumers) = consumers_by_id.get(*id) else {
            continue;
        };
        for p in providers {
            for c in consumers {
                if p.repo == c.repo {
                    continue;
                }
                links.push(make_link(c, p, MatchType::Exact, 1.0));
                matched_uids.insert(c.inner.symbol_uid.clone());
                matched_uids.insert(p.inner.symbol_uid.clone());
            }
        }
    }

    // BM25 stage
    if !exact_only {
        let unmatched_consumers: Vec<&StoredContract> = kept
            .iter()
            .copied()
            .filter(|c| {
                c.inner.role == ContractRole::Consumer
                    && !matched_uids.contains(&c.inner.symbol_uid)
            })
            .collect();

        if !unmatched_consumers.is_empty() {
            let index_dir = group_dir.join("contracts_index");
            build_bm25_index(&index_dir, &kept).map_err(io::Error::other)?;

            // Open index + build searcher/parser ONCE — reused across all consumers.
            let (searcher, parser, uid_field) =
                open_bm25_searcher(&index_dir).map_err(io::Error::other)?;

            for cons in &unmatched_consumers {
                let candidates = bm25_search(
                    &searcher,
                    &parser,
                    uid_field,
                    &cons.inner.contract_id,
                    cfg.max_candidates_per_step as usize,
                );
                for (uid, score) in candidates {
                    if score < cfg.bm25_threshold {
                        continue;
                    }
                    let Some(prov) = kept.iter().copied().find(|c| {
                        c.inner.symbol_uid == uid && c.inner.role == ContractRole::Provider
                    }) else {
                        continue;
                    };
                    if prov.repo == cons.repo {
                        continue;
                    }
                    links.push(make_link(cons, prov, MatchType::Bm25, score));
                    // Only consumer UID tracked: unmatched-set filter only counts consumers,
                    // and a provider may legitimately link to many consumers (1-to-N service).
                    matched_uids.insert(cons.inner.symbol_uid.clone());
                }
            }
        }
    }

    let unmatched: Vec<StoredContract> = kept
        .iter()
        .copied()
        .filter(|c| {
            c.inner.role == ContractRole::Consumer && !matched_uids.contains(&c.inner.symbol_uid)
        })
        .cloned()
        .collect();

    Ok((links, unmatched))
}

fn make_link(from: &StoredContract, to: &StoredContract, mt: MatchType, conf: f32) -> CrossLink {
    CrossLink {
        from: CrossLinkEndpoint {
            repo: from.repo.clone(),
            service: from.inner.service.clone(),
            symbol_uid: from.inner.symbol_uid.clone(),
            symbol_ref: from.inner.symbol_ref.clone(),
        },
        to: CrossLinkEndpoint {
            repo: to.repo.clone(),
            service: to.inner.service.clone(),
            symbol_uid: to.inner.symbol_uid.clone(),
            symbol_ref: to.inner.symbol_ref.clone(),
        },
        contract_type: to.inner.contract_type.clone(),
        contract_id: to.inner.contract_id.clone(),
        match_type: mt,
        confidence: conf,
    }
}

fn is_excluded(contract_id: &str, cfg: &GroupConfig) -> bool {
    let Some(path) = contract_id.split(':').nth(2) else {
        return false;
    };
    let norm = path.trim_end_matches('/');
    if cfg
        .exclude_links_paths
        .iter()
        .any(|p| p.trim_end_matches('/') == norm)
    {
        return true;
    }
    if cfg.exclude_links_param_only_paths
        && norm
            .split('/')
            .filter(|s| !s.is_empty())
            .all(|s| s.starts_with('{') && s.ends_with('}'))
    {
        return true;
    }
    false
}

fn build_bm25_schema() -> (Schema, tantivy::schema::Field, tantivy::schema::Field) {
    let mut builder = Schema::builder();
    let contract_id_field = builder.add_text_field("contract_id", TEXT | STORED);
    let uid_field = builder.add_text_field("uid", STRING | STORED);
    let schema = builder.build();
    (schema, contract_id_field, uid_field)
}

/// Build a Tantivy index of all kept contracts at `index_dir/`.
/// Wipes and recreates the directory on each call so stale writer locks
/// from killed prior runs don't block. Writer is dropped before returning
/// so the same process can open a reader immediately after.
fn build_bm25_index(index_dir: &Path, kept: &[&StoredContract]) -> Result<(), String> {
    if index_dir.exists() {
        let _ = fs::remove_dir_all(index_dir);
    }
    fs::create_dir_all(index_dir).map_err(|e| format!("create contracts_index dir: {e}"))?;

    let (schema, contract_id_field, uid_field) = build_bm25_schema();
    let index = Index::create_in_dir(index_dir, schema)
        .map_err(|e| format!("create contracts bm25 index: {e}"))?;

    // Single-threaded writer is sufficient for the small contract corpus
    // (hundreds to low thousands of docs). Minimum budget is 15 MB.
    let mut writer: IndexWriter = index
        .writer(15_000_000)
        .map_err(|e| format!("acquire contracts writer: {e}"))?;

    for c in kept {
        // Escape `:` → space so Tantivy's query parser doesn't treat
        // `http:GET:/users` as field-qualified sub-queries.
        let escaped = c.inner.contract_id.replace(':', " ");
        let mut doc = tantivy::TantivyDocument::default();
        doc.add_text(contract_id_field, &escaped);
        doc.add_text(uid_field, &c.inner.symbol_uid);
        writer
            .add_document(doc)
            .map_err(|e| format!("tantivy add_document: {e}"))?;
    }

    writer
        .commit()
        .map_err(|e| format!("tantivy commit: {e}"))?;
    // Drop writer before caller opens a reader to release the write lock.
    drop(writer);
    Ok(())
}

/// Open the contracts Tantivy index and return a cached (Searcher, QueryParser, uid_field)
/// bundle. Called once per `match_contracts` invocation — not per consumer.
fn open_bm25_searcher(index_dir: &Path) -> Result<(Searcher, QueryParser, Field), String> {
    let index = Index::open_in_dir(index_dir)
        .map_err(|e| format!("group::bm25_search: Index::open_in_dir failed: {e:?}"))?;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .map_err(|e| format!("group::bm25_search: reader build failed: {e:?}"))?;
    let schema = index.schema();
    let contract_id_field = schema
        .get_field("contract_id")
        .map_err(|e| format!("group::bm25_search: missing contract_id field: {e:?}"))?;
    let uid_field = schema
        .get_field("uid")
        .map_err(|e| format!("group::bm25_search: missing uid field: {e:?}"))?;
    let parser = QueryParser::for_index(&index, vec![contract_id_field]);
    let searcher = reader.searcher();
    Ok((searcher, parser, uid_field))
}

/// Search the contracts Tantivy index using a pre-built searcher + parser.
/// Returns `(uid, score)` pairs ranked by BM25 score, capped at `limit`.
/// Returns empty vec on any Tantivy failure (graceful degradation).
fn bm25_search(
    searcher: &Searcher,
    parser: &QueryParser,
    uid_field: Field,
    contract_id: &str,
    limit: usize,
) -> Vec<(String, f32)> {
    // Escape `:` same way as at index time.
    let escaped = contract_id.replace(':', " ");
    let Ok(query) = parser.parse_query(&escaped) else {
        return Vec::new();
    };

    let top_docs = match searcher.search(&query, &TopDocs::with_limit(limit).order_by_score()) {
        Ok(docs) => docs,
        Err(e) => {
            tracing::warn!("group::bm25_search: searcher.search failed: {e:?}");
            return Vec::new();
        }
    };

    let mut results = Vec::with_capacity(top_docs.len());
    for (score, addr) in top_docs {
        let Ok(doc) = searcher.doc::<tantivy::TantivyDocument>(addr) else {
            continue;
        };
        if let Some(uid_val) = doc.get_first(uid_field) {
            if let Some(uid_str) = uid_val.as_str() {
                results.push((uid_str.to_string(), score));
            }
        }
    }
    results
}
