# Index Layout Redesign — Two-Tier (L2 commit-content-addressed + L1 session-local)

| Field | Value |
|---|---|
| Status | Draft |
| Date | 2026-05-17 |
| Scope | `~/.cgn/` filesystem layout, registry schema, read/write paths, GC, CLI surface |
| Affects | `cgn-core::registry`, `cgn-cli::{graph_path, repo_selector, auto_ensure, engine, commands::admin::*, commands::hook::*}` |
| Migration | None — `~/.cgn/` schema v1 拒讀；`cgn admin reset` wipe + auto-rebuild |
| Replaces | 現況 `~/.cgn/<repo>/<branch>/{graph.bin, tantivy/, meta.json, incremental_cache.bin}` 樹狀結構 |

---

## 1. Motivation

現況 (v1) `~/.cgn/` 把 index 收在 `<repo>/<branch>/` 二層命名空間，造成幾個結構性問題：

1. **同 repo 多 worktree 被當不同 repo** — `IndexLayout::resolve` 用 worktree_path 算 disambiguator hash，導致 `git worktree add` 出來的每份 worktree 都另開 `<repo>-<hash8>/` 目錄、各自重 index。實測 `~/.cgn/` 已累積 15 個 `code-graph-nexus-*` 目錄，多數內容近似。
2. **Branch 切換重來** — branch 是 storage key，切 branch 就是換一整套 `graph.bin + tantivy/ + incremental_cache.bin`。沒有「同 commit 不同 branch 共享」、「相鄰 commit 增量重用」的可能。
3. **Dirty worktree 沒有穩定 identity** — 編輯中的 worktree 既不是 commit X 也不是 commit Y，現況靠 mtime stale 偵測觸發 full reindex，每次 edit 都付重 build 成本。
4. **多 session 並發無模型** — Claude Code + Cline + Cursor MCP 同時操作同 repo，現況只能搶 `<repo>/<branch>/graph.lock` 互鎖，沒辦法各 session 獨立看到自己的 dirty view。
5. **重複實作散落** — `write_atomic` 三個副本（`registry/store.rs`、`registry/meta.rs`、`commands/admin/claude_code.rs`），`sanitize_*` 一堆變體，IndexLayout collision 邏輯只服務 branch-based key。

LLM-first 的核心限制（CLAUDE.md priority order）：per-query latency <30ms、減少 hallucination、output signal density。v1 layout 違反第 1 條（registry read + canonicalize 每查 ~5–10ms）並間接違反第 2 條（stale 偵測偶有漏判，LLM 收到舊結果）。

## 2. 設計原則

| 原則 | 含意 |
|---|---|
| **Commit SHA 是唯一 storage key** | Branch / tag / PR 都是指向 SHA 的可變指標，不入 storage layout。CLI 入口 `git rev-parse <ref>` 翻成 SHA 後查 L2。 |
| **兩階層：L2 immutable + L1 session-local mutable** | L2 = commit-content-addressed canonical；L1 = per-session 動態 working state，incremental update。 |
| **Filesystem 是 source of truth** | `registry.json` 降格為 alias cache，遺失可從 filesystem 重建。不再有「registry 跟硬碟漂移」狀態。 |
| **Reader 永不阻塞** | L2 mmap 共享讀；L1 fragment atomic rename；writer 透明。 |
| **Atomic write 統一一個 helper** | 三個 `write_atomic` 合一到 `registry/io.rs::atomic_write_json`。 |
| **Deterministic serialization** | 落盤資料 (`registry.json` / `repo_meta.json` / `session_meta.json` / `dirty_files.json`) 用 `BTreeMap`，byte-stable across 機器 / runs。`HashMap` 僅限 in-memory hot cache。 |

## 3. Storage Layout

```
~/.cgn/
├── audit.log                                                ← top-level，跨 repo audit trail
├── registry.json                                            ← alias cache（v2），filesystem 為 truth
├── registry.json.lock                                       ← fs2 exclusive
├── .last-gc                                                 ← GC heartbeat（stat-only 觸發判斷）
└── <repo>/                                                  ← e.g. code-graph-nexus-rs__a1b2c3d4/
    ├── meta.json                                            ← per-repo source-of-truth metadata
    ├── .meta.lock                                           ← fs2 exclusive for meta.json mutation
    ├── commits/                                             ── L2 (immutable, commit-content-addressed)
    │   ├── branch_main__abc123def4567890abc123def4567890abc123de/
    │   │   ├── graph.bin
    │   │   ├── tantivy/
    │   │   ├── embeddings.bin                               ← optional
    │   │   ├── meta.json                                    ← CommitBuildMeta
    │   │   └── .build.lock                                  ← fs2 exclusive during build
    │   ├── branch_feat-x__654321abc987def123654321abc987def1236/
    │   ├── tag_v1.2.3__789abc123def456789abc123def456789abc1234/
    │   ├── pr_123__def456789abc123def456789abc123def456789ab/
    │   └── commit__456789abc123def456789abc123def456789abc123/
    └── sessions/                                            ── L1 (mutable, session-local)
        └── <session-id>/
            ├── session_meta.json
            ├── dirty_files.json
            ├── graph_overlay/
            │   └── <fragment_id>.bin                        ← rkyv archive，per dirty 檔
            ├── tantivy_delta/                               ← tantivy 原生 segment dir
            └── (Phase 2 預留：query_memo.lmdb、read_set.bin — 不在此 spec 範圍)
```

### 3.1 `<repo>` 目錄命名

`<repo>` 目錄名規則：`sanitize(basename(common_dir)) + "__" + sha256(canonical_common_dir)[:8]`

例：worktree `~/work/myrepo`，`git -C ~/work/myrepo rev-parse --git-common-dir` 回 `~/work/myrepo/.git`，canonicalize 後算 hash8 = `a1b2c3d4`，目錄名 = `myrepo__a1b2c3d4`。

