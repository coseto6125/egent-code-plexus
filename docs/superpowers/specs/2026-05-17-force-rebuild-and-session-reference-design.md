# Force Rebuild + Session Reference Model

| Field | Value |
|---|---|
| Status | Draft |
| Date | 2026-05-17 |
| Scope | `gnx admin index --force` semantic in v2 layout；新增 SessionState first-class concept；query hot-path PureReference fast-path；admin sessions list state column；移除 `--no-cache / --embeddings / --drop-embeddings` |
| Affects | `graph-nexus-cli::{commands::admin::index, commands::admin::sessions, build::orchestrator, engine}`, `graph-nexus-core::session` (新增 `state.rs`) |
| Parent | `docs/superpowers/specs/2026-05-17-index-layout-redesign-design.md` (v2 layout) |
| Follow-up to | `docs/feat/2026-05-17-index-layout-followups.md` P3 第二項 |

---

## 1. Motivation

v2 index layout（PR #55）把 storage 改成 commit-content-addressed L2 + session-local L1 之後，遺留了幾個 design gap：

1. **`--force / --no-cache` 是 warn-no-op**。`admin/index.rs:303–309` 接到 flag 只 eprintln，然後直接 `build_l2(&worktree, None)`，沒有 force 語意。
2. **`build_l2` 沒有 skip-if-exists**。同 SHA 已建好時，`fs::rename(building, commit_dir)` 在 commit_dir 非空時 `ENOTEMPTY` 失敗 — `admin index` 重跑同 SHA 一律報錯而不是 idempotent。
3. **L1 dirty 概念隱式**。spec §5 hot-path 與 §8 promotion 都引用「session 有沒有 dirty file」但沒有 first-class state；每個 caller 自己讀 `dirty_files.json` 自己判斷，邏輯重複且各自 inconsistent。
4. **PR #51 已 hard-delete embedding pipeline**，但 `IndexArgs` 仍保留 `--embeddings / --drop-embeddings` 兩個沒語意的 flag。

本 spec 一次解這 4 條。

## 2. Goals / Non-goals

### Goals

- `--force` 是「drop + reconstruct」的明確語意：drop target SHA 的 L2 dir、invalidate 該 SHA 的 dirty L1 sessions、重 build L2。
- 引入 `SessionState` 為 derived state，供 `--force` selective invalidate / query hot-path early branch / admin sessions list 三方 caller 共用。
- Query hot-path 在 PureReference state 下跳過 overlay merge stage。
- `admin index` 不帶 `--force` 時是 idempotent — 同 SHA 已建好就 silent skip + 提示。
- 移除 `--no-cache / --embeddings / --drop-embeddings`。Project pre-1.0，不留 deprecation 期。

### Non-goals

- AugmentedReference 的 overlay merge 實作 — 由 P2 (`docs/feat/2026-05-17-index-layout-followups.md` §P2) 處理；本 spec 僅 wire `SessionState::AugmentedReference` 分支，merge 邏輯仍走 P2 完成前的 fallback。
- `admin index --rev <sha>` — 目前隱式 HEAD；加 `--rev` 與 spec §11.1 query 命令 `--rev` 一併推（後續 PR）。
- `gnx admin force` 獨立命令 — flag 已足夠，不引入新 subcommand。
- 強搶其他 process 的 build lock — `--force` 與其他 builder 並發時走 `wait_for_completion` 等對方 publish，再 drop+rebuild。

## 3. Session Reference Model

### 3.1 SessionState enum

```rust
// crates/graph-nexus-core/src/session/state.rs (new)
pub enum SessionState {
    /// dirty_files.json missing or entries 空。
    /// Session 對 base L2 無任何 overlay；query 路徑可直接走 L2-only。
    PureReference {
        base_sha: String,
        l2_dirname: String,
    },

    /// dirty_files.entries 非空。Session 有 graph_overlay/ + tantivy_delta/。
    /// query 路徑需走 L2 + overlay merge（依賴 P2 完成度）。
    AugmentedReference {
        base_sha: String,
        l2_dirname: String,
        fragment_count: usize,
    },

    /// session_meta.json 損毀 / dirty_files.json 損毀 / l2_dirname 不存在。
    /// query 拒絕 open；admin sessions list 顯示原因；GC sweep 該回收。
    Stale {
        reason: StaleReason,
    },
}

pub enum StaleReason {
    MetaUnreadable,
    DirtyFilesCorrupt,
    L2Missing,
    Orphan,                              // last_touched > 24h（同 v2 spec §10 規則）
}
```

