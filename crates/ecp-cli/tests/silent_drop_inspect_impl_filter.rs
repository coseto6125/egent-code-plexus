//! Pin the contract that `ecp inspect <name>` always carries `omitted_kinds`
//! so LLM consumers can detect when Impl nodes were silently filtered out
//! because a primary type (Trait/Struct/Class/Enum) matched the same name.
//!
//! Without `omitted_kinds`, an LLM asking `inspect Foo` on a struct spread
//! across multiple impl files would see zero Impl blocks and infer "no impl
//! blocks exist" — which is wrong.
//!
//! Contract: payload always carries `omitted_kinds` (object mapping kind name
//! → count).  When a primary type matches and Impl nodes are filtered, the
//! count must equal the number of dropped Impl blocks.
//!
//! Note: the parser creates one Impl node per *file* containing `impl Foo`.
//! Three separate impl files → three Impl nodes named "Foo" → omitted count 3.

mod common;

use common::{ecp_bin, init_and_analyze, write};

use serde_json::Value;
use std::process::Command;

#[test]
fn inspect_impl_filter_exposes_omitted_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // One struct Foo defined in lib.rs, then three separate impl files — each
    // in its own file so the parser emits one Impl node per file (all named
    // "Foo").  Three Impl nodes named "Foo" + one Struct "Foo" → the filter
    // suppresses all three Impl nodes and must record them in `omitted_kinds`.
    write(repo, "src/lib.rs", "pub struct Foo;\n");
    write(
        repo,
        "src/impl_a.rs",
        "use crate::Foo;\nimpl Foo { pub fn method_a(&self) {} }\n",
    );
    write(
        repo,
        "src/impl_b.rs",
        "use crate::Foo;\nimpl Foo { pub fn method_b(&self) {} }\n",
    );
    write(
        repo,
        "src/impl_c.rs",
        "use crate::Foo;\nimpl Foo { pub fn method_c(&self) {} }\n",
    );
    init_and_analyze(repo);

    let out = Command::new(ecp_bin())
        .args(["inspect", "Foo", "--format", "json", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp inspect failed to spawn");
    assert!(
        out.status.success(),
        "ecp inspect failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result: Value =
        serde_json::from_slice(&out.stdout).expect("ecp inspect produced non-JSON output");

    // `omitted_kinds` must always be present.
    let omitted = result
        .get("omitted_kinds")
        .expect("payload must carry `omitted_kinds`");

    let impl_count = omitted["Impl"]
        .as_u64()
        .expect("`omitted_kinds.Impl` must be a u64");
    assert_eq!(
        impl_count, 3,
        "expected 3 Impl nodes omitted (one per impl file), got {impl_count}: result={result}"
    );

    // The visible match must be the Struct node, not any Impl block.
    let kind = result["symbol"]["kind"]
        .as_str()
        .expect("payload must carry symbol.kind");
    assert_eq!(
        kind, "Struct",
        "primary match should be the Struct, got kind={kind}: result={result}"
    );
}
