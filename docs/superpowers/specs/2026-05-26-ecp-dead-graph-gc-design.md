# `~/.ecp` 殭屍圖世代清理修復（dead-graph GC）

**Date**: 2026-05-26
**Follow-up**: FU-2026-05-26-001（`~/.ecp` 圖快取洩漏，M）
**Surfaced**: 開發 CompensatedBy（FU-008）時實機死當，診斷 `~/.ecp` 發現 16G 快取中 13G 為殭屍圖

## 問題（已查證，非推論）

`~/.ecp` 圖快取累積至 16G，其中 13G 為從不被清理的殭屍目錄。三層清理機制全部漏失：

| 層級 | 殭屍類型 | 漏因 | 佔比 |
|------|---------|------|------|
| L1 | `~/.ecp/<repo>__<hash>.dead.<pid>.<n>.<ts>` | `fs_safe::retire_dir_async` 的 detached thread `let _ = remove_dir_all` 靜默失敗；短命 CLI 進程退出即殺掉 thread，刪除半途中止無痕跡 | 少量 |
| L2 | `<repo>/commits/branch_X__<SHA>.gen.<ts>.<pid>.0/graph.bin` | **無任何清理者**。同一 SHA 被重複 ingest 產生多份 `.gen` 世代（實測 sample_repo 單一 SHA 累積 25 份 63MB 圖），舊世代從不收斂 | **主因** |
| L3 | `<repo>/sessions/*.dead` | `gc::sweep_sessions` 有刪除邏輯，但 `admin gc` 子命令 `gc.rs:4` 標記 "isn't wired yet"，無觸發點 | 少量 |

**佐證**：`~/.ecp/last-prune.log` 僅含 `=== attempt 1/1 ===`；marker `.prune-complete` 存在（今晚 prune 成功）但 13G 未減 → 證實現有 `prune --orphans` 範圍不含上述三層（它只清「common_dir 不存在」的 orphan repo，不掃活 repo 內的退役/重複世代）。

## 目標

讓背景清理收斂三層殭屍，採**分離「檢查」與「清理」的併發模型**，最輕量、避免衝突、確保任務完成。不新增觸發點——複用 `session_start` 既有的 detached + flock 背景 prune 機制。

## 架構

```
session_start hook
  └─ spawn_bg (detached process + flock .prune.lock，跑完才釋放鎖)
       ├─ ecp admin prune --orphans   ← 既有，不動
       └─ ecp admin gc                ← 新接線
            ├─ Phase 1 檢查（rayon 併發，唯讀 readdir + stat，產待刪清單）
            │    ├─ L2: 每個 SHA 分組，非最新 mtime 的 .gen 世代
            │    ├─ L1: 殘留 <repo>.dead.*
            │    └─ L3: sessions/*.dead（複用 sweep_sessions）
            │    └─ 排除：mtime < 10s 或同名有 .building/ 的世代（正在寫）
            └─ Phase 2 清理（串行，低優先 nice，逐一 remove_dir_all）
                 └─ 失敗 → 記入 failed count，最終非零寫 .prune-failed marker（不靜默）
```

### 併發模型要求落實

- **分離檢查與清理**：Phase 1 唯讀掃描用 rayon 併發（只 readdir + stat，不讀圖內容，輕）；Phase 2 刪除串行。
- **最輕量**：檢查不讀圖內容；清理串行 + `nice` 降優先，避免重演 WSL2 vhdx 併發 I/O 風暴（本次死當主因）。
- **避免衝突**：`flock(.prune.lock)` 序列化——多 session 同時觸發時 losers 直接 exit 0 不重跑；檢查階段排除 mtime<10s / 有 `.building/` 的世代，不刪別的 session 正在 ingest 的圖。
- **確保完成**：清理跑在 detached **進程**（非 in-process thread），flock 包住直到跑完才釋放；失敗寫 `.prune-failed` marker，不再 `let _ =` 靜默吞錯。

## 元件

### 新增 `gc::sweep_stale_generations(repo_root: &Path) -> SweepStats`（gc.rs）

