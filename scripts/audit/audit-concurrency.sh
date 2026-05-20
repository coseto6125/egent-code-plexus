#!/usr/bin/env bash
# scripts/audit/audit-concurrency.sh
# Re-run the concurrency audit suite. Required before each ecp release tag
# and before each parity sub-project merge.
#
# Sub-projects 1-6 of the parity roadmap each extend the equivalence tests
# below; running this script catches regressions before merge.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "==> Building test helper binaries"
cargo build -p ecp-core --example registry_writer_child
cargo build -p egent-code-plexus --example slow_noop

echo "==> Equivalence tests — --test-threads=1"
cargo test -p ecp-core --test concurrency_string_pool_intern -- --test-threads=1
cargo test -p ecp-core --test concurrency_registry_writers -- --test-threads=1
cargo test -p ecp-analyzer --test concurrency_graph_builder_order -- --test-threads=1
cargo test -p ecp-analyzer --lib resolution::builder::tests::pass2_parallel_serial_identical_per_reltype -- --test-threads=1
cargo test -p egent-code-plexus --test concurrency_hook_flock -- --test-threads=1

NPROC="$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
echo "==> Equivalence tests — --test-threads=$NPROC"
cargo test -p ecp-core --test concurrency_string_pool_intern -- --test-threads="$NPROC"
cargo test -p ecp-core --test concurrency_registry_writers -- --test-threads="$NPROC"
cargo test -p ecp-analyzer --test concurrency_graph_builder_order -- --test-threads="$NPROC"
cargo test -p ecp-analyzer --lib resolution::builder::tests::pass2_parallel_serial_identical_per_reltype -- --test-threads="$NPROC"
cargo test -p egent-code-plexus --test concurrency_hook_flock -- --test-threads="$NPROC"

# TSan run (best-effort: nightly toolchain + sanitizer libs + rust-src)
if rustup toolchain list 2>/dev/null | grep -q nightly \
   && [ "$(uname -s)" = "Linux" ] \
   && rustup component list --toolchain nightly --installed 2>/dev/null | grep -q rust-src; then
  echo "==> TSan run (nightly)"
  for crate in ecp-core ecp-analyzer; do
    RUSTFLAGS="-Z sanitizer=thread" \
    RUSTDOCFLAGS="-Z sanitizer=thread" \
    cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu \
      -p "$crate" --tests -- --test-threads=4 \
      2>&1 | tee "/tmp/tsan-$crate.log" \
      | grep "WARNING: ThreadSanitizer" \
      && { echo "TSan reports in $crate — see /tmp/tsan-$crate.log"; exit 1; } \
      || true
  done
else
  echo "==> TSan run SKIPPED — nightly toolchain, Linux, or rust-src not available"
  echo "    Re-enable via: rustup component add rust-src --toolchain nightly-x86_64-unknown-linux-gnu"
fi

echo "==> Audit PASS"