理由：
- common_dir 對所有 worktrees 同 repo **唯一**（`git worktree add` 出來的 worktrees 共用同一 common_dir），自然解 v1 的「多 worktree 重複 index」問題
- Hash suffix 避免 basename collision（`myrepo` 在多個父路徑都存在）
- Sanitize basename 給人類看，hash 給機器辨識

### 3.2 `commits/<dirname>` 命名

格式：`<source_type>_<source_id>__<full-40-char-sha>`

`<source_type>` closed vocabulary（parser 顯式驗證，不可擴張）：

| type | source_id 範例 | 來源 |
|---|---|---|
| `branch` | `main`, `feat-x`, `develop` | local branch ref |
| `tag` | `v1.2.3`, `release-2026-Q2` | tag ref |
| `pr` | `123`, `456` | PR / MR（GitLab 的 mr/N normalize 成 pr）|
| `commit` | （無 source_id）| detached HEAD、無 ref 指向、直接 SHA 查詢 |

`/` 在 ref 名 → `-`（如 `feat/x` → `feat-x`），其他 fs-unsafe 字元 → `_`。

`__` 雙底線作 sha 分隔 — SHA 是 hex（`[0-9a-f]+`），永遠不會含 `__`；branch / tag / pr id 含 `_` 或 `__` 都不會破壞解析（`rsplit_once("__")` 從右切，sha 段是 40-hex 驗證後才接受）。

### 3.3 Parser 規則

```rust
pub fn parse_commit_dir_name(name: &str) -> Result<CommitDirName, ParseError> {
    // 1. 從右切 "__" 拆出 sha
    let (prefix, sha_str) = name.rsplit_once("__").ok_or(ParseError::NoSha)?;
    if sha_str.len() != 40 || !sha_str.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ParseError::InvalidSha);
    }
    let sha = hex_decode_to_20_bytes(sha_str)?;

    // 2. commit__sha 特例 (無 source_id)
    if prefix == "commit" {
        return Ok(CommitDirName { source_type: SourceType::Commit, source_id: None, sha });
    }

    // 3. 從左切第一個 '_'，type 對 closed vocab
    let (type_str, id_str) = prefix.split_once('_').ok_or(ParseError::NoTypeId)?;
    let source_type = match type_str {
        "branch" => SourceType::Branch,
        "tag" => SourceType::Tag,
        "pr" => SourceType::Pr,
        _ => return Err(ParseError::UnknownSourceType(type_str.into())),
    };
    Ok(CommitDirName { source_type, source_id: Some(id_str.into()), sha })
}
```

### 3.4 同 SHA 多 ref / ref 移動

- 同 SHA 被多個 ref 同時指到 → dir name 取 build 當下**最具體**的 ref（優先序：`branch > tag > pr > commit`）
- Build 完成後新增的 ref（`git tag v2.0`、`pr-456` 開上來）→ **不重命名 dir**，append 進 `meta.json.refs_seen_since`
- 原 ref 移走（`main` 跳到新 SHA）→ **不重命名 dir**，舊 dir 仍是 `branch_main__<old_sha>`，新 SHA build 後新增 `branch_main__<new_sha>`；live ref status 由 `cgn admin list` 跑時 `git for-each-ref` 重算顯示

## 4. Rust Types

### 4.1 RegistryFile (v2) — Alias cache, NOT source of truth

```rust
// crates/cgn-core/src/registry/store.rs
#[derive(Serialize, Deserialize)]
pub struct RegistryFile {
    pub version: u32,                              // 2
    pub repos: BTreeMap<String, RepoAlias>,        // dir_name → alias
    pub groups: Vec<GroupEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct RepoAlias {
    pub dir_name: String,                          // "<basename>__<hash8>"
    pub common_dir: String,                        // canonical abs path; identity 主鍵
    pub remote_url: Option<String>,
    pub aliases: Vec<String>,                      // 使用者 --repo @alias
    pub last_touched: String,                      // RFC3339
    pub groups: Vec<String>,                       // 冷資料，保留 Vec
}

#[derive(Serialize, Deserialize)]
pub struct GroupEntry {
    pub name: String,
    pub members: Vec<String>,                      // dir_name 列表
}
```

**舊 v1 的 `RepoEntry` / `BranchEntry` 整個刪除**。

### 4.2 RepoMeta — Per-repo source of truth

```rust
// crates/cgn-core/src/registry/repo_meta.rs (new)
// 落地位置：~/.cgn/<repo>/meta.json
#[derive(Serialize, Deserialize)]
pub struct RepoMeta {
    pub version: u32,                              // 1
    pub common_dir: String,
    pub remote_url: Option<String>,
    pub aliases: Vec<String>,
    pub known_refs: BTreeMap<String, String>,      // ref → sha (cache;權威仍是 git rev-parse)
    pub last_built_sha: Option<String>,
    pub total_size_bytes: u64,
    pub last_touched: String,
}
```

### 4.3 CommitBuildMeta — Per-commit build metadata

```rust
// crates/cgn-core/src/registry/commit_meta.rs (rename from meta.rs::BranchMeta)
// 落地位置：~/.cgn/<repo>/commits/<dirname>/meta.json
#[derive(Serialize, Deserialize)]
pub struct CommitBuildMeta {
    pub version: u32,                              // 1
    pub sha: String,                               // 40-char hex
    pub source_type: SourceType,
    pub source_id: Option<String>,
    pub built_from_worktree: String,
    pub built_at: String,
    pub parent_sha: Option<String>,
    pub node_count: u32,
    pub embedding_status: EmbeddingStatus,
    pub refs_at_build: Vec<RefRecord>,             // build 當下所有指向此 SHA 的 ref
    pub refs_seen_since: Vec<RefRecord>,           // build 後新指過此 SHA 的 ref (append-only)
}

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Eq)]
pub enum SourceType { Branch, Tag, Pr, Commit }

#[derive(Serialize, Deserialize)]
pub struct RefRecord {
    pub ref_name: String,                          // full ref ("refs/heads/main", "refs/tags/v1.2.3")
    pub seen_at: String,                           // RFC3339
}

#[derive(Serialize, Deserialize)]
pub enum EmbeddingStatus { None, Skipped, Computed }
```

