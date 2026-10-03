use ecp_core::graph::ZeroCopyGraph;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tantivy::schema::*;
use tantivy::{
    collector::{Count, TopDocs},
    query::QueryParser,
    Index, IndexWriter, ReloadPolicy,
};

pub struct TantivyEngine;

static STALE_COUNTER: AtomicU64 = AtomicU64::new(0);

const BUILDING_PREFIX: &str = "tantivy.building.";

fn sweep_building_dirs(index_dir: &Path) {
    let Ok(entries) = fs::read_dir(index_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(BUILDING_PREFIX))
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn stale_tantivy_path(tantivy_dir: &Path) -> std::path::PathBuf {
    let parent = tantivy_dir.parent().unwrap_or_else(|| Path::new(""));
    let stale_name = format!(
        "{}.tantivy.dead.{}.{}.{}",
        parent
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("index"),
        std::process::id(),
        STALE_COUNTER.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    if let Some(root) = parent.parent() {
        root.join(stale_name)
    } else {
        parent.join(stale_name)
    }
}

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
    // Every subword is a contiguous run of alphanumerics, so it is a byte
    // range of `name` and needs no buffer of its own. The original
    // identifier is written first only once a second subword appears:
    // `"config" → "config config"` would double the term frequency and skew
    // BM25 IDF, so a single subword is returned alone.
    let mut out = String::new();
    let mut only = 0..0;
    let mut count = 0usize;
    let mut emit = |token: std::ops::Range<usize>| {
        match count {
            0 => only = token,
            1 => {
                out.reserve(name.len() * 2);
                out.push_str(name);
                out.push(' ');
                out.push_str(&name[only.clone()]);
                out.push(' ');
                out.push_str(&name[token]);
            }
            _ => {
                out.push(' ');
                out.push_str(&name[token]);
            }
        }
        count += 1;
    };

    let mut start: Option<usize> = None;
    let mut prev = '\0';
    let mut chars = name.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !c.is_alphanumeric() {
            if let Some(s) = start.take() {
                emit(s..i);
            }
            continue;
        }
        match start {
            None => start = Some(i),
            Some(s) => {
                // lower→Upper (parseConfig → parse | Config)
                let camel_boundary = prev.is_lowercase() && c.is_uppercase();
                // Upper→Upper→lower (HTTPServer → HTTP | Server): split between
                // the trailing capital and the new word's leading capital.
                let acronym_boundary = prev.is_uppercase()
                    && c.is_uppercase()
                    && chars.peek().is_some_and(|&(_, next)| next.is_lowercase());
                // letter↔digit boundary (utf8 → utf | 8, h2 → h | 2)
                let digit_boundary = prev.is_alphabetic() != c.is_alphabetic()
                    && (prev.is_ascii_digit() || c.is_ascii_digit());
                if camel_boundary || acronym_boundary || digit_boundary {
                    emit(s..i);
                    start = Some(i);
                }
            }
        }
        prev = c;
    }
    if let Some(s) = start {
        emit(s..name.len());
    }

    match count {
        0 => String::new(),
        1 => name[only].to_owned(),
        _ => out,
    }
}

impl TantivyEngine {
    /// Build the tantivy index into `<index_dir>/tantivy/`. `index_dir`
    /// is the resolved per-(repo, branch) directory under `~/.ecp/...`
    /// (or a tempdir in tests); the `tantivy` subdir is created on demand.
    /// Returns `Err` instead of panicking so the caller can degrade
    /// gracefully — `graph.bin` is the primary artifact and exact-name
    /// resolution still works when BM25 build fails (writer lock held
    /// by zombie, prior commit corrupt, FS full). The next `ecp analyze`
    /// rebuilds from scratch via the `remove_dir_all` step below.
    pub fn build_index(index_dir: &Path, graph: &ZeroCopyGraph) -> Result<(), String> {
        let final_dir = index_dir.join("tantivy");
        // Build beside the final dir and rename in after `commit()`: a reader
        // sees either the previous complete index or the new one, never an
        // index whose `meta.json` has no segments yet. Leftovers of a killed
        // build are swept first.
        sweep_building_dirs(index_dir);
        let index_dir = index_dir.join(format!("{BUILDING_PREFIX}{}", std::process::id()));
        fs::create_dir_all(&index_dir).map_err(|e| format!("create tantivy dir: {e}"))?;

        let mut schema_builder = Schema::builder();
        let uid_field = schema_builder.add_text_field("uid", STRING | STORED);
        // name is query-only (QueryParser parses against it); search()
        // only fetches uid_field. STORED here would write the doc store
        // for every node, with no reader ever calling get_first(name).
        let name_field = schema_builder.add_text_field("name", TEXT);
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
            let uid_str = node.uid.to_string();
            let uid = uid_str.as_str();

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
        drop(index_writer);
        if final_dir.exists() {
            // Move stale Tantivy state outside the build dir before async
            // cleanup. On Windows, a background delete under `<sha>.building/`
            // can keep a handle open and make the later `.building -> <sha>`
            // publish fail with os error 5.
            let dead_path = stale_tantivy_path(&final_dir);
            fs::rename(&final_dir, &dead_path)
                .map_err(|e| format!("move stale tantivy dir aside: {e}"))?;
            std::thread::spawn(move || {
                let _ = fs::remove_dir_all(dead_path);
            });
        }
        fs::rename(&index_dir, &final_dir).map_err(|e| format!("publish tantivy dir: {e}"))?;
        Ok(())
    }

