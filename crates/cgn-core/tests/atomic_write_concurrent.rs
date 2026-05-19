//! Regression: `atomic_write_bytes` previously used a fixed `<path>.tmp`
//! sibling — two concurrent writers truncated the same tmp inode and
//! interleaved their writes, producing a final file that contained the
//! shorter writer's prefix followed by the longer writer's tail. JSON
//! parsing later choked with "trailing characters at line N column M"
//! because what survived was two stacked JSON documents.
//!
//! Repro: spawn N threads writing JSONs of varying length to the same
//! target. Every final read must parse as valid JSON (one document).
//! Background context: PR #149 search_batch flake (Round 81).

use cgn_core::registry::{atomic_write_bytes, atomic_write_json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn tmpdir() -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "gnx-atomic-write-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn concurrent_atomic_write_bytes_never_corrupts_target() {
    let dir = tmpdir();
    let target = dir.join("payload.json");

    // 16 threads × 50 iterations alternating short (200 B) and long
    // (~60 KB) payloads. The original flake was reproducible with 600 KB
    // (20k entries) but the corruption signal fires the same way at 60 KB
    // (2k entries) — the race depends on inode-truncate timing, not
    // payload size — so we keep the 2k size to amortise the 800 probe
    // deserializations within CI's per-test budget (~400 ms vs ~4 s).
    let n_threads = 16;
    let iters = 50;
    let short = serde_json::json!({"k": "short"});
    let short_bytes = serde_json::to_vec_pretty(&short).unwrap();
    let long: serde_json::Value = serde_json::json!({
        "version": 1,
        "entries": (0..2_000)
            .map(|i| (format!("file_{i}.rs"), serde_json::Value::String(format!("hash_{i:032x}"))))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    });
    let long_bytes = serde_json::to_vec_pretty(&long).unwrap();

    let observed_corrupt = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for tid in 0..n_threads {
        let target = target.clone();
        let short_bytes = short_bytes.clone();
        let long_bytes = long_bytes.clone();
        let corrupt = Arc::clone(&observed_corrupt);
        handles.push(std::thread::spawn(move || {
            for i in 0..iters {
                let bytes = if (tid + i) % 2 == 0 { &short_bytes } else { &long_bytes };
                atomic_write_bytes(&target, bytes).expect("write");

                // Probe: re-read AFTER our rename — must always parse
                // as one valid JSON document. (A concurrent writer's
                // rename may have replaced our content, but it must
                // replace cleanly, not interleave.)
                if let Ok(contents) = std::fs::read(&target) {
                    if !contents.is_empty()
                        && serde_json::from_slice::<serde_json::Value>(&contents).is_err()
                    {
                        corrupt.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let n_corrupt = observed_corrupt.load(Ordering::Relaxed);
    assert_eq!(
        n_corrupt, 0,
        "atomic_write_bytes produced corrupt JSON on {} read probes \
         under concurrent writers (target: {})",
        n_corrupt,
        target.display()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_atomic_write_json_never_corrupts_target() {
    // Same scenario, but via atomic_write_json (the higher-level helper
    // used by DirtyFiles::write_atomic etc.).
    let dir = tmpdir();
    let target = dir.join("dirty_files.json");

    let n_threads = 8;
    let iters = 100;
    let observed_corrupt = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for tid in 0..n_threads {
        let target = target.clone();
        let corrupt = Arc::clone(&observed_corrupt);
        handles.push(std::thread::spawn(move || {
            for i in 0..iters {
                let payload = serde_json::json!({
                    "tid": tid,
                    "i": i,
                    "entries": (0..(if (tid + i) % 2 == 0 { 5 } else { 1000 }))
                        .map(|k| (format!("k_{k}"), serde_json::Value::String(format!("v_{k:020}"))))
                        .collect::<serde_json::Map<String, serde_json::Value>>(),
                });
                atomic_write_json(&target, &payload).expect("write");
                if let Ok(contents) = std::fs::read(&target) {
                    if !contents.is_empty()
                        && serde_json::from_slice::<serde_json::Value>(&contents).is_err()
                    {
                        corrupt.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let n_corrupt = observed_corrupt.load(Ordering::Relaxed);
    assert_eq!(
        n_corrupt, 0,
        "atomic_write_json corruption observed {} times under concurrent writers",
        n_corrupt
    );

    let _ = std::fs::remove_dir_all(&dir);
}