### 4.4 CommitDirName — In-memory parsed form

```rust
// crates/cgn-core/src/registry/dirname.rs (new)
pub struct CommitDirName {
    pub source_type: SourceType,
    pub source_id: Option<String>,
    pub sha: [u8; 20],                             // raw bytes for cheap compare/hash
}

impl CommitDirName {
    pub fn parse(name: &str) -> Result<Self, ParseError>;
    pub fn format(&self) -> String;                // round-trip
    pub fn sha_hex(&self) -> String;
}
```

### 4.5 SessionMeta — L1 session header

```rust
// crates/cgn-core/src/session/meta.rs (new)
// 落地位置：~/.cgn/<repo>/sessions/<sid>/session_meta.json
#[derive(Serialize, Deserialize)]
pub struct SessionMeta {
    pub version: u32,                              // 1
    pub session_id: String,
    pub pid: Option<u32>,                          // None on NFS / multi-machine
    pub started_at: String,
    pub last_touched: String,                      // heartbeat — 每次 L1 寫入更新
    pub base_sha: String,
    pub source_worktree: String,
    pub overlay_version: u32,                      // 每次 dirty 變動 +1
}
```

### 4.6 DirtyFiles — L1 overlay manifest

```rust
// crates/cgn-core/src/session/overlay.rs (new)
// 落地位置：~/.cgn/<repo>/sessions/<sid>/dirty_files.json
#[derive(Serialize, Deserialize)]
pub struct DirtyFiles {
    pub version: u32,                              // 1
    pub entries: BTreeMap<String, DirtyEntry>,     // relative path from worktree root
}

#[derive(Serialize, Deserialize)]
pub struct DirtyEntry {
    pub mtime_ns: u64,
    pub content_hash: String,                      // sha256 hex of file at parse time
    pub fragment_id: String,                       // graph_overlay/<fragment_id>.bin
    pub tantivy_delta_segment: Option<String>,
    pub parse_failed: bool,                        // 留舊 fragment、標 parse 失敗
}
```

## 5. Read Path

Hot-path 目標：per-query <30ms（CLAUDE.md priority #1）。

```
cgn <cmd>  (cwd = /work/myrepo)
  │
  ├─ 1. Resolve repo (~3–8ms)
  │     A. git -C cwd rev-parse --git-common-dir       ← ~3ms (fork + OS cache hot)
  │     B. canonicalize → common_dir
  │     C. hash8 = sha256(common_dir)[:8]
  │     D. repo_dir_name = sanitize(basename(common_dir)) + "__" + hash8
  │     E. repo_path = ~/.cgn/<repo_dir_name>/
  │        若不存在 → first-time，trigger build (§6, sync mode)
  │
  ├─ 2. Resolve session_id (~0ms)
  │     hook payload → MCP transport → env CLAUDE_CODE_SESSION_ID → fallback "cli-<pid_hash8>"
  │
  ├─ 3. Resolve base_sha (~0–3ms)
  │     A. --rev <ref>?
  │           consult repo_meta.json.known_refs cache 先 → hit 跳 D
  │           miss → git rev-parse <ref>，命中後 update known_refs (async, 不阻塞 query)
  │     B. else → git rev-parse HEAD (~3ms; cache 同上規則)
  │     C. SHA validated (40-hex)
  │     D. base_sha 確定
  │
  ├─ 4. Locate L2 commits dir (~0–2ms)
  │     A. process-local sha→dirname map cached？hit 跳 D
  │        (cache lifetime: CLI 進程整段 / MCP server 至 commits/ mtime 變動才 invalidate)
  │     B. miss → readdir(<repo>/commits/) ← 通常 <50 entries, ~1ms
  │     C. parse each dir name via CommitDirName::parse → build HashMap<[u8;20], String>
  │        parse 失敗的 dir name (壞名 / partial build leftover) → 略過 + log
  │     D. lookup base_sha; not found → trigger build (§6)
  │     E. l2_dir = <repo>/commits/<dirname>/
  │
  ├─ 5. Auto-ensure / L1 sync (~varies)
  │     A. walk worktree (現況 auto_ensure 邏輯) 找 mtime > l2_dir/graph.bin 的檔
  │     B. 對 dirty 檔逐一比對 dirty_files.json
  │           不在 dirty_files：new entry，parse → 寫 graph_overlay fragment + tantivy delta
  │           在 dirty_files 且 mtime 未變：skip
  │           在 dirty_files 且 mtime 變了：re-parse，覆寫 fragment
  │
  └─ 6. Query exec (~varies; rkyv mmap dominant)
        mmap l2_dir/graph.bin → base ArchivedZeroCopyGraph
        merge L1 graph_overlay/*.bin via in-memory union (overrides per-uid)
        tantivy: open(l2_dir/tantivy/) + open(sessions/<sid>/tantivy_delta/) → MultiIndex
        run query
        return result
```

### Hot-path invariants

1. **不讀 `registry.json`** — cwd 解析靠 git common-dir，registry 僅在 `--repo @alias` / `@all` 才動。
2. **L2 mmap 永遠 read-only** — multi-reader 不互斥；writer 的 atomic rename 對 reader 透明（rename 前 reader 持舊 inode）。
3. **L1 fragment 寫入都 atomic** — reader merge 時看到 partial state = 不可能。
4. **`known_refs` 是 cache** — miss 不影響正確性，只多一次 git fork。

