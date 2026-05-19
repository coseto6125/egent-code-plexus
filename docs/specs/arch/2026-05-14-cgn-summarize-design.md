# `cgn coverage --detailed` — LLM-friendly Project Overview

**Status**: design
**Date**: 2026-05-14
**Related**: Issue #1558 (parent gitnexus Node.js wiki request)

## Goal

加一個 `cgn coverage --detailed` 選項，從已建好的 `graph.bin` 產出**LLM-friendly markdown 摘要**，供 coding agent 直接 paste 進 prompt 以快速理解專案結構。**不**重新做傳統 HTML wiki ── 對 token efficiency 與 LLM 閱讀路徑最佳化。

## Non-Goals

- 不重新 `admin index`；要求 `graph.bin` 已存在
- 不產生靜態網頁 / HTML
- 不做交互式探索（CLI 一次性輸出）
- 不取代 `cgn inspect` / `cgn search`（那些是針對單一 symbol 的精準查詢，這個是「整體鳥瞰」）

## Output Strategy: D = 分層 + 去 noise + 同名消歧

POC 比過 A/B/C 三版（樣本：359 檔 / 2792 symbols 的 Python repo）：

| 變體 | 結論 |
|---|---|
| A: 1 sym/file | 大量 generic 同名 (`verify_act` × N) 失效 |
| B: top-5/file | 359 個 `##` section + in_deg=0 noise 過多（~18K tokens） |
| C: community-grouped | 同 community 重名嚴重；60 個 community 過散（~3.4K tokens 但實用性低） |

最終採 **D**：

### 分層輸出結構

```markdown
# Project Summary

Repo: <name>  •  Branch: <branch>  •  Files: <N>  •  Symbols: <M>

## Top hot files
1. `verify_method/public_methods.py` — 12 symbols, 503 in_deg
2. `module/assert_func.py` — 1 symbol (assert_func, 4437 in_deg)
...

## Architecture
**Communities** (top-10 by symbol count):
- Community 3 (593 symbols, anchor: `verify_method/public_methods.py`)
- Community 7 (307 symbols, anchor: `verify_method/game_logic/*`)
...

## Per-file detail
### `module/assert_func.py`  [community 3]
- Function `assert_func` (in_deg=4437) ← hottest

### `verify_method/public_methods.py`  [community 3]
- Class `PokerGames` (in_deg=42)
- Class `PublicVerify` (in_deg=38)
- Function `verify_act` (in_deg=29)  ← shadowed by 230 same-name funcs

_… (truncated; 349 more files)_
```

### 過濾 / 排序規則

1. **noise 過濾**：跳過 `in_deg == 0 && out_deg == 0` 的孤兒 symbol（典型 fixture / temp helper）
2. **file 排序**：以 file-aggregated `in_deg` 降序
3. **per-file symbol 排序**：`in_deg` 降序、平手以 alphabetical name
4. **同名消歧**：若同 name 在 ≥2 處出現，附加 `← shadowed by N same-name funcs` 提示
5. **截斷策略**：
   - **Top hot files**: 預設 top-10 (`--top-files`)
   - **Per-file detail**: 預設 top-10 file × top-3 symbols/file，剩餘以 `_… (truncated; N more)_` 收尾
   - **Communities**: 預設 top-10 (`--top-communities`)

### CLI 介面

```
cgn coverage --detailed [OPTIONS]

Options:
  --repo <NAME>              多 repo 場景必填；單 repo 自動偵測
  --top-files <N>            top hot files 數量 (default 10)
  --top-communities <N>      top communities 數量 (default 10)
  --top-symbols <N>          per-file symbol 數量 (default 3)
  --format <FMT>             md (default) | json
  --output <FILE>            寫入檔案 (default stdout)
  --include-orphans          保留 in_deg=0 孤兒（debug 用）
  --graph <PATH>             顯式指定 graph.bin（預設由 --repo 解析）
```

`--repo` 缺失且 registry 有 ≥2 repo 時，回傳清楚錯誤 + 列出可用 repo（呼應 Issue #1542 的多 repo 教訓）。

## Architecture

### 模組劃分

```
crates/cgn-cli/src/commands/
├── summarize.rs        # CLI args + run() + orchestration
└── summarize/
    ├── mod.rs          # 內部 trait & 共用型別
    ├── analysis.rs     # in_deg / file aggregation / community grouping
    ├── ranking.rs      # 排序、截斷、同名偵測
    └── render.rs       # markdown / json 輸出
```

### 資料流

```
graph.bin (mmap)
  ↓
analysis::degree_stats()    → Vec<(node_idx, in_deg, out_deg)>
analysis::by_file()         → BTreeMap<file_idx, Vec<node_idx>>
analysis::by_community()    → BTreeMap<community_id, Vec<node_idx>>
analysis::name_collisions() → HashMap<name, Vec<node_idx>>
  ↓
ranking::top_files(top_files)
ranking::top_communities(top_communities)
ranking::top_symbols_per_file(top_symbols, exclude_orphans=!include_orphans)
  ↓
render::markdown(&Summary) | render::json(&Summary)
  ↓
stdout / file
```

### 錯誤處理

- `graph.bin` 不存在 → 提示先跑 `cgn admin index`
- 多 repo 缺 `--repo` → 列出可選 repo
- `--top-*` 為 0 → 解釋成「不輸出該 section」
- 空 graph (0 nodes) → 輸出最小骨架 + 警告

## Testing

### 單元測試

- `analysis::degree_stats`: 已知 edges 算出正確 in/out_deg
- `analysis::name_collisions`: 同名集合正確
- `ranking::top_files`: 排序 + 截斷
- `ranking::dedupe_orphans`: 空 graph、全 orphan、混合

### Integration 測試

- 對 `tests/fixtures/summarize_sample/` 中一個小 graph.bin 跑 `cgn coverage --detailed`，snapshot 比對輸出
- 確認 `--top-files 0` 行為
- 確認 multi-repo 缺 `--repo` 提示符合 Issue #1542 期待

### 效能

graph.bin mmap + 一次線性掃 nodes + edges 即可，O(N+E)。對 POC repo（2792 nodes / ~tens of thousands edges）應 <50ms。CI 跑 nextest 內含 timing 報表。

## Out of Scope (defer to follow-up)

- TOON 格式輸出（先 markdown / json 兩個）
- 自動寫到 `CLAUDE.md` / `AGENTS.md`（item #1 防雷的功能）── 等本指令穩定後另案
- 跨 repo 摘要 (`--repo @group`) — *Status (2026-05-18, PR #146): 改由 `cgn group coverage <name>` 提供；top-level `--repo @<group>` 拒收。*
- 含 embeddings semantic clustering 取代 Louvain community

## Acceptance Criteria

1. `cgn coverage --detailed` 對 359 檔 / 2792 symbols 的 Python repo 輸出 < 4K tokens（D 變體已 POC 驗）
2. 多 repo 缺 `--repo` 時 error 訊息正確列出 available repo
3. CI 全綠：fmt / clippy `-D warnings` / nextest 含 summarize 單元/整合測試
4. README 加 install / usage 短例