### 3.2 classify() 演算法

```rust
impl SessionState {
    pub fn classify(repo_root: &Path, sid: &str) -> Self {
        let sid_dir = repo_root.join("sessions").join(sid);
        let sm_path = sid_dir.join("session_meta.json");
        let sm = match SessionMeta::try_read(&sm_path) {
            Ok(sm) => sm,
            Err(_) => return Self::Stale { reason: StaleReason::MetaUnreadable },
        };

        // 對應 L2 dir 是否存在
        let l2_dirname = match resolve_l2_dirname(repo_root, &sm.base_sha) {
            Some(d) => d,
            None => return Self::Stale { reason: StaleReason::L2Missing },
        };

        let dirty_path = sid_dir.join("dirty_files.json");
        let dirty = match DirtyFiles::try_read(&dirty_path) {
            Ok(d) => d,
            Err(io_err) if io_err.kind() == ErrorKind::NotFound => DirtyFiles::empty(),
            Err(_) => return Self::Stale { reason: StaleReason::DirtyFilesCorrupt },
        };

        if dirty.entries.is_empty() {
            Self::PureReference { base_sha: sm.base_sha, l2_dirname }
        } else {
            Self::AugmentedReference {
                base_sha: sm.base_sha,
                l2_dirname,
                fragment_count: dirty.entries.len(),
            }
        }
    }
}
```

### 3.3 不持久化

`SessionState` 是 **derived view**，不寫進 `session_meta.json`。原因：

- v2 spec §2 原則「Filesystem 是 source of truth」。
- 持久化會引入 sync invariant：每次 `DirtyFiles` mutate 就要連動 update SessionMeta — 增加 atomic write 範圍 + 多 caller 容易遺漏。
- classify 成本只是兩個 small json read（< 10KB 各），cached 在 process 內若需要。

### 3.4 Invariants

| # | Invariant |
|---|---|
| S1 | `SessionState::classify` 是 pure function of (session_meta, dirty_files, l2 commits/) — 同輸入兩次 classify 結果相等 |
| S2 | `PureReference` ⇒ `sessions/<sid>/graph_overlay/` 內所有檔皆為 GC 可清的 orphan（dirty_files 沒列即為孤兒）|
| S3 | `Stale` ⇒ query path 拒絕用此 session；CLI 入口走 fallback `cli-<pid_hash8>` |

## 4. Force Rebuild Behaviour

### 4.1 Behaviour matrix

| `--force` | L2 (target SHA) | `.building/` lock | 行為 |
|---|---|---|---|
| ✗ | absent | — | 正常 `build_l2` |
| ✗ | present | — | silent skip + `l2.exists sha=<8> (use --force to rebuild)` exit 0 |
| ✓ | absent | unlocked | build → 不影響 L1 (沒有 base_sha == target 的 session 可動) |
| ✓ | present | unlocked | invalidate L1（selective）→ drop L2 + orphan `.building/` → build |
| ✓ | — | locked by other | `wait_for_completion` → unlocked → 走「present + unlocked」分支 |
| ✗ | absent | locked by other | `wait_for_completion`（現況 attach pattern）|
| ✗ | present | locked by other | silent skip（不等對方）|

### 4.2 Force rebuild flow

