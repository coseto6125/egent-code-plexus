//! Concurrency invariant 4.4 — StringPool intern dedupe.
//!
//! `StringPool::add` is `&mut self`, so direct concurrent use across
//! threads is rejected by the type system. This test pins the contract:
//! anywhere a pool is shared across threads, it MUST be wrapped in
//! `Mutex`/`RwLock`, and the wrap MUST preserve dedup.

use graph_nexus_core::pool::StringPool;
use std::sync::{Arc, Mutex};
use std::thread;

#[test]
fn string_pool_serial_dedupe_holds_under_pressure() {
    let mut pool = StringPool::new();
    let unique: Vec<String> = (0..1_000).map(|i| format!("uid_{i:04}")).collect();

    // Insert 10 times — must dedupe
    for _ in 0..10 {
        for s in &unique {
            pool.add(s);
        }
    }

    let expected_bytes: usize = unique.iter().map(|s| s.len()).sum();
    assert_eq!(
        pool.bytes.len(),
        expected_bytes,
        "serial dedup leaked bytes: {} actual vs {} expected",
        pool.bytes.len(),
        expected_bytes,
    );
    assert_eq!(pool.index.len(), unique.len());
}

#[test]
fn string_pool_mutex_wrapped_concurrent_dedupe() {
    let pool = Arc::new(Mutex::new(StringPool::new()));
    let unique: Vec<String> = (0..100).map(|i| format!("uid_{i:03}")).collect();
    let unique_arc = Arc::new(unique.clone());

    let mut handles = Vec::new();
    for _thread_id in 0..8 {
        let pool = Arc::clone(&pool);
        let unique = Arc::clone(&unique_arc);
        handles.push(thread::spawn(move || {
            for s in unique.iter() {
                let mut p = pool.lock().unwrap();
                p.add(s);
            }
        }));
    }
    for h in handles {
        h.join().expect("thread panicked");
    }

    let pool = pool.lock().unwrap();
    let expected_bytes: usize = unique.iter().map(|s| s.len()).sum();
    assert_eq!(
        pool.bytes.len(),
        expected_bytes,
        "Mutex-wrapped concurrent dedup leaked bytes — wrap is broken or dedup logic raced",
    );
    assert_eq!(pool.index.len(), unique.len());

    // Cross-check via raw byte resolution: every unique string must round-trip
    for s in unique.iter() {
        let offset = pool.index[s];
        let resolved = std::str::from_utf8(
            &pool.bytes[offset as usize..(offset as usize + s.len())]
        ).unwrap();
        assert_eq!(resolved, s);
    }
}