- 掃 `<repo>/commits/`，**重用既有 `registry::CommitDirName::parse(name)`** 解析每個目錄名為 `{ sha: [u8;20], generation: Option<Generation> }`（不手寫字串切割——DRY）。
- 同 `sha` 分組，組內用 `Generation` 既有的 `Ord` 取最大者保留（doc 保證：base dir `None < Some(_)`；generations 間按 `timestamp_ms.pid.counter` lex 排序）。其餘加入待刪清單。
- 排除：mtime 在 10s 內，或同 commit 鍵存在 `.building/` 目錄者（避免刪別的 session 正在 ingest 的圖）。
- 保留策略：**同 SHA 只留 `Generation` 最大一份**（同 SHA → 冪等同圖，舊世代零價值）。比 mtime 比較更可靠——`Generation` Ord 本就是 producer 的確定性排序鍵。

### 新增 `gc::sweep_retired_repos(home_ecp: &Path) -> SweepStats`（gc.rs）

- 掃 `~/.ecp/` 頂層 `*.dead.*` repo 退役目錄，全加入待刪（已是 retire 標記的死目錄）。

### 修 `fs_safe::retire_dir_async`（fs_safe.rs）

- detached thread 的 `let _ = fs::remove_dir_all(retired_path)` 改為失敗時 `eprintln!` 記錄。
- WHY 註解：背景退役刪除失敗由 `admin gc` 兜底，仍記 stderr 供診斷（先前靜默吞錯是 L1 洩漏根因）。

### 接線 `admin gc` 子命令（main.rs + commands/admin/）

- 移除 `gc.rs` 的 `#![allow(dead_code)]`。
- 串接執行序：`sweep_stale_generations` + `sweep_retired_repos` + `sweep_sessions` + 既有 reachability/quota evict。
- Phase 2 串行刪除，背景進程以 `nice` 降優先。

### 接線背景觸發（session_start.rs）

- 在**同一個** detached 背景 job 內、**同一把 `.prune.lock` 持有期間**，於 `prune --orphans` 之後串接 `ecp admin gc`，依序執行（非兩個各自搶鎖的獨立 spawn——那會讓 gc 永遠搶不到鎖）。
- 實作上：擴充現有 `spawn_orphan_prune` 的 `BgJob.args` 為一個跑 `prune` 後接 `gc` 的複合命令，或新增 `spawn_prune_and_gc` 用單一 flock 包住兩步。沿用既有 marker（`.prune-complete` / `.prune-failed`）。

## 錯誤處理

- 每個 `remove_dir_all` 失敗：記入 `SweepStats.failed`，`continue`（不中止整批）。
- 整批結束後 `failed > 0` → 寫 `.prune-failed` marker（UserPromptSubmit 消費，浮現給使用者）。
- flock 搶不到（別的 session 在跑）→ exit 0，不重跑。

## 測試（registry/fs 層，14-lang 不適用）

| 測試 | 驗證 |
|------|------|
| `sweep_stale_generations_keeps_newest_per_sha` | 3 個同 SHA `.gen` + 1 個不同 SHA → 只刪同 SHA 較舊的 2 個，不同 SHA 與最新份保留 |
| `sweep_stale_generations_skips_building` | 同 commit 鍵存在 `.building/` → 該組不刪 |
| `sweep_stale_generations_skips_fresh` | mtime < 10s 的世代不刪 |
| `sweep_retired_repos_removes_dead` | 頂層 `<repo>.dead.*` 被刪 |
| 既有 `tests/gc.rs` | sweep_sessions / reachability / quota evict 維持綠 |

## 範圍邊界（YAGNI）

- **不改 ingest 寫圖路徑**：不在 retire/ingest 熱路徑加同步清理（那是本次 I/O 風暴的反方向）。
- **不加併發刪除**：Phase 2 刻意串行 + nice，安全優先於速度（清理是背景任務，不趕時間）。
- **不改 `prune --orphans` 語意**：它繼續只清 orphan repo；gc 補位活 repo 內的退役/重複世代。
- **不做跨 repo quota 全域協調**：沿用 gc.rs 既有 per-repo `DEFAULT_QUOTA_BYTES`。
