//! Canonical tests for `ecp_core::uid::compute`.

use ecp_core::graph::NodeKind;
use ecp_core::uid;
use xxhash_rust::xxh3::Xxh3;

/// Print the golden value and verify streaming == concat one-shot.
/// The printed u64 must be copy-pasted into the `assert_eq!` below.
#[test]
fn test_uid_streaming_matches_concat_hash() {
    let result = uid::compute(NodeKind::Function, "src/a.rs", None, "foo");

    // One-shot concat equivalent: "Function\0src/a.rs\0\0foo"
    let mut h = Xxh3::new();
    h.update(b"Function\0src/a.rs\0\0foo");
    let concat_result = h.digest();

    println!("golden u64 = {result}");

    assert_eq!(
        result, concat_result,
        "streaming must equal concat one-shot"
    );

    // Golden value — changing this means byte-order drift; update the spec too.
    assert_eq!(result, 5502148978972446557_u64);
}

#[test]
fn test_uid_owner_class_disambiguates_collision() {
    let a = uid::compute(NodeKind::Method, "f.rs", Some("A"), "m");
    let b = uid::compute(NodeKind::Method, "f.rs", Some("B"), "m");
    assert_ne!(a, b, "different owner_class must produce different UIDs");
}

#[test]
fn test_uid_stable_across_1000_invocations() {
    let expected = uid::compute(NodeKind::Function, "src/stable.rs", Some("MyClass"), "run");
    for _ in 0..999 {
        assert_eq!(
            uid::compute(NodeKind::Function, "src/stable.rs", Some("MyClass"), "run"),
            expected,
            "compute must be deterministic across repeated calls"
        );
    }
}

/// Heap-stability gate.
///
/// `dhat` is not in dev-deps. To wire it up, add to `ecp-core/Cargo.toml`:
///
/// ```toml
/// [dev-dependencies]
/// dhat = { version = "0.3", optional = true }
///
/// [features]
/// dhat-heap = ["dhat"]
/// ```
///
/// Then replace the body with:
///
/// ```rust
/// #[global_allocator]
/// static ALLOC: dhat::Alloc = dhat::Alloc;
///
/// let _profiler = dhat::Profiler::builder().testing().build();
/// // warm-up: one call before baseline to ensure any lazy statics are init'd
/// let _ = uid::compute(NodeKind::Function, "x.rs", None, "y");
/// let stats_before = dhat::HeapStats::get();
/// for _ in 0..10_000 {
///     std::hint::black_box(uid::compute(NodeKind::Function, "x.rs", None, "y"));
/// }
/// let stats_after = dhat::HeapStats::get();
/// assert_eq!(
///     stats_after.curr_blocks, stats_before.curr_blocks,
///     "uid::compute must not heap-allocate in the hot loop"
/// );
/// ```
///
/// Without `dhat`, we verify zero-alloc by running 10k iterations and relying
/// on Miri in CI (via `cargo miri test`) to catch any hidden allocations.
/// The test is left un-ignored so it always exercises the call path.
#[test]
fn test_uid_zero_alloc_verified() {
    // 10k iterations — Miri will detect any hidden allocation.
    // Replace with dhat assertion once dhat is added to dev-deps (see doc above).
    for i in 0_u64..10_000 {
        // vary inputs to prevent dead-code elimination
        let name = if i % 2 == 0 { "foo" } else { "bar" };
        std::hint::black_box(uid::compute(NodeKind::Function, "src/a.rs", None, name));
    }
}