```
fn force_rebuild_l2(worktree, target_sha) -> io::Result<BuildResult>:

1. sha_hex + dirname 解析（同現況 build_l2 §1）
   repo_root = ~/.gnx/<repo_dir_name>/
   dirname   = pick_dirname(worktree, sha_hex)
   commit_dir = repo_root.join("commits").join(&dirname)
   building   = repo_root.join("commits").join(format!("{dirname}.building"))

2. Acquire build lock
   create_dir_all(&building)
   lock_file = File::open(building/.build.lock)
   try_lock_exclusive(lock_file):
     ok    => proceed
     fail  => wait_for_completion(&building, &commit_dir)
              [此時 commit_dir 已 publish]
              create_dir_all(&building)  // wait 過程 building 已隨 rename 一起被消化
              重新 try_lock_exclusive  // 必成功

3. Invalidate matching L1 sessions（順序在 L2 drop 之前）
   invalidate_matching_l1(&repo_root, &sha_hex)?
   // 詳見 §4.3

4. Drop existing L2
   if commit_dir.exists():
       rm -rf commit_dir
   // building dir 已存在（step 2 mkdir），其內容是空的；下面 step 5 寫入

5. Continue 正常 build_l2 path（src resolution → analyzer → meta → fsync → atomic rename → repo_meta update）

6. Release lock
   return BuildResult { sha_hex, source_type, rebuilt: true }
```

### 4.3 Selective L1 invalidation

```rust
fn invalidate_matching_l1(
    repo_root: &Path,
    target_sha: &str,
) -> io::Result<InvalidateReport> {
    let sessions_dir = repo_root.join("sessions");
    if !sessions_dir.exists() { return Ok(InvalidateReport::default()); }

    let sha8 = &target_sha[..8];
    let mut report = InvalidateReport::default();

    for entry in sessions_dir.read_dir()? {
        let sid_dir = entry?.path();
        let name = sid_dir.file_name().unwrap().to_string_lossy();
        if !sid_dir.is_dir() || name.starts_with('.') || name.contains(".stale-") {
            continue;
        }
        let sid = name.into_owned();

        match SessionState::classify(repo_root, &sid) {
            SessionState::PureReference { base_sha, .. }
                if base_sha == target_sha =>
            {
                report.kept += 1;
                // 留著，不動
            }
            SessionState::AugmentedReference { base_sha, .. }
                if base_sha == target_sha =>
            {
                let stale_path = sessions_dir.join(format!("{sid}.stale-{sha8}"));
                fs::rename(&sid_dir, &stale_path)?;
                spawn_delayed_rm_rf(&stale_path, Duration::from_secs(2));
                report.invalidated += 1;
            }
            SessionState::Stale { reason } if matches_sha_hint(repo_root, &sid, target_sha) => {
                // 僅當該 Stale session 的原 base_sha 對應到 target 才計入 —
                // 否則 unrelated repos / SHA 的 stale session 會混入 report，
                // 對使用者形成噪音。matches_sha_hint 讀 raw session_meta；
                // 讀失敗時保守地視為 in-scope（Err → true）。
                tracing::warn!("stale session {sid} for sha={sha8}: {reason:?}");
                report.stale_skipped += 1;
            }
            _ => {}  // base_sha 不匹配 / Stale 但非此 SHA，略過
        }
    }
    Ok(report)
}
```

**錯誤態保守處理**：當 `DirtyFiles::try_read` 損毀 → `classify` 回 `Stale { DirtyFilesCorrupt }` → 不在 invalidate 範圍（state 不是 Augmented）。但這個 session 本來就無法服務 query，GC 該清。

### 4.4 為何順序是「L1 先、L2 後」

中途 crash 三種：

| Crash 時機 | 觀察狀態 | 下次 query 行為 |
|---|---|---|
| step 3 中（部分 L1 已 rename .stale） | L2 還在 + 部分 L1 stale | 未 rename 的 PureReference / Augmented 仍可用；stale 那批等 GC；無 silent inconsistency |
| step 4 後、step 5 前（L2 dropped、building 空） | L2 不存在 | auto_ensure 偵測缺 L2 → 自動 trigger build；無 stale serve |
| step 5 中（analyzer 跑一半） | L2 不存在（building 不算）| 同上 |

反向順序「L2 先、L1 後」會在 step 4 後、step 5 完成前，L2 換上新版但 L1 仍 hold 舊 UID schema — 雖然 fragments 走 lazy load，但每個 fragment 都是潛在 silent corrupt。先 L1 後 L2 把 inconsistent window 縮到無。