## 6. Write Path — L2 Build

```
Trigger: query at SHA X，<repo>/commits/ 沒對應 entry
  │
  ├─ 1. Build mode 決定
  │     count_existing_commits = ls(<repo>/commits/) 過濾 *.building/ 後的數量
  │     mode = if count_existing_commits == 0 { Sync } else { Background }
  │     // 第一次 build：必須 sync (沒舊 L2 可暫時撐)
  │     // 後續 SHA drift：bg (舊 L2 + L1 overlay 可即時答)
  │
  ├─ 2. Dirname 決定
  │     refs = git for-each-ref --points-at <sha> --format='%(objectname) %(refname)'
  │     pick 最具體：branch > tag > pr > commit
  │     dirname = "<type>_<sanitize(id)>__<sha>"
  │
  ├─ 3. 取 build lock
  │     mkdir -p <repo>/commits/<dirname>.building/
  │     fs2::try_lock_exclusive(<dirname>.building/.build.lock)
  │     locked-by-other → attach pattern:
  │           poll lock 釋放 + 等對方 atomic rename 完成
  │           偵測 <dirname>/ 存在後直接 read，return（不重 build）
  │
  ├─ 4. Source 解出
  │     if HEAD == <sha> AND `git diff-index --quiet HEAD` (worktree clean):
  │           src_root = worktree                        ← 零拷貝、最快路徑
  │     else:
  │           tmp_src = <dirname>.building/_src/
  │           git -C <worktree> archive <sha> | tar -x -C tmp_src
  │           src_root = tmp_src
  │
  ├─ 5. Build
  │     parse src_root 走現況 analyzer pipeline (tree-sitter → ZeroCopyGraph → rkyv)
  │     write <dirname>.building/graph.bin
  │     build tantivy → <dirname>.building/tantivy/
  │     (optional) compute embeddings → <dirname>.building/embeddings.bin
  │
  ├─ 6. Metadata
  │     CommitBuildMeta {
  │         sha, source_type, source_id, built_from_worktree, built_at,
  │         parent_sha = git rev-parse <sha>^,
  │         node_count, embedding_status,
  │         refs_at_build = parsed from step 2,
  │         refs_seen_since = []
  │     }
  │     atomic_write_json(<dirname>.building/meta.json, &commit_meta)
  │
  ├─ 7. Publish (atomic)
  │     for f in <dirname>.building/**/*: fsync(f)
  │     rename(<dirname>.building/, <dirname>/)            ← 此刻起 reader 可見
  │     rm -rf <dirname>/_src/                             ← 若有
  │
  └─ 8. Registry cache update
        atomic update <repo>/meta.json (持 .meta.lock):
            last_built_sha = sha
            total_size_bytes += size_of(<dirname>/)
        atomic update ~/.cgn/registry.json (持 registry.json.lock):
            repos.get_mut(repo_dir_name).last_touched = now()
        release .build.lock
```

### Build mode binary rule

```
fn build_mode(repo_path: &Path, target_sha: &Sha) -> BuildMode {
    let commits = repo_path.join("commits");
    let any_commit_dir = commits
        .read_dir().ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|e| e.file_name() != ".building" && e.path().is_dir());
    if !any_commit_dir { BuildMode::Sync }
    else if has_target(target_sha, &commits) { BuildMode::None }
    else { BuildMode::Background }
}
```

理由：sync 不是「為小 repo 優化」的選擇，是「沒有任何舊 L2 可暫時撐」的必然；其他情況 background 永遠是對的。避免 magic threshold（2k files / N MB）造成邊界 flap。

## 7. Write Path — L1 Incremental

每次偵測到 worktree 檔案變動 (auto-ensure walk 找到的 dirty file) 觸發。Per-file 操作，不全 dir rebuild。

```
file F at <worktree>/<rel_path>, mtime T, content_hash H
  │
  ├─ 1. Consult dirty_files.json[rel_path]
  │     A. 不在 entries → new dirty, fragment_id = H[:16]
  │     B. 在 entries 且 entry.mtime_ns == T_ns 且 entry.parse_failed == false → 已最新，skip
  │     C. 在 entries 且 (mtime 變了 OR entry.parse_failed == true) → re-parse, 同 fragment_id 或新 (若 H 改變則重命名)
  │        // parse_failed 重試：上次 parse 失敗的 fragment 不視為 fresh，每次都嘗試重 parse 直到成功
  │
  ├─ 2. Per-file parse lock (跨 session race 保護)
  │     fs2::try_lock_exclusive(<worktree>/.cgn/parse-<rel_path_hash>.lock)
  │     已 locked → poll；持鎖期間僅 parse 本檔，~10–50ms
  │
  ├─ 3. Parse F
  │     tree-sitter on F 當前 content → 本檔貢獻的 Node/Edge 子集
  │     rkyv archive bytes → write to graph_overlay/<fragment_id>.tmp
  │     fsync → rename → graph_overlay/<fragment_id>.bin
  │     // 若 fragment_id 變了 (content hash 改)，舊 <old_id>.bin 留待 sweep — 不嘗試
  │     // 與 dirty_files.json 同步刪除（怕 crash 在中間，避免 dangling fragment 被 reader
  │     // 讀到對應不到 dirty_files 的 fragment 仍然 OK，因 reader 只走 dirty_files
  │     // 列出的 fragment_id）。Orphan fragment GC：sweep 階段比對
  │     // graph_overlay/*.bin set vs dirty_files.entries[*].fragment_id set，差集 rm。
  │
  ├─ 4. Tantivy delta
  │     open IndexWriter for <session>/tantivy_delta/
  │     writer.delete_term(file_path_field == rel_path)
  │     writer.add_documents(<F 的新 docs>)
  │     writer.commit()           ← segment 落地，記錄 segment 名到 DirtyEntry
  │
  ├─ 5. Update dirty_files.json (atomic rewrite < 100KB)
  │     entries[rel_path] = DirtyEntry { mtime_ns, content_hash: H, fragment_id, tantivy_delta_segment, parse_failed: false }
  │     atomic_write_json
  │
  └─ 6. Update session_meta
        overlay_version += 1
        last_touched = now()
        atomic_write_json
```

