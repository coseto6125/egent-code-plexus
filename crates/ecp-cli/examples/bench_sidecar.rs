use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

fn sidecar_path(graph_path: &Path) -> PathBuf {
    let mut p = graph_path.as_os_str().to_owned();
    p.push(".compatible_version");
    PathBuf::from(p)
}

fn read_sidecar(graph_path: &Path) -> Option<u32> {
    let content = fs::read_to_string(sidecar_path(graph_path)).ok()?;
    content.trim().parse::<u32>().ok()
}

fn bench<F: FnMut()>(label: &str, n: usize, mut f: F) {
    let warmup = 5;
    for _ in 0..warmup {
        f();
    }
    let t = Instant::now();
    for _ in 0..n {
        f();
    }
    let dur = t.elapsed();
    let per_call = dur / n as u32;
    println!(
        "{:<55} n={n} total={:>10.3?} avg/call={:>10.3?}",
        label, dur, per_call
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let graph_bin = args
        .next()
        .expect("usage: bench_sidecar <graph.bin> [commits_dir]");
    let graph_path = PathBuf::from(&graph_bin);
    assert!(graph_path.is_file(), "graph.bin not found: {graph_bin}");
    let size_mb = fs::metadata(&graph_path).unwrap().len() as f64 / 1_048_576.0;
    println!("graph.bin: {size_mb:.1} MB  ({graph_bin})");

    let scp = sidecar_path(&graph_path);
    fs::write(&scp, b"10\n").expect("write sidecar");

    let commits_dir = args.next().map(PathBuf::from);
    if let Some(ref cd) = commits_dir {
        assert!(cd.is_dir(), "commits dir not found: {}", cd.display());
        let entries = fs::read_dir(cd).unwrap().count();
        println!("commits_dir: {} entries  ({})", entries, cd.display());
    }
    println!();

    let n = 100;
    println!("=== E2 sidecar A/B (n={n}, warm cache) ===");

    bench("E2: sidecar read (4-byte file)", n, || {
        let v = read_sidecar(&graph_path);
        std::hint::black_box(v);
    });
    bench("E2: header_compatible (mmap+rkyv::access)", n, || {
        let ok = ecp_cli::engine::header_compatible(&graph_path);
        std::hint::black_box(ok);
    });

    if let Some(cd) = commits_dir {
        println!("\n=== E1 cache A/B (n={n}, simulates MCP repeated ensure_fresh) ===");

        bench("E1 miss: find_latest_by_mtime + sidecar_check", n, || {
            let sib = ecp_cli::commit_lookup::find_latest_by_mtime(&cd);
            let ok = sib.map(|d| {
                let gp = d.join("graph.bin");
                read_sidecar(&gp).is_some()
            });
            std::hint::black_box(ok);
        });

        let cache: Mutex<HashMap<PathBuf, Option<PathBuf>>> = Mutex::new(HashMap::new());
        cache
            .lock()
            .unwrap()
            .insert(cd.clone(), Some(graph_path.clone()));
        bench("E1 hit:  HashMap.get() (cache simulation)", n, || {
            let map = cache.lock().unwrap();
            let v = map.get(&cd).cloned();
            std::hint::black_box(v);
        });
    } else {
        println!("\n(skip E1 bench — pass commits_dir as 2nd arg to enable)");
    }
}