### 4.5 Attach interaction

當 step 2 的 `try_lock_exclusive` 失敗：

- **`--force` 模式**：走 `wait_for_completion` 等對方 publish。對方建好後我們把那個 dir drop + rebuild。對方的工作沒浪費 — 它替我們確認了「目前 L2 可被合法 publish」這個基線；我們再蓋上自己的版本。
- **無 `--force` 模式 + L2 不存在**：走現況 attach pattern，wait_for_completion 完直接 return 對方建好的 dir。

不嘗試強搶鎖 / abort 對方 — fs2 advisory lock 沒有強搶語意，硬 rm building 會 race。

## 5. Query Hot-Path Update

### 5.1 Engine::open 分類分支

```rust
// crates/graph-nexus-cli/src/engine.rs
pub enum GraphView<'a> {
    L2Only(&'a ArchivedZeroCopyGraph),
    L2WithOverlay(&'a ArchivedZeroCopyGraph, OverlayMerged),
}

impl Engine {
    pub fn open(repo_root: &Path, sid: &str) -> io::Result<Self> {
        match SessionState::classify(repo_root, sid) {
            SessionState::PureReference { l2_dirname, .. } => {
                let l2_dir = repo_root.join("commits").join(&l2_dirname);
                let graph = mmap_l2(&l2_dir)?;
                let tantivy = TantivyEngine::open(&l2_dir)?;
                Ok(Engine { view: GraphView::L2Only(graph), tantivy })
            }
            SessionState::AugmentedReference { l2_dirname, .. } => {
                let l2_dir = repo_root.join("commits").join(&l2_dirname);
                let graph = mmap_l2(&l2_dir)?;
                let overlay = OverlayMerged::load(
                    repo_root.join("sessions").join(sid),
                )?;
                let tantivy = TantivyEngine::open_multi(&l2_dir, &overlay)?;
                Ok(Engine { view: GraphView::L2WithOverlay(graph, overlay), tantivy })
            }
            SessionState::Stale { reason } => {
                Err(io::Error::other(format!(
                    "session stale: {reason:?}; remove via `gnx admin sessions reset <id>`"
                )))
            }
        }
    }
}
```

### 5.2 PureReference 不讀 graph_overlay

`L2Only` 分支 **不開啟、不 stat、不 list** `sessions/<sid>/graph_overlay/`。這條由 invariant F5 enforce + test 監控（§7）。

### 5.3 AugmentedReference 與 P2 接面

`OverlayMerged::load` 在本 spec 階段仍是 P2-pre 的 stub（讀檔但不真 merge，行為等同 L2-only）。Spec 文字明寫「Augmented 路徑在 P2 完成前 等同 L2 view，編輯中的 dirty 查詢不反映」。本 spec 不修這個 — `SessionState` enum 已就位，P2 完成時只動 `OverlayMerged::load` 內部，不需再改 Engine 分支。

## 6. Admin Sessions List Display

### 6.1 STATE 欄

```
$ gnx admin sessions list
SESSION                  REPO                BASE_SHA  STATE              LAST_TOUCHED
claude-tab-A             myrepo__a1b2c3d4    abc12345  PureReference      2s ago
claude-tab-B             myrepo__a1b2c3d4    abc12345  Augmented (3)      now
cline-edit               otherrepo__f1e2d3c4 def67890  PureReference      40m ago
broken                   myrepo__a1b2c3d4    --------  Stale(dirty_corr)  5m ago
```

`Augmented (N)` 的 N = `fragment_count`。`Stale(<reason>)` reason 顯示縮寫（meta / dirty_corr / l2_missing / orphan）。

### 6.2 JSON output

`gnx admin sessions list --json`（若有）回傳：

```json
[{
  "session_id": "claude-tab-A",
  "repo": "myrepo__a1b2c3d4",
  "base_sha": "abc12345...",
  "state": { "kind": "pure_reference", "l2_dirname": "branch_main__abc..." },
  "last_touched": "2026-05-17T14:32:11Z"
}, {
  "session_id": "claude-tab-B",
  "state": { "kind": "augmented_reference", "fragment_count": 3, "l2_dirname": "..." },
  ...
}]
```