**Parse 失敗處理**：tree-sitter 回錯 → 保留舊 fragment、`dirty_files.entries[F].parse_failed = true`、stderr log warning。Query 該檔走舊 fragment（degraded view 而非 hard fail）。

## 8. Promotion — HEAD drift / Commit

當 `git rev-parse HEAD != session_meta.base_sha` 時觸發。兩 case：

### Case A — Fast-forward / commit (新 SHA 與 base 有共同祖先)

L1 dirty 多半已被 commit 吸收，只需清掉「commit 進去了的 fragment」。

```
new_sha = git rev-parse HEAD
old_sha = session_meta.base_sha
// 確認新 L2 存在；若無 → trigger build (§6)，先返回讓 query 用 old_sha 期間結束

for (rel_path, entry) in dirty_files.entries:
    // L2 沒有預存 per-file hash — 用 git 直接從 commit 取檔內容算 sha256
    // `git cat-file blob <new_sha>:<rel_path>` → sha256 → hex
    // 成本 ~5ms / 檔；典型 session 5–10 dirty 檔，總計 ~50ms 一次性
    new_l2_file_hash = sha256(git_cat_file_blob(new_sha, rel_path))
    if entry.content_hash == new_l2_file_hash:
        // Content-equivalent — commit 已吸收這份編輯
        rm graph_overlay/<entry.fragment_id>.bin
        if let Some(seg) = entry.tantivy_delta_segment:
            tantivy delete-doc + drop segment
        dirty_files.entries.remove(rel_path)
    else:
        // Worktree 有 commit 後新編輯 — 保留
        // 但 content_hash 可能跟 commit 不同，留舊 fragment
        keep entry

session_meta.base_sha = new_sha
atomic_write_json(session_meta)
atomic_write_json(dirty_files)
audit.log: "session=X promoted A→B, fragments_kept=N, fragments_dropped=M"
```

**Drop 條件本質是 content-equivalence**，不是 path-equivalence — 一個檔在 L1 dirty 與 commit 後新 L2 內容**相同** ⇒ commit 已吸收這份 delta。

### Case B — Cross-refactor checkout (新 SHA 與 base 無近祖、或大幅 diverged)

L1 fragments 大量過時，不值得逐一比對；整批 invalidate。

```
new_sha = git rev-parse HEAD
old_sha = session_meta.base_sha
session_dir = <repo>/sessions/<sid>/

// Atomic invalidate
rename(session_dir, <repo>/sessions/<sid>.stale-<old_sha>/)
mkdir session_dir
write fresh session_meta { base_sha: new_sha, started_at: 原值, ... }

// 觸發 L2 build at new_sha 若不存在 (§6, bg mode)
// 觸發 L1 incremental update 走 worktree 仍 dirty 的所有檔 (§7 fan-out)

// 背景 GC：2 秒後 rm -rf <repo>/sessions/<sid>.stale-<old_sha>/
spawn_async("rm -rf <stale>", delay = 2s)

audit.log: "session=X rebased A→B (cross-refactor), L1 invalidated"
```

### Case A vs B 判定

```rust
fn promotion_case(old_sha: &Sha, new_sha: &Sha, worktree: &Path) -> PromotionCase {
    // Heuristic: distance via merge-base
    let base = git_merge_base(old_sha, new_sha, worktree).ok();
    match base {
        Some(b) if b == *old_sha => PromotionCase::A,  // new_sha 是 old_sha descendant (fast-forward)
        _ => PromotionCase::B,                          // 任何其他情況走 B (保守)
    }
}
```

不嘗試 detect 「rebase / 小幅 cherry-pick」這類 mid case；保守一律 Case B，多付一次 L1 rebuild 換正確性。

## 9. Concurrency & Locking

| 場景 | 鎖位置 | 模式 | 持有時長 |
|---|---|---|---|
| L2 build at SHA | `<repo>/commits/<dirname>.building/.build.lock` | fs2 exclusive non-blocking | ~10s–2min |
| L2 build attach (後到者) | 同上 | fs2 shared poll | poll 200ms 一次 |
| L1 fragment write | atomic rename pattern | 無鎖 | <5ms |
| L1 metadata write | atomic rename pattern | 無鎖 | <1ms |
| 同檔多 session parse | `<worktree>/.cgn/parse-<file_hash>.lock` | fs2 exclusive | parse 期間 ~10–50ms |
| registry.json mutate | `~/.cgn/registry.json.lock` | fs2 exclusive | <5ms 寫入瞬間 |
| repo_meta.json mutate | `<repo>/.meta.lock` | fs2 exclusive | <5ms 寫入瞬間 |

**Reader 永不被阻塞** — L2 mmap 是 OS-level shared read；writer 走 atomic rename，舊 inode 仍 valid 給 in-flight reader，新 reader 拿新 inode。L1 fragment 同此模式。

**Multi-session race scenarios**：

