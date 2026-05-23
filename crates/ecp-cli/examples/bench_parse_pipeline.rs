//! Pipeline-construction vs parse-cost bench. Run when `parity_gate_smoke`
//! (or any per-file `analyze` workflow) slows down, to bisect where the
//! cost lives without a full systematic-debugging pass.
//!
//! Run with:
//!   cargo run --release -p egent-code-plexus --example bench_parse_pipeline
//!
//! Output interpretation:
//!   - If [6] ≈ test wall  → pipeline construction is the bottleneck
//!     (parse_direct is calling `make_pipeline()` instead of cached
//!     `pipeline()`, or `make_pipeline()` itself got slower from
//!     added providers / capture queries).
//!   - If [5] dominates    → `parse_file` itself is the bottleneck
//!     (a per-language capture query grew too expensive).
//!   - If both small       → look elsewhere in the test harness
//!     (e.g. `reanalyze_files::tracked_files` git subprocess overhead,
//!     or proptest's tempdir churn).

use ecp_cli::reanalyze::{make_pipeline, pipeline};
use std::path::PathBuf;
use std::time::Instant;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/incremental_parity")
}

fn fixture_files() -> Vec<PathBuf> {
    let root = fixture_root();
    let mut out = Vec::new();
    for lang_entry in std::fs::read_dir(&root).expect("fixture dir") {
        let lang_dir = lang_entry.unwrap().path();
        if !lang_dir.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&lang_dir).unwrap() {
            let p = f.unwrap().path();
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn main() {
    let t = Instant::now();
    let _p1 = make_pipeline();
    eprintln!(
        "[1] make_pipeline() cold:        {:.3}s",
        t.elapsed().as_secs_f32()
    );

    let t = Instant::now();
    for _ in 0..10 {
        let _p = make_pipeline();
    }
    let total = t.elapsed().as_secs_f32();
    eprintln!(
        "[2] make_pipeline() x10:         {:.3}s  avg={:.3}s",
        total,
        total / 10.0
    );

    let t = Instant::now();
    for _ in 0..10 {
        let _p = pipeline();
    }
    eprintln!(
        "[3] pipeline() x10 cached:       {:.6}s",
        t.elapsed().as_secs_f32()
    );

    let files = fixture_files();
    let root = fixture_root();
    eprintln!("[4] fixture file count:          {}", files.len());

    let p = pipeline();
    let mut total_parse = 0.0_f64;
    for abs in &files {
        let rel = abs.strip_prefix(&root).unwrap().to_path_buf();
        let t = Instant::now();
        let _graphs = p.analyze(vec![(abs.clone(), rel)]);
        total_parse += t.elapsed().as_secs_f64();
    }
    eprintln!(
        "[5] per-file analyze x{} (cached):     {:.3}s  avg={:.4}s",
        files.len(),
        total_parse,
        total_parse / files.len() as f64
    );

    let n = 60;
    let t = Instant::now();
    for i in 0..n {
        let abs = &files[i % files.len()];
        let rel = abs.strip_prefix(&root).unwrap().to_path_buf();
        let p = make_pipeline();
        let _graphs = p.analyze(vec![(abs.clone(), rel)]);
    }
    eprintln!(
        "[6] 60x (make_pipeline + 1 file):  {:.3}s   <-- simulates the broken parse_direct path",
        t.elapsed().as_secs_f32()
    );

    let t = Instant::now();
    let p = pipeline();
    for i in 0..n {
        let abs = &files[i % files.len()];
        let rel = abs.strip_prefix(&root).unwrap().to_path_buf();
        let _graphs = p.analyze(vec![(abs.clone(), rel)]);
    }
    eprintln!(
        "[7] 60x (cached pipeline + 1 file):{:.3}s   <-- simulates the fixed parse_direct path",
        t.elapsed().as_secs_f32()
    );
}