## 7. CLI Surface Changes

### 7.1 `IndexArgs` 修改

```rust
#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    #[arg(long)]
    pub repo: String,

    /// Force-rebuild L2 at the target SHA. Drops the existing L2 dir
    /// and any orphan `.building/`, invalidates L1 sessions that have
    /// overlays for this SHA (clean sessions kept), then rebuilds.
    /// Use after analyzer/grammar upgrade or to recover from L2
    /// corruption. Without --force, an existing L2 is reused.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Optional path to write a JSONL dump of every resolver decision.
    #[arg(long)]
    pub dump_resolver: Option<std::path::PathBuf>,

    #[arg(skip)]
    pub quiet: bool,
}
```

刪除：`no_cache`, `embeddings`, `drop_embeddings` 三欄 + 對應 warn-no-op 區塊（`admin/index.rs:303–309`）。

### 7.2 `gnx admin index` 行為

```rust
pub fn run(args: IndexArgs) -> Result<(), String> {
    let worktree = PathBuf::from(&args.repo);
    if !worktree.exists() {
        return Err(format!("repo path does not exist: {}", worktree.display()));
    }

    let start = Instant::now();
    let sha = head_sha_hex(&worktree)?;
    let repo_root = resolve_repo_root(&worktree)?;
    let commit_dir = locate_commit_dir(&repo_root, &sha);  // Option<PathBuf>

    match (args.force, commit_dir) {
        (false, Some(existing)) => {
            if !args.quiet {
                eprintln!(
                    "l2.exists sha={} type={:?} elapsed={:.2}s (use --force to rebuild)",
                    &sha[..8],
                    detect_source_type(&existing),
                    start.elapsed().as_secs_f32(),
                );
            }
            Ok(())
        }
        (false, None) => {
            let r = build_l2(&worktree, None).map_err(|e| format!("build_l2 failed: {e}"))?;
            if !args.quiet {
                eprintln!("l2.built sha={} type={:?} elapsed={:.2}s",
                          &r.sha_hex[..8], r.source_type, start.elapsed().as_secs_f32());
            }
            Ok(())
        }
        (true, _) => {
            let r = force_rebuild_l2(&worktree, &sha)
                .map_err(|e| format!("force rebuild failed: {e}"))?;
            if !args.quiet {
                eprintln!("l2.rebuilt sha={} type={:?} elapsed={:.2}s l1_kept={} l1_invalidated={}",
                          &r.sha_hex[..8], r.source_type,
                          start.elapsed().as_secs_f32(),
                          r.invalidate_report.kept,
                          r.invalidate_report.invalidated);
            }
            Ok(())
        }
    }
}
```

### 7.3 Exit codes

| Code | 場景 |
|---|---|
| 0 | build / silent skip / force rebuild 成功 |
| 1 | build / rebuild 失敗（analyzer error、disk full、worktree invalid）|
| 2 | `wait_for_completion` timeout（attach 等對方超過 deadline）|
| 2 | clap 拒絕 unknown flag（`--no-cache` 等）— clap 預設 exit 2 |

## 8. Concurrency & Error Handling

### 8.1 並發場景

| Scenario | 處理 |
|---|---|
| Process A `--force` + Process B `--force` 同 SHA | 先 lock 者跑全套；後者 wait_for_completion + 再 drop+rebuild。最終 commit_dir 是後者建的。前者工作沒浪費（替後者「觀察到一個合法 publish」）|
| Process A `--force` + Process B query 同 SHA | B query 在 A drop L2 後（step 4 後、step 5 完成前）落地：B 偵測缺 L2 → 自動 trigger build（在 A 仍持鎖時走 wait_for_completion）。B 等 A 完，拿到 A 新建的 L2 |
| Process A `--force` + Process B 持 L1 augmented session 同 base_sha | A step 3 把 B 的 session rename `.stale-<sha8>`。B 下次 query 偵測 session 消失 → auto_ensure 重 init 新 session（dirty_files 從 worktree walk 重建）|