    /// Query the tantivy index at `<index_dir>/tantivy/`. The return
    /// type distinguishes two failure modes that callers (especially
    /// `bm25_hits_from_graph`) need to handle differently:
    ///
    /// - `None` — index unavailable (missing dir, open failed, reader
    ///   build failed, query parse error). Caller should fall back to
    ///   substring scan so the hook still produces context.
    /// - `Some((empty, 0))` — index opened cleanly, query ran, BM25
    ///   genuinely matched nothing. Caller MUST NOT fall back, since
    ///   substring scan would produce noisy 0.4 hits that the trusted
    ///   index already ruled out.
    /// - `Some((vec, total))` — ranked uids + scores; `total` is the
    ///   exact document count matching the query across the whole index.
    ///   When `total > limit`, the result set was capped and downstream
    ///   consumers can surface the truncation to LLM callers.
    pub fn search(
        index_dir: &Path,
        query_str: &str,
        limit: usize,
    ) -> Option<(Vec<(f32, String)>, u64)> {
        let index_dir = index_dir.join("tantivy");
        if !index_dir.exists() {
            return None;
        }
        let index = Index::open_in_dir(&index_dir).ok()?;
        // Cold-open one-shot query: the index is already committed on disk and
        // never mutated during this searcher's lifetime, so OnCommitWithDelay's
        // background commit-watcher thread is pure overhead. Manual reads the
        // current committed state on first searcher() with no watcher.
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .ok()?;
        let searcher: tantivy::Searcher = reader.searcher();
        // `create_in_dir` writes `meta.json` with no segments before a single
        // document is added; an index still in that state was never
        // committed (the writer died), and it must not be read as "BM25
        // ruled out every symbol". Absent lets the substring scan answer.
        if searcher.segment_readers().is_empty() {
            return None;
        }
        let schema = index.schema();
        let name_field = schema.get_field("name").unwrap();
        let uid_field = schema.get_field("uid").unwrap();

        let query_parser = QueryParser::for_index(&index, vec![name_field]);
        let expanded = tokenize_identifier(query_str);
        let query = query_parser.parse_query(&expanded).ok()?;
        // Count runs alongside TopDocs at zero extra cost — tantivy's tuple
        // collector fuses both in a single index scan pass.
        let (total_count, top_docs) = searcher
            .search(
                &query,
                &(Count, TopDocs::with_limit(limit).order_by_score()),
            )
            .ok()?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            if let Ok(retrieved_doc) = searcher.doc::<tantivy::TantivyDocument>(doc_address) {
                if let Some(uid_val) = retrieved_doc.get_first(uid_field) {
                    if let Some(uid_str) = uid_val.as_str() {
                        results.push((score, uid_str.to_string()));
                    }
                }
            }
        }

