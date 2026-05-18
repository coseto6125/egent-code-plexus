//! Test helper: simulates a slow reindex by appending its PID to a
//! marker file after a brief sleep. Used by
//! `tests/concurrency_hook_flock.rs` to confirm flock serialises
//! concurrent spawns to exactly one side-effect.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let marker = PathBuf::from(args.next().expect("arg 1: marker path"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&marker)
        .unwrap();
    writeln!(f, "{}", std::process::id()).unwrap();
}