### 8.2 Error paths

| 失敗點 | 行為 |
|---|---|
| step 2 `wait_for_completion` timeout | return `Err(io::Error::other("build attach timeout"))` → exit 2 |
| step 3 中部分 rename .stale 後 fs error | 已 renamed 的留著（GC 會清）；error 上回 → 不繼續 step 4 → L2 保持原狀 |
| step 4 `rm -rf commit_dir` 失敗（permission） | error 上回 → L2 留著；已 invalidated 的 L1 session 是 unfortunate 副作用，user 重 query 會自動 rebuild L1 |
| step 5 build 失敗 | building dir 留著（atomic rename 還沒發生）→ 下次 GC sweep 走 v2 spec §10 既有機制（`*.building` 名稱 + 無 active lock holder ⇒ orphan）清；commit_dir 不存在（已 dropped）→ user 重跑 query / admin index 自動 rebuild |

## 9. Invariants

| # | Invariant | Test ref |
|---|---|---|
| F1 | `SessionState::classify` 是 dirty_files + session_meta + l2_commits 的 pure function | §10 unit |
| F2 | `--force` 完成 ⇒ `commits/<dirname>/` 是新 build（mtime 大於 force 開始時間）| §10 integration |
| F3 | `--force` 完成 ⇒ 該 repo sessions/ 下，`AugmentedReference + base_sha == target` 的 session 都不存在於原名 dir（已 rename `.stale-*` 或已 GC）| §10 integration |
| F4 | `--force` 完成 ⇒ 該 repo sessions/ 下，`PureReference + base_sha == target` 的 session 仍存於原名 dir、base_sha 不變 | §10 integration |
| F5 | Query hot-path 走 `GraphView::L2Only` 時，不開 / 不 stat / 不 list `sessions/<sid>/graph_overlay/` | §10 lsof/strace test |
| F6 | `--force` 過程任意點 crash ⇒ 下次 query 可達 self-consistent state（缺 L2 觸發 build；缺 L1 觸發 init） | §10 crash injection |
| F7 | 不帶 `--force` + L2 已存在 ⇒ admin index 無 fs mutation（commit_dir mtime / 內容不變）| §10 integration |

## 10. Test Plan

### 10.1 Unit / schema

- `SessionState::classify` happy: PureReference, AugmentedReference, Stale 各 1
- `SessionState::classify` edges:
  - `dirty_files.json` 不存在 → PureReference
  - `dirty_files.json` 內容損毀 → `Stale(DirtyFilesCorrupt)`
  - `session_meta.json` 不存在 → `Stale(MetaUnreadable)`
  - L2 對應 SHA 不存在 → `Stale(L2Missing)`
  - `dirty_files.entries.is_empty()` 但 `graph_overlay/` 有檔（orphan）→ PureReference（orphan 是 GC scope）

### 10.2 Integration — `admin index`

- L2 不存在 + 無 --force → 正常 build；exit 0；`l2.built` printed
- L2 存在 + 無 --force → silent skip；exit 0；`l2.exists ... (use --force to rebuild)` printed；commit_dir mtime/inode 不變 (F7)
- L2 存在 + --force → drop + rebuild；commit_dir 新 mtime；新 build_meta.json.built_at 是 now (F2)
- L2 不存在 + --force → 正常 build（等價 no-force build 但 audit log 有 force）
- Orphan `.building/` 留下 → --force 一併清除

### 10.3 Integration — L1 invalidate

- `--force` + base_sha == target 的 PureReference session → session 保留 (F4)
- `--force` + base_sha == target 的 Augmented session → session rename `.stale-<sha8>`；2.5s 後不存在 (F3)
- `--force` + base_sha == target 的 dirty_files 損毀 session → 不動（state = Stale）；recovery 走 `gnx admin sessions reset <id>`
- `--force` + base_sha != target 的 任意 state session → 不動

### 10.4 Integration — concurrent --force

