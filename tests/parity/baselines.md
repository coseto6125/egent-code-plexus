# Wave 1 Language Baseline Stats

Captured: 2026-05-14  
gnx binary: `target/debug/gnx` (dev profile)  
Command: `gnx analyze --repo .sample_repo/<lang>`

## Baseline Results

| Lang       | Upstream Repo                              | Files Scanned | Nodes | Scan Time | Analyze Time | Total Time | Status  |
|------------|--------------------------------------------|--------------|-------|-----------|--------------|------------|---------|
| lua        | kikito/middleclass (depth=1)               | 15           | 148   | 3.5 ms    | 329 ms       | 433 ms     | OK      |
| solidity   | OpenZeppelin/openzeppelin-contracts (d=1)  | 727          | 4862  | 17.9 ms   | 408 ms       | 765 ms     | OK      |
| bash       | Bash-it/bash-it (depth=1)                  | 398          | 5595  | 18.1 ms   | 350 ms       | 560 ms     | OK      |
| zig        | karlseguin/http.zig (depth=1)              | 1            | 0     | 3.7 ms    | 334 ms       | 370 ms     | PARTIAL |
| crystal    | kemalcr/kemal (depth=1)                    | 80           | 423   | 5.2 ms    | 337 ms       | 459 ms     | OK      |
| dockerfile | docker-library/postgres (depth=1)          | 68           | 1266  | 5.1 ms    | 352 ms       | 475 ms     | OK      |
| move       | aptos-labs/aptos-core (sparse, d=1)        | 486          | 7743  | 16.2 ms   | 681 ms       | 1647 ms    | OK      |

## Notes

### zig — PARTIAL (0 nodes)

The `.zig` extension is not registered in `analyze.rs`'s file-extension match arm.
Only `readme.md` from the http.zig repo was scanned (1 file, 0 Zig-specific nodes).
The `analyze` command exited 0, but no Zig symbols were extracted.

**Fix required:** Add `"zig"` to the extension match in
`crates/gnx-cli/src/commands/analyze.rs` and implement a `ZigProvider` in
`crates/gnx-analyzer/src/zig/`.

### move — sparse checkout

Aptos-core is ~3 GB total; sparse checkout of `aptos-move/framework/` reduces
it to ~15 MB on disk. The 486 files include `.move` sources only from that
subtree.

### Disk usage (.sample_repo/wave-1 only)

| Dir          | Size  |
|--------------|-------|
| lua          | 324 K |
| solidity     | 22 M  |
| bash         | 5.7 M |
| zig          | 928 K |
| crystal      | 948 K |
| dockerfile   | 1.1 M |
| move         | 15 M  |
| **Total**    | **~46 M** |
