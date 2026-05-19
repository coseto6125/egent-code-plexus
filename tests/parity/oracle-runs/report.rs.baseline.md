# verify-resolver report (rs)

Oracle records: 4581
Cgn records: 21846

## Summary

| class | count |
|---|---|
| TP (correct) | 48 |
| FP_ghost (wrong target) | 327 |
| FP_overmatch (Global with alts) | 3 |
| FN_dangling (oracle resolved, cgn didn't) | 255 |
| tier_demoted (correct but fell back to Global) | 46 |
| oracle_only (oracle imports cgn never saw) | 3951 |
| cgn_only same-file (excluded from diff) | 3820 |
| cgn_only other (no oracle counterpart) | 16865 |

## Per-tier breakdown (cgn side)

| tier | TP | FP_ghost | FP_overmatch | tier_demoted | FN_dangling |
|---|---|---|---|---|---|
| Global | 46 | 275 | 3 | 46 | 0 |
| SameFile | 2 | 52 | 0 | 0 | 0 |
| Unresolved | 0 | 0 | 0 | 0 | 255 |
| oracle | 0 | 0 | 0 | 0 | 0 |

## Top-20 worst offenders

| src_file | name | class | detail |
|---|---|---|---|
| tokio/src/lib.rs | spawn | oracle_only_resolved |  |
| tokio/src/lib.rs | trace_leaf | oracle_only_resolved |  |
| tokio/src/lib.rs | os | oracle_only_resolved |  |
| tokio/src/lib.rs | os | oracle_only_resolved |  |
| tokio/src/lib.rs | select_priv_declare_output_enum | oracle_only_resolved |  |
| tokio/src/lib.rs | select_priv_clean_pattern | oracle_only_resolved |  |
| tokio/src/lib.rs | main | oracle_only_resolved |  |
| tokio/src/lib.rs | test | oracle_only_resolved |  |
| tokio/src/lib.rs | main | oracle_only_resolved |  |
| tokio/src/lib.rs | test | oracle_only_resolved |  |
| tokio/src/lib.rs | main | oracle_only_resolved |  |
| tokio/src/lib.rs | test | oracle_only_resolved |  |
| tokio/src/fs/mod.rs | canonicalize | oracle_only_resolved |  |
| tokio/src/fs/mod.rs | create_dir | oracle_only_resolved |  |
| tokio/src/fs/mod.rs | create_dir_all | oracle_only_resolved |  |
| tokio/src/fs/mod.rs | DirBuilder | oracle_only_resolved |  |
| tokio/src/fs/mod.rs | File | oracle_only_resolved |  |
| tokio/src/fs/mod.rs | hard_link | oracle_only_resolved |  |
| tokio/src/fs/mod.rs | metadata | oracle_only_resolved |  |
| tokio/src/fs/mod.rs | OpenOptions | oracle_only_resolved |  |