- 兩 process 同 `--force` 同 SHA 同 worktree → 後到者走 wait_for_completion → 兩者皆 exit 0 → 最終 commit_dir 唯一 + 內容是後到者的 build
- Process A `--force` + Process B query 同 SHA → B 在 A drop L2 後落地 query：B 等 A → 拿 A 新建的 L2 答 query → A B 都 exit 0

### 10.5 Integration — hot-path

- PureReference query `gnx inspect <sym>` → 結果只反映 L2 內容
- PureReference query → 監控 fd open list 不含 `graph_overlay/` 路徑 (F5)
- Augmented query `gnx inspect <sym>` → 結果 = L2 fallback（P2 完成前 stub 行為；明寫 expected = L2-only result + 標 ignored-pending-P2 if needed）

### 10.6 CLI surface

- `gnx admin index --repo X --no-cache` → clap reject → exit 2
- `gnx admin index --repo X --embeddings` → clap reject → exit 2
- `gnx admin index --repo X --drop-embeddings` → clap reject → exit 2
- `gnx admin sessions list` 輸出含 STATE 欄、值 ∈ {`PureReference`, `Augmented (N)`, `Stale(<reason>)`}
- `gnx admin sessions list --json` 每筆有 `state.kind ∈ {pure_reference, augmented_reference, stale}`

### 10.7 Crash injection

模擬 step 3 中 / step 4 後 / step 5 中 crash（測試 helper kill subprocess）→ 下次 query 自動 recover：
- step 3 中：部分 L1 stale + L2 完整 → 未 stale 的 session 可繼續 query；stale 的 session id 觸發 fresh init
- step 4 後 + step 5 前：L2 缺 → query 自動 build_l2
- step 5 中：building 留著 + commit_dir 缺 → 下次 GC 清 building + query 自動 build_l2

## 11. Out of scope

- AugmentedReference overlay merge 實作（屬 P2）
- `admin index --rev <sha>`（後續一併處理）
- `gnx admin force` 獨立命令（flag 足夠）
- 強搶 build lock / abort 對方 builder
- `--no-cache` deprecation alias（直接刪）

## 12. File-Level Change Inventory

### 新增

- `crates/graph-nexus-core/src/session/state.rs` — `SessionState` enum + `StaleReason`（pure types）
- `crates/graph-nexus-cli/src/session/state.rs` — `classify()` function（needs `commit_lookup::CommitIndex`，故落 cli 而非 core）
- `crates/graph-nexus-cli/src/build/force.rs` — `force_rebuild_l2` + `invalidate_matching_l1` + `InvalidateReport`（共用 build_l2 internal helpers）
- `crates/graph-nexus-cli/src/commands/admin/sessions.rs` — `admin sessions list` subcommand（minimal: list only；reset/sweep 為 parent spec §11.2 後續）

### 修改

- `crates/graph-nexus-core/src/session/mod.rs` — re-export `SessionState` + `StaleReason`
- `crates/graph-nexus-cli/src/session/mod.rs` — re-export `state::classify`
- `crates/graph-nexus-cli/src/commands/admin/mod.rs` — `AdminCommands` 加 `Sessions` variant
- `crates/graph-nexus-cli/src/commands/admin/index.rs` — 刪除 `no_cache / embeddings / drop_embeddings` 欄 + warn-no-op 區塊；`run` 改 match (force, commit_dir) 三分支
- `crates/graph-nexus-cli/src/engine.rs` — `Engine::open` 改走 `SessionState::classify` + `GraphView` enum dispatch
- `crates/graph-nexus-cli/src/build/orchestrator.rs` — `build_l2` 簽名不變；`force_rebuild_l2` 共用 internal helpers (source resolution, analyzer, atomic publish)
- `crates/graph-nexus-cli/src/build/mod.rs` — re-export `force_rebuild_l2`

### 刪除（程式碼塊）

- `admin/index.rs:303–309` warn block
- `IndexArgs::no_cache / embeddings / drop_embeddings`

### 預估 LOC

del ~50（warn block + 三個 flag + 對應 doc comment）、add ~300（SessionState module ~80、force_rebuild ~80、Engine::open dispatch ~50、sessions list STATE ~30、tests ~60）。Net **+250 LOC**。