        Some((results, total_count as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::{stale_tantivy_path, tokenize_identifier};
    use std::path::Path;

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
        // No splits → return the input only (no `"config config"` duplicate
        // that would skew BM25 IDF for unsplittable identifiers).
        assert_eq!(tokenize_identifier("config"), "config");
    }

    #[test]
    fn empty_string_yields_empty() {
        assert_eq!(tokenize_identifier(""), "");
    }

    #[test]
    fn test_tokenize_identifier_case_digit_unicode_table_exact_tokens() {
        let cases: &[(&str, &[&str])] = &[
            (
                "parseConfigFile",
                &["parseConfigFile", "parse", "Config", "File"],
            ),
            (
                "ParseConfigFile",
                &["ParseConfigFile", "Parse", "Config", "File"],
            ),
            (
                "parse_config_file",
                &["parse_config_file", "parse", "config", "file"],
            ),
            (
                "MAX_BUFFER_SIZE",
                &["MAX_BUFFER_SIZE", "MAX", "BUFFER", "SIZE"],
            ),
            (
                "HTTPServer2Go",
                &["HTTPServer2Go", "HTTP", "Server", "2", "Go"],
            ),
            ("parseHTML", &["parseHTML", "parse", "HTML"]),
            ("ABCd", &["ABCd", "AB", "Cd"]),
            ("x86_64", &["x86_64", "x", "86", "64"]),
            ("A1", &["A1", "A", "1"]),
            ("config", &["config"]),
            ("ABC", &["ABC"]),
            // One subword after stripping separators: the subword, not the input.
            ("_config_", &["config"]),
            ("", &[]),
            ("___", &[]),
            ("naïveÉtat", &["naïveÉtat", "naïve", "État"]),
            // CJK letters are neither cased nor digits: no boundary inside.
            ("解析設定", &["解析設定"]),
            ("parse解析Config", &["parse解析Config"]),
            ("rocket🚀Launch", &["rocket🚀Launch", "rocket", "Launch"]),
            ("🚀", &[]),
            // Non-ASCII digits are alphanumeric but not a digit boundary.
            ("x١٢", &["x١٢"]),
        ];
        for (input, want) in cases {
            assert_eq!(
                tokenize_identifier(input),
                want.join(" "),
                "input {input:?}"
            );
        }
    }

    /// The tokenizer before it moved to byte ranges, kept verbatim as the
    /// oracle: BM25 term frequencies depend on the exact token sequence.
    fn tokenize_identifier_reference(name: &str) -> String {
        let mut tokens: Vec<String> = Vec::with_capacity(4);
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
                let camel_boundary = prev.is_lowercase() && c.is_uppercase();
                let acronym_boundary = prev.is_uppercase()
                    && c.is_uppercase()
                    && i + 1 < chars.len()
                    && chars[i + 1].is_lowercase();
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
        match tokens.len() {
            0 => String::new(),
            1 => tokens.into_iter().next().unwrap(),
            _ => {
                let mut out = String::with_capacity(name.len() * 2);
                out.push_str(name);
                for t in &tokens {
                    out.push(' ');
                    out.push_str(t);
                }
                out
            }
        }
    }

    #[test]
    fn test_tokenize_identifier_generated_inputs_match_reference() {
        // Cased ASCII, digits, separators, a space, multi-byte cased letters
        // (ï É ß İ), an uncased letter (解), a non-letter symbol (🚀) and a
        // non-ASCII digit (١).
        const ALPHABET: [char; 17] = [
            'a', 'z', 'A', 'Z', '0', '9', '_', '-', '.', ' ', 'ï', 'É', 'ß', 'İ', '解', '🚀', '١',
        ];
        let check = |input: &str| {
            assert_eq!(
                tokenize_identifier(input),
                tokenize_identifier_reference(input),
                "input {input:?}"
            );
        };

        // Every string of length 0..=3: 1 + 17 + 289 + 4913 inputs.
        let mut short = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..3 {
            frontier = frontier
                .iter()
                .flat_map(|p| ALPHABET.iter().map(move |&c| format!("{p}{c}")))
                .collect();
            short.extend(frontier.iter().cloned());
        }
        assert_eq!(short.len(), 5220);
        for s in &short {
            check(s);
        }

        // 3000 longer strings, lengths 4..=19, from a fixed-seed LCG.
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (x >> 33) as usize
        };
        for _ in 0..3000 {
            let len = 4 + next() % 16;
            let s: String = (0..len)
                .map(|_| ALPHABET[next() % ALPHABET.len()])
                .collect();
            check(&s);
        }
    }

    #[test]
    fn stale_tantivy_path_lands_outside_build_dir() {
        let commits = Path::new("repo").join("commits");
        let stale = stale_tantivy_path(&commits.join("main__abc123.building").join("tantivy"));

        assert_eq!(stale.parent().unwrap(), commits);
        assert!(stale
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("main__abc123.building.tantivy.dead."));
    }

    // ── TantivyEngine::search total-count tests ───────────────────────────────

    use ecp_core::graph::ZeroCopyGraph;
    use ecp_core::graph_fixture::GraphFixture;
    use tempfile::TempDir;

    fn build_graph_with_names(names: &[&str]) -> ZeroCopyGraph {
        let mut fx = GraphFixture::new();
        for name in names {
            let id = fx.func("src/x.ts", name);
            fx.span(id, (1, 0, 2, 0));
        }
        fx.build()
    }

    fn index_dir_for(graph: &ZeroCopyGraph) -> TempDir {
        let dir = TempDir::new().unwrap();
        super::TantivyEngine::build_index(dir.path(), graph).unwrap();
        dir
    }

    #[test]
    fn search_total_equals_hit_count_when_below_cap() {
        // 3 nodes all matching "parse" — well below any MULTI_CAP
        let names = ["parseConfig", "parseFile", "parseToken"];
        let graph = build_graph_with_names(&names);
        let dir = index_dir_for(&graph);

        let (hits, total) = super::TantivyEngine::search(dir.path(), "parse", 100)
            .expect("index should be readable");
        assert_eq!(
            hits.len(),
            total as usize,
            "total must equal returned hit count when no cap applied"
        );
        assert_eq!(total, 3);
    }

    #[test]
    fn search_total_exceeds_hits_when_cap_applied() {
        // 110 nodes all matching "handler" — more than the MULTI_CAP of 100
        let names: Vec<String> = (0..110).map(|i| format!("handler{i}")).collect();
        let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let graph = build_graph_with_names(&name_refs);
        let dir = index_dir_for(&graph);

        let cap = 100;
        let (hits, total) = super::TantivyEngine::search(dir.path(), "handler", cap)
            .expect("index should be readable");
        assert_eq!(hits.len(), cap, "result set capped at limit");
        assert!(
            total > cap as u64,
            "total must exceed cap when index has more matches"
        );
        assert_eq!(total, 110);
    }
}