1. Session A + B 同時偵測 SHA X 不存在 → 各自 fork build subprocess → 兩個都嘗試 `mkdir <dirname>.building/` 並 lock — 後者 `try_lock` 失敗 → attach pattern poll + read（§6 step 3）
2. Session A + B 同檔並發 parse → per-file parse lock 序列化（§7 step 2），先到先做、後者等 ~10–50ms 走完
3. Session A pin SHA X，Session B pin SHA Y（同 repo 不同 base）→ L1 完全獨立 dir，零 contention；L2 共享 mmap read-only
4. 多 worktree 同 repo 並發查 → `git rev-parse` 走各自 worktree，common_dir 相同 ⇒ 同 `<repo>/` namespace，commits/ 可能共享

## 10. GC

### 10.1 Reachability

```
reachability(repo) = git_refs(repo) ∪ active_session_bases(repo)

git_refs(repo):
    for worktree in worktrees_for(repo.common_dir):
        for ref in git_for_each_ref(worktree):
            yield git_rev_parse(ref)

active_session_bases(repo):
    for sm in repo/sessions/*/session_meta.json:
        if sm.last_touched > now() - 24h:
            yield sm.base_sha
```

### 10.2 LRU eviction

```
quota_enforce(repo):
    if repo.total_size_bytes > QUOTA:                                // default 5GB
        candidates = sorted(commits/, by built_at ASC)
        for dir in candidates:
            sha = CommitDirName::parse(dir.name).sha
            if sha not in reachability(repo):
                rm -rf <repo>/commits/<dir>
                repo.total_size_bytes -= size_of(dir)
                if repo.total_size_bytes < QUOTA * 0.8: break
```

Reachable commits 不被 evict（即使最舊）— LLM session pin 的 base_sha 永遠保住對應 L2。

### 10.3 觸發時機

| 觸發 | 邏輯 |
|---|---|
| 每次 cgn CLI 啟動 | stat `~/.cgn/.last-gc`；若 > 24h ago，背景 `spawn_async("cgn admin gc --quiet")`，主流程不阻塞 |
| Session orphan 偵測 | session_meta.last_touched > 24h → mark `<sid>.dead/`；下次 sweep 真删 |
| 死 pid 偵測 (local only) | session_meta.pid 存在但 `kill(pid, 0)` ENOENT → 立即 mark `.dead` |
| `.stale-<sha>/` from Case B | 2 秒延遲後 `rm -rf` (避免 reader race) |
| 手動 | `cgn admin gc [--repo <name>] [--dry-run] [--force]` |

## 11. CLI Surface

### 11.1 新增 flag

| Flag | 適用命令 | 行為 |
|---|---|---|
| `--rev <ref>` | inspect / impact / cypher / search / scan / contracts / coverage / routes / shape-check / diff | 預設 HEAD；`git rev-parse <ref>` → SHA → L2 lookup |
| `--session-id <id>` | 全部 agent commands | hook / MCP 自帶；CLI 直跑 fallback `cli-<pid_hash8>` |

### 11.2 新增 admin 命令

| Command | 行為 |
|---|---|
| `cgn admin sessions list` | walk `~/.cgn/*/sessions/*` 列出 active sessions（session_id、repo、base_sha、last_touched、dirty count）|
| `cgn admin sessions reset <session-id>` | atomic rename `<sid>/` → `<sid>.dead/`、下次 sweep 清 |
| `cgn admin sessions sweep` | 立即跑一次 orphan + dead pid sweep |
| `cgn admin reset` | confirm prompt → `rm -rf ~/.cgn/` → 下次 query auto-rebuild |
| `cgn admin gc` | LRU + reachability eviction；`--repo X` 只跑單一、`--dry-run` 只報告 |

### 11.3 移除

| 移除 | 替代 |
|---|---|
| `--branch <name>` flag | `--rev <name>`；pre-1.0、project 未對外發行 — 直接刪除，不留 deprecation 期 |
| `cgn admin rename_branch` | branch 不在 storage，命令無意義 — 直接刪 |
| `--graph <path>` 預設 `.cgn/graph.bin` legacy fallback | 走 `--rev` 路徑 + auto-ensure；legacy 預設值刪除 |

### 11.4 保留 (semantics 不變)

`--repo <path | @alias | @group | @all | csv>` — 內部 alias → repo_dir hash 改走 v2 RegistryFile 快取。`@group` / `@all` 行為不變。

## 12. Error Handling & Recovery

| 失敗 | 處理 |
|---|---|
| `registry.json` version != 2 | `try_read` 回明確錯誤 `"registry schema migrated to v2; run \`cgn admin reset\` to wipe and rebuild"`；不嘗試 best-effort 轉換 |
| `registry.json` 損毀 / 缺失 | `rebuild_from_disk` walk `<repo>/meta.json` 重建 RegistryFile (alias cache 而非權威) |
| `<repo>/meta.json` 損毀 | rebuild from `commits/*/meta.json` aggregate + `git rev-parse --git-common-dir` probe |
| L2 build fail mid-process | `<dirname>.building/` 留著 → 下次啟動 GC sweep 清；不污染 `<dirname>/`（atomic rename 還沒發生）|
| L1 fragment corrupt (rkyv access fail) | reader skip 該 fragment、log warning、用 L2 base + 其餘 fragment 答；stderr 提示 `cgn admin sessions repair <id>` |
| session_meta missing/corrupt | 從 worktree state + git HEAD 重新 init；丟 query_memo + read_set；dirty_files 重新 walk worktree 重建 |
| L2 not found at SHA | trigger build per §6；sync if first L2 for repo else bg |
| 並發 build 同 SHA | attach pattern (§6 step 3) |
| HEAD drift mid-session | auto-rebase Case A/B (§8) |
| Single-file parse fail | 保留舊 fragment、`dirty_files.entries[F].parse_failed = true`；該檔的 query 走舊 fragment 而非 hard fail |
| `git rev-parse` 失敗 (not a git repo) | reject early；hint user `cd` 到 git repo 或 `cgn admin index --repo <abs-path>` |

## 13. Testing Strategy

