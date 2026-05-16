# Concurrency Audit — Findings

**Date started:** 2026-05-16
**Spec:** [2026-05-16-concurrency-audit-design.md](./2026-05-16-concurrency-audit-design.md)
**Status:** Open

## §3 Inventory pass

### 3.1 Rayon parallel iterators
(populated by Task 2)

### 3.2 Interior mutability (RefCell / Cell / UnsafeCell)
(populated by Task 2)

### 3.3 Unsafe blocks
(populated by Task 2)

### 3.4 Shared mutex / atomic state
(populated by Task 2)

### 3.5 File locks
(populated by Task 2)

### 3.6 Process / thread spawn
(populated by Task 2)

## §4 Hot-path equivalence test results
(populated by Phases 2–6)

## §5 TSan results
(populated by Phase 7)

## §6 Performance findings (surfaced)
(populated by Phase 8)

## §7 Bugs found
(populated incrementally; each row links to fix-PR/commit)

## §8 Closure checklist
- [ ] All §3 axes populated
- [ ] All 5 §4 tests PASS under `--test-threads=1` and `--test-threads=N`
- [ ] Zero unfiltered TSan reports
- [ ] All §7 bugs have merged fixes
- [ ] All §6 perf items have follow-up issues filed (or marked documented-tradeoff)