### 13.1 Unit / schema tests

| Test | 覆蓋 |
|---|---|
| `RegistryFile` v2 round-trip | BTreeMap deterministic → twice serialize byte-equal |
| `RepoMeta` round-trip | known_refs BTreeMap deterministic |
| `CommitBuildMeta` round-trip | refs_at_build / refs_seen_since 順序穩定 |
| `SessionMeta` round-trip | overlay_version monotonic 不變量檢測 |
| `DirtyFiles` round-trip | entries BTreeMap deterministic |
| `CommitDirName::parse` happy cases | branch / tag / pr / commit 各 1 |
| `CommitDirName::parse` edge cases | (a) source_id 含 `_`（`feat_x_v2`）(b) source_id 含 `__`（`weird__name`）(c) 不在 vocab 的 type (d) SHA 非 40-hex (e) SHA 含非 hex 字元 (f) name 無 `__` 分隔 (g) `commit__sha` 無 source_id |
| `CommitDirName::format` round-trip | parse → format → parse 結果一致 |

### 13.2 Read / Write path integration

| Test | 覆蓋 |
|---|---|
| First-time build → sync mode | 空 `~/.cgn/<repo>/` → query → build 同步完成、graph.bin 存在 |
| Subsequent build → bg mode | 已有舊 L2 → checkout 新 SHA → query 立即返回（用舊 L2 + L1 overlay）、bg 完成新 L2 |
| Attach pattern | spawn 2 threads 同 detect missing SHA → 只一個 build、另一個 attach + read 完成 dir |
| L1 incremental update | edit file F → fragment 寫入、dirty_files 更新；不重 build L2 |
| Atomic write 半寫保護 | inject crash 在 `<dirname>.building/` 階段 → 啟動後 GC 清；`<dirname>/` 永不出現半成品 |

### 13.3 Promotion

| Test | 覆蓋 |
|---|---|
| Case A: fast-forward fragment drop | build L2 at A → L1 加 dirty fragment X → commit 包進 SHA B → promotion → assert fragment X dropped 因 content_hash == 新 L2 該檔 hash |
| Case A: 留 fragment | build L2 at A → L1 加 dirty → commit 不包該檔 → promotion → fragment 留住 |
| Case B: cross-refactor invalidate | build L2 at A → L1 加 dirty → `git checkout` 跨重構 SHA C → assert L1 atomic rename `.stale-<A>/`、新 L1 重新算 |
| Promotion content-equivalence (非 path-equivalence) | 兩檔同 path、不同 content → 只 content match 的 drop |

### 13.4 Concurrency / Multi-session

| Test | 覆蓋 |
|---|---|
| 2 sessions 同 repo 不同 base | Session A pin SHA X、Session B pin SHA Y → L1 完全獨立、查詢互不污染 |
| 2 sessions 並發 edit 同檔 | per-file parse lock 序列化、最終狀態一致 |
| 同 session 跨 repo | 同 session_id 在 repoA + repoB 各有 L1 dir、audit log 用 session_id 串起來 |
| Session orphan cleanup | mock session_meta.last_touched 過期 → sweep 後 dir 不存在 |
| Dead pid cleanup | mock session_meta.pid 不存在的 PID → next CLI 啟動 sweep 立即清 |

### 13.5 GC

| Test | 覆蓋 |
|---|---|
| Reachable commits 不 evict | 即使 built_at 最舊，只要 branch 仍指向 → 留 |
| Active session pin 護住 L2 | session.base_sha 對應 commit 不論多舊都不 evict |
| Quota 觸發 LRU | total_size > QUOTA → 從最舊 unreachable 開始 evict 到 QUOTA × 0.8 |
| `.stale-<sha>/` 延遲 GC | Case B 觸發 → 2s 內 stale dir 仍存（reader race 防護）→ 之後消失 |

### 13.6 Multi-language smoke (per CLAUDE.md §"Parser / core-feature changes require 14-language coverage")

雖然此 spec 不改 parser，但 build-path 路徑解析變動會 trigger 各語言 build flow，需做 smoke：

對 14 種語言（TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart）各跑一份：

1. fresh `~/.cgn/`
2. `cgn inspect <known-symbol> --repo <fixture>` → assert build 成功、結果包含正確 file:line
3. 編輯該檔 → 重 query → assert 結果反映編輯 (L1 overlay 生效)

Test fixture 沿用現況 `crates/cgn-analyzer/tests/<lang>_*.rs` 樣本。

## 14. Invariants

Spec 級宣告，每條都有對應 test：

| # | Invariant |
|---|---|
| I1 | L2 `<dirname>/` 可見 ⇒ 該目錄 build 完整（reader 永不看到半成品；半成品在 `<dirname>.building/`）|
| I2 | L1 `graph_overlay/<fragment_id>.bin` 可見 ⇒ rkyv archive 完整可 access |
| I3 | `session_meta.overlay_version` monotonically increasing；reader 偵測 mid-read 變動可選 retry |
| I4 | Cross-session L1 隔離 — session A 寫入永不影響 session B 的 L1 dir |
| I5 | `RegistryFile` / `RepoMeta` / `SessionMeta` / `DirtyFiles` serialize 二次 byte-equal（BTreeMap 確定性）|
| I6 | `CommitDirName::parse` 是 `format` 的 inverse（round-trip 不丟資訊）|
| I7 | Promotion drop fragment **iff** `fragment.content_hash == new_L2.file_hash` (content-equivalence, not path-equivalence) |
| I8 | GC 永不 evict reachable SHA（branch ref 或 active session base 指到的）|
| I9 | Build mode = Sync **iff** no other commit dir exists for the repo（避免 magic threshold）|
| I10 | Hot-path read 不依賴 `registry.json`（cwd-based 解析走 git common-dir）|

## 15. Migration Cleanup (No Migration Code)

Project 尚未對外發行；不寫 v1 → v2 自動轉換。

**動作清單**：

1. `RegistryFile::try_read` 偵測 `version != 2` → 回錯訊息（§12）
2. 新增 `cgn admin reset` 命令：confirm prompt → `rm -rf ~/.cgn/` → next any query 走 auto-ensure 自動重建
3. **刪除的 code**：
   - `crates/cgn-core/src/registry/store.rs::RepoEntry / BranchEntry / RepoEntryRaw / GroupsField`（保留 `GroupEntry`）
   - `crates/cgn-core/src/registry/store.rs::RegistryFile::rebuild_from_disk` v1 logic（重寫成 v2 邏輯）
   - `crates/cgn-core/src/registry/path.rs::IndexLayout`（整個刪）
   - `crates/cgn-core/src/registry/path.rs::sanitize_branch`（整個刪）
   - `crates/cgn-core/src/registry/meta.rs::BranchMeta`（rename + 改 schema 成 `CommitBuildMeta`，30% 邏輯重用）
   - `crates/cgn-cli/src/graph_path.rs::resolve`（重寫；舊 `LEGACY_DEFAULT` 路徑 fallback 刪除）
   - 三個 `write_atomic` 副本合一到 `registry/io.rs::atomic_write_json`
4. **預估 LOC 變化**：delete ~600，add ~900，net **+300 LOC**

## 16. Out of Scope (Future Work)

明確列出 spec 不解決、但 layout 已為其鋪好地基的後續工作：

| Future feature | 為何不在此 spec | 此 spec 鋪的地基 |
|---|---|---|
| Query memo cache | invalidation correctness 風險高，要先收 30 天 audit 才知 repeat-query 率 | L1 dir 結構容納 `query_memo.lmdb` 不需 migration |
| Read-set agent dedup | 需 cgn ↔ agent 回傳 protocol change | L1 dir 結構容納 `read_set.bin` 不需 migration |
| Cross-fork shared object pool | YAGNI；現況無此 workload | `<repo>/commits/<dirname>/` 可改 symlink 到 `_shared/objects/<sha>/`，schema 不變 |
| Background daemon (`cgn daemon`) | YAGNI；現況無 ops 需求 | 所有檔案 schema 同；daemon 是 process model 變動而非 storage 變動 |
| Embedding 增量 (overlay 也帶 embedding delta) | embedding 計算成本高，session 內 lazy 跑不化算 | DirtyEntry 已預留欄位空間；現階段 `embedding_overlay = None` |
| Multi-machine / NFS-shared `~/.cgn/` | YAGNI | flock 在 NFS 不靠譜 — 若未來要支援需切換 lock 模型，是獨立 spec |

## 17. File-Level Change Inventory

新增：

- `crates/cgn-core/src/registry/repo_meta.rs` — RepoMeta type + read/write
- `crates/cgn-core/src/registry/commit_meta.rs` — CommitBuildMeta type + read/write
- `crates/cgn-core/src/registry/dirname.rs` — CommitDirName parser
- `crates/cgn-core/src/session/mod.rs` — session module entry
- `crates/cgn-core/src/session/meta.rs` — SessionMeta
- `crates/cgn-core/src/session/overlay.rs` — DirtyFiles + overlay update logic
- `crates/cgn-cli/src/commands/admin/sessions.rs` — `sessions list / reset / sweep`
- `crates/cgn-cli/src/commands/admin/reset.rs` — `admin reset`
- `crates/cgn-cli/src/commands/admin/gc.rs` — `admin gc`

修改：

- `crates/cgn-core/src/registry/store.rs` — RegistryFile v2 schema、刪 RepoEntry / BranchEntry
- `crates/cgn-core/src/registry/meta.rs` — 刪除（內容遷至 `commit_meta.rs`）
- `crates/cgn-core/src/registry/path.rs` — 刪除 `IndexLayout` + `sanitize_branch`，保留 `sanitize_segment / derive_repo_name / uid_path / resolve_home_cgn`
- `crates/cgn-core/src/registry/io.rs` — `atomic_write_json` 變唯一 helper
- `crates/cgn-core/src/registry/mod.rs` — facade 重新 export
- `crates/cgn-cli/src/graph_path.rs` — 重寫 `resolve`：common-dir → repo_dir → commits 解析
- `crates/cgn-cli/src/repo_selector.rs` — `find_by_path` 用 common-dir 比對；`ResolvedRepo` 改抓 `<repo>/` 而非 `index_dir`
- `crates/cgn-cli/src/auto_ensure.rs` — `ensure_fresh` 改觸發 L2 build (§6) + L1 incremental (§7)
- `crates/cgn-cli/src/engine.rs` — `Engine::load` 改 mmap L2 + accept L1 overlay 參數
- `crates/cgn-cli/src/commands/admin/index.rs` — 改走 §6 build path；刪 branch 邏輯
- `crates/cgn-cli/src/commands/admin/drop.rs` — 改走 SHA-based 刪除
- `crates/cgn-cli/src/commands/admin/prune.rs` — 改走 §10 GC
- `crates/cgn-cli/src/commands/admin/rename_branch.rs` — 刪除整個命令
- `crates/cgn-cli/src/commands/hook/common.rs::lookup_index_dir` — 改走新解析路徑
- 三個 `write_atomic` 副本（`registry/store.rs`、`registry/meta.rs`、`commands/admin/claude_code.rs`）→ 合一

刪除（檔案級）：

- `crates/cgn-cli/src/commands/admin/rename_branch.rs`
- `crates/cgn-cli/src/incremental_cache.rs`（功能由 L1 graph_overlay 取代）

預估 LOC 變化：del ~600 / add ~900 / **net +300**。
