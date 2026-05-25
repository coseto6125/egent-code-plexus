<div align="center">

# `ecp` · EgentCodePlexus

### AIエージェントのために構築された構造的コードグラフ——人間のためではなく。

*22k ファイルを 2.6 s でインデックス · すべてのクエリを &lt;175 ms で回答 · 正直な未解決情報、エッジの捏造なし。*

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
![Cold index 2.6s](https://img.shields.io/badge/cold_index-2.6s%20%2F%2022k%20files-brightgreen)
![Query latency](https://img.shields.io/badge/query-%3C175ms%20cold-blue)
![Languages](https://img.shields.io/badge/languages-31%20parsed-orange)
![License](https://img.shields.io/badge/license-PolyForm%20NC-lightgrey)
![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange?logo=rust)
![Status early release](https://img.shields.io/badge/status-early%20release-yellow)

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · **日本語** · [한국어](./README_ko.md) · [Español](./README_es.md) · [Português](./README_pt-BR.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md)

</div>

---

自律型コーディングエージェントは**タスクごとに 20〜50 件の構造クエリ**を発行します。それらのクエリはすべて、人間向けに作られたツールに当たります：IDEのサイドバー、ウォームアップが必要なデーモン、読み取り用にフォーマットされた出力。このミスマッチは 3 つの具体的な障害モードとして現れます：

1. **トークンの無駄** — `grep` のダンプはエージェントが 10 シンボル必要な場面で 400 行を返す
2. **破損したリファクタリング** — リゾルバの推測が外れ、見逃した呼び出し元が漏れる
3. **依存関係の捏造** — 静的解析がエッジに到達できないとき、ツールが架空のエッジを作り出す

`ecp` はこの 3 つすべてを排除するために構築されました。

| 障害モード | `ecp` の回答 |
|---|---|
| 生の検索出力でコンテキストウィンドウが溢れる | **TOON / コンパクト JSON** — シンボル・行・エッジのみ、パディングなし |
| 呼び出し元の見逃し、サイレントな下流障害 | **`impact`** — 実際の call・extend エッジ上の正確なブラスト半径 |
| エージェントの推論内の依存関係捏造 | **`BlindSpot` レコード** — エージェントが迂回できる型付き正直な未解決情報 |
| 主要言語の外でグラフが暗くなる | **31 言語** — サービスコード・IaC・SQL・スマートコントラクトを 1 回のトラバーサルで |

---

## 🎯 設計原則

各設計決定の源は一つです：*受信するエージェントが実際に必要としているものは何か？*

**出力はデータ構造です。** TOON とコンパクト JSON は、エージェントが次の判断に必要なものだけを運びます。散文的な要約はありません。視覚的な装飾もありません。コンテキストバジェットを消費するセクションヘッダーもありません。フォーマットのデフォルトは、ほとんどの LLM プロンプトに対して既に最適な選択です。

**ステートレス。ゼロウォームアップ。** 各呼び出しはゼロコピーの `rkyv` グラフファイルを `mmap` してから終了します。**プロセス起動込みで 1 クエリあたり約 140〜170 ms。** 維持すべきデーモンはありません。ウォームアップフェーズもありません。「サーバーがクラッシュした、再起動してください」という回復パスもありません。エージェントはプロセス起動コストを払わずに、タスクごとに 50 クエリを発行できます。

**捏造よりも BlindSpot。** `ecp` がコールサイトを静的に解決できない場合——動的ディスパッチ、リフレクション、未解決のインポート——`BlindSpot` レコードを発行します：グラフ内の名前付き・型付きの明示的なギャップ。エージェントは既知の未知を迂回できます。確信ある捏造からは回復できません。

**ポリグロットをデフォルトで。** 31 言語を構造的な深さで。サービスコード、Dockerfile、GitHub Actions、Terraform、SQL、Move、Solidity——1 回のトラバーサルがすべてのレイヤーをカバーします。言語の切り替えがなければ、グラフのブラインドスポットもありません。

🎙️ **[エージェントインタビュー](../../interviews/README.md)** — Gemini CLI と Codex が、ライブの自律タスクフローで `ecp` をどのように使用しているかを説明します。

[Abhigyan Patwari](https://github.com/abhigyanpatwari) による [GitNexus](https://github.com/abhigyanpatwari/GitNexus) をベースに構築——同じ構造グラフの概念を Rust で書き直し、異なるオーディエンス向けに。[PolyForm Noncommercial 1.0.0](../../LICENSE.md)；必要な帰属表示については [NOTICES.md](../../LICENSES/NOTICES.md) を参照してください。

---

## ⚡ パフォーマンスの実績

### アップストリーム GitNexus と比較して 60× 高速なコールドインデックス

[gitnexus](https://github.com/abhigyanpatwari/GitNexus) TypeScript コードベースで計測 · `scripts/parity/benchmark_vs_gitnexus.py`:

| フェーズ | ecp (Rust) | gitnexus (Node) | 高速化倍率 |
|---|---|---|---|
| **コールドインデックス** | **~970 ms** | ~58 s | **60×** |
| **シンボルコンテキスト** | **~70 ms** | ~430 ms | **6×** |
| **ブラスト半径** | **~70 ms** | ~460 ms | **6×** |
| **Cypher クエリ** | **~70 ms** | ~400 ms | **5×** |

*`ecp` のレイテンシにはプロセス起動全体が含まれます（デーモンなし）。GitNexus (v1.6.5) はウォームアップ済みのインデックス済みリポジトリに対して計測。*

### スケール：`.sample_repo` — 22,645 ファイル、25 言語、2.1 GB ポリグロットコーパス

**インジェスト：**

| メトリクス | 値 |
|---|---|
| インデックスされたファイル数 | **22,645**（25 検出言語にわたる） |
| コールドインジェスト | **2.60 s**（パース + 解決 + シリアライズ） |
| インクリメンタルインジェスト | **4.9 ms**（xxh3_64 ハッシュウォーク、ダーティファイルなし） |
| ハードウェア | AMD Ryzen 9 9950X（16 論理コア）、39.2 GiB RAM、Linux 6.6.87 |

**プロセス起動込みのクエリレイテンシ：**

| クエリ | 中央値 | カバー範囲 |
|---|---|---|
| `summary` | **1.4 ms** | レジストリ mmap — 最小読み取り |
| `routes` | **142.3 ms** | 宣言的 + 命令的ルート列挙 |
| `summary --detailed` | **143.4 ms** | フル レジストリ + フレームワーク別信頼スコアリング |
| `impact --direction down` | **145.0 ms** | Calls / Extends エッジ上の BFS |
| `inspect` | **145.6 ms** | シンボル解決 + 1 ホップトラバーサル |
| `find --mode bm25` | **154.5 ms** | Tantivy クエリ + 5 バケットパーティション |
| `cypher`（ナロー） | **161.5 ms** | 1 パターン、1 行 |
| `cypher`（ブロード） | **174.2 ms** | より広いパターン、より多くのマッチ |
| `impact --baseline HEAD~1` | **359.0 ms** | git diff + 並列ファイル別パース + BFS |

すべてを再現するには：`python scripts/benchmark/benchmark_ecp.py`。

### Rust 系競合他社との比較

`scripts/benchmark/benchmark_vs_competitors.py` は [`codescope`](https://github.com/onur-gokyildiz-bhi/codescope)（SurrealDB バックエンド）と `coraline`（SQLite バックエンド）を `cold-index`、`symbol-find`、`callers`、`file-context`、`route-map`、`cypher` の 6 フェーズでベンチマークします。フェーズが欠けている場合は `N/A`（不在はシグナルです）。結果は `docs/benchmark-vs-competitors.md` に再生成されます。

```bash
python scripts/benchmark/benchmark_vs_competitors.py
python scripts/benchmark/benchmark_vs_competitors.py --corpus path/to/repo --iterations 5 --no-plot
```

---

## 🆚 アップストリーム GitNexus との比較

同じ構造グラフの概念、異なるオーディエンス。ドロップイン代替品ではありません——出力を読むのが誰で、それを何に使うかに基づいて選んでください。

| 次元 | EgentCodePlexus | GitNexus |
|---|---|---|
| 主要コンシューマー | 自律型 AI コードエージェント | 人間の開発者 + IDE 統合 |
| ランタイム | ステートレスワンショット CLI（ゼロウォームアップ） | 長時間稼働の MCP サーバー |
| パフォーマンス | **< 2.5s コールドインデックス / < 175ms クエリ** | ~60s コールドインデックス / ~400ms クエリ |
| 未解決エッジ | `BlindSpot` レコード（正直な未知） | ヒューリスティックな推測 |
| デフォルト出力 | TOON / コンパクト JSON（トークン効率が高い） | Wiki / UI レンダリング |
| 言語 | 31（深部 14 + 構造的 17） | 14（深部、9 次元） |
| ストレージ | Rust + `rkyv` ゼロコピー mmap | Node.js + LadybugDB |

**完全な詳細、哲学、判断マトリックス → [docs/vs-gitnexus.md](../vs-gitnexus.md)**

---

## 📦 インストール

ビルド済みバイナリは各 GitHub リリースとともに提供されます。インストーラースクリプトは、一致するアセットが利用できない場合にのみ cargo ソースビルドにフォールバックします。

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Direct cargo (no installer wrapper)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

CPU チューン済みソースビルド：

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 クイックスタート

起動すべきデーモンはありません。設定不要。ゼロからクエリ可能なグラフまで 1 コマンド。

```bash
# インデックス（インクリメンタル；インデックスが存在しない場合は最初のクエリも自動インデックス）
ecp admin index --repo .

# シンボルを検索 — デフォルトは完全一致
ecp find loginUser
ecp find login --mode bm25            # BM25 ランキング、5 出力バケットに分割

# ブラスト半径 — これを変更すると何が壊れるか？
ecp impact validateUser --direction upstream

# 完全なシンボルコンテキスト（シグネチャ、ボディ、呼び出し元、呼び出し先、1 ホップ影響）
ecp inspect validateUser

# HTTP ルートマップ（宣言的 @Get + 命令的 app.get()）
ecp routes
ecp routes /api/users --method POST   # ルート → ハンドラ → 呼び出し元チェーン

# ファイル使用状況：このパスを読み書きするのは誰か？
ecp impact --literal session_meta.json
```

すべての読み取り系コマンドは `--format text|json|toon` を受け付けます。デフォルトはコマンドごとにトークン効率が最も高いもの（主に `toon`；`find` は `text` がデフォルト；`cypher`/`summary` は `json` がデフォルト）。

---

## 🛠️ CLI サーフェス

2 つの階層があります：**エージェントコマンド**はトップレベル（クエリ / リファクタリング / 検証）、**アドミンコマンド**は `ecp admin` 配下（レジストリ / フック / 破壊的操作）。完全なフラグマトリックスは `ecp --help` と `ecp admin --help` を実行してください。

**エージェントコマンド：**

| コマンド | 用途 |
|---|---|
| `inspect <name>` | シンボル → メタデータ、デコレーター、シグネチャ、呼び出し元、呼び出し先、1 ホップ影響、含まれるメソッド / プロパティ / 列挙バリアント |
| `find <pattern>` | 完全一致 · `--mode fuzzy` · `--mode bm25`（5 バケット：ソース / テスト / リファレンス / ドキュメント / 設定） |
| `find-schema-bindings <field>` | クラス / サービス横断の MirrorsField ヒューリスティックエッジ + ブラインドスポット候補 |
| `find-transaction-patterns [--class <Name>]` | Saga compensate/undo/rollback 名前ペア；≥0.75 → POSSIBLY_RELATED、<0.75 → BLIND_SPOT |
| `impact <name> --direction <up\|down>` | 信頼フィルタリング付き BFS ブラスト半径；変更セット影響には `--since <ref>` |
| `rename --symbol <old> --new-name <new>` | 14 言語にわたる AST 対応マルチファイルリネーム。常に `--dry-run` を先に。 |
| `cypher '<query>'` | openCypher エスケープハッチ；`m.content` はソースボディを返す |
| `summary` | レジストリ概要、フレームワークカバレッジ、LLM 対応ブラインドスポットカタログ、グラフ鮮度 |
| `routes [<path>]` | HTTP ルート列挙（宣言的 + 命令的）；`<path>` を指定するとハンドラ + 呼び出し元チェーン |
| `contracts` | クロスリポジトリ API コントラクトインベントリ（ルート / キュー / RPC） |
| `diff` | リゾルバデルタ：バインディング階層低下 + ルート / コントラクト変更 |
| `tool-map` | インポートバインディング解析による外部 HTTP / DB / Redis / キューコールサイト |
| `shape-check` | HTTP コンシューマーアクセスパターンとルートレスポンスシェイプ間のドリフト |
| `peers` | マルチセッションコラボレーション：`status / diff / say / inbox / log / thread / watch / gc` |
| `review` | ワンショット監査：impact + summary + tool-map + shape-check + diff、高信頼シグナルのみ |

**アドミンコマンド**（`ecp admin <cmd>`）：

| コマンド | 用途 |
|---|---|
| `index --repo <path>` | グラフのビルド / 更新；xxh3_64 コンテンツキャッシュによるインクリメンタル。完全再ビルドには `--force`。 |
| `drop / prune / rename-branch` | インデックスライフサイクル：削除、古いブランチディレクトリの削除、ディスク上のブランチ名変更 |
| `install-hook` | Git reference-transaction フック（ブランチ切り替えの自動追跡） |
| `config` | `.ecp/config.toml` のインタラクティブ TOML ウィザード |
| `mcp serve` / `mcp tools` | MCP サーバー（stdio）；`tools` は公開サーフェスを一覧表示 |

すべてのコマンドは `--graph <path>` が指定されない限り、CWD から `.ecp/graph.bin` を解決します。すべてのエージェント向けコマンドは非インタラクティブで、すべての出力ストリームはパース可能です。

### マルチセッションピア同期

複数の LLM セッションが同じリポジトリを並行して編集している場合、`ecp peers` は各セッションのシンボルレベルのダーティ状態を表示し、セッション間の直接メッセージングを可能にします。`ECP_SESSION_ID`、`CODEX_SESSION_ID`、`CODEX_THREAD_ID`、または `CLAUDE_CODE_SESSION_ID` で登録します。

```bash
# ウォッチャーを起動（セッションごとに 1 つ；インボックスのプッシュイベントに必要）
ecp peers watch --start

# 今他に誰が編集しているか？
ecp peers status                                  # text
ecp peers status --format json                    # {session_id, pid, watcher: alive|dead|not-started}

# ピアのダーティシンボルを検査
ecp peers diff <peer-session-id> [<symbol>]

# メッセージを送信
ecp peers say "rebasing on main, hold pushes 5min"    # ブロードキャスト
ecp peers say --to <peer-session-id> "take auth.rs?"  # ターゲット指定

# 読み取りと管理
ecp peers inbox
ecp peers log --limit 20
ecp peers thread <msg-id>

# クリーンアップ
ecp peers watch --stop && ecp peers gc
```

`watcher` フィールドは `alive` | `dead` | `not-started` を区別します——クラッシュが「機能未使用」に見せかけられません。

### 証明可能なコードレビュー判定

`ecp review --verdicts` は `ecp diff` セクションからグラフバックの判定を事前計算します。JSON をレビューコンテキストとして直接渡してください——生の diff から呼び出し元関係を LLM が再導出する必要がありません。

```bash
ecp review --since main --verdicts --format json
```

| 深刻度 | ルール |
|---|---|
| `RISK` | クロスファイル呼び出し元が存在する、公開シンボルが削除された、または diff 領域にブラインドスポットがある |
| `WARN` | ファイル内呼び出し元のみ、またはルートが変更された |
| `INFO` | 呼び出し元が見つからない、または新しい公開サーフェスが追加された |

判定の種類：`SIGNATURE_OR_BODY_CHANGED` · `NEW_PUBLIC_SURFACE` · `REMOVED_PUBLIC_SURFACE` · `ROUTE_CONTRACT_CHANGED` · `BLINDSPOT_IN_DIFF_REGION`

すべての判定は、それをトリガーした正確な diff セクションとグラフの事実を引用します。完全な仕様：[docs/specs/2026-05-22-review-verdicts.md](../specs/2026-05-22-review-verdicts.md)。

---

## 🔌 エージェント統合

利用可能な場合は**ネイティブパスを優先**してください——自動リインデックスフックと、グラフクエリが往復コストに見合うかをエージェントに教えるワークフロースキルが組み込まれます。**MCP はプロトコルを話すあらゆるホストのためのユニバーサルフォールバック**です。

| エージェント | パス | 組み込まれるもの |
|---|---|---|
| Claude Code | ネイティブ | フック + スキル + オプション MCP |
| Codex CLI | ネイティブ | スキル（ネイティブツールは保留中） |
| Gemini CLI | ネイティブ | ネイティブスキル **または** MCP |
| Cursor · Windsurf · Cline · Copilot · 任意の MCP ホスト | MCP | MCP サーバー |

ガイド付きセットアップ：`ecp admin → Agent Integrations → <host>`。自動化のためのスクリプト可能なパス：`ecp admin <host> install <component>`。任意のホストを検査：`ecp admin <host> status`。

### Claude Code

```bash
ecp admin claude install hooks          # settings.json: 自動リインデックス + コンテキスト強化
ecp admin claude install skills all     # ecp + simplify スキルパック（または：ecp | simplify）
ecp admin claude install mcp-server     # オプション — フック + スキル + CLI で既に十分
```

フックは明示的なツールコールなしに、Grep/Glob/Bash のたびにグラフコンテキストを供給します。`ecp` スキルはシンボル / 影響 / ルート / コントラクト / リネームのワークフローを教えます。`simplify` はグラフファーストのコードレビューを推進します。

### Gemini CLI

```bash
ecp admin gemini install native-skill   # `gemini skills link` 経由でリンク
ecp admin gemini install mcp-server     # `gemini mcp add` 経由で登録
```

`native-skill` と `mcp-server` は相互排他的です——一方をインストールするともう一方が削除されます。

### Codex CLI

```bash
ecp admin codex install skills all      # ecp + simplify；ネイティブツールは Codex の配線保留中
```

**ワークフロースキル：**

| スキル | 使用場面 |
|---|---|
| `ecp` | グラフ対応ワークフローが、シンボル・呼び出し元・ルート・コントラクトに対して grep / ファイル読み取りより優れているかをエージェントが判断する場合 |
| `simplify` | ecp impact、ブラインドスポット、egress、シェイプドリフト、リゾルバデルタを起点とするコードレビュー |

### MCP フォールバック（Cursor、Windsurf、Cline、任意の MCP ホスト）

| ホスト | 設定ファイル |
|---|---|
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Cline (VS Code) | `cline_mcp_settings.json`（MCP パネル → 「Edit MCP Settings」） |
| 汎用 MCP ホスト | ホスト固有 |

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

```bash
ecp admin mcp tools    # 接続前に公開サーフェスを確認
ecp admin mcp serve    # コールごとにステートレスワンショット（ウォームアップコストなし）
```

---

## 🏗️ アーキテクチャ

```
crates/
├── ecp-core        # Zero-copy graph (rkyv + mmap), incremental cache, graph queries
├── ecp-analyzer    # Tree-sitter parsers, HTTP route detector, framework confidence
├── ecp-mcp         # MCP server (stdio) — exposes core commands as tools
└── ecp-cli         # `ecp` binary, Tantivy BM25 engine, token-optimized output
```

パース → 解決 → シリアライズは MPSC チャンネルを通じて、グラフを組み立ててゼロコピーの `.ecp/graph.bin` を書き込む単一のビルダースレッドに流れます。読み取りパス（`inspect`、`cypher`、`impact`、…）はこのファイルを直接 mmap します——デシリアライズステップはありません。xxh3_64 コンテンツキャッシュにより、22k ファイルのリポジトリでのインクリメンタル再ビルドをサブ秒以内に保ちます。

---

## 🌐 言語カバレッジ

31 言語を構造レベルでパース。**14 言語はフル深度**（TypeScript、JavaScript、Python、Java、Kotlin、C#、Go、Rust、PHP、Ruby、Swift、C、C++、Dart）——インポート、名前付きバインディング、エクスポート、継承、型、コンストラクタ、設定、フレームワーク、エントリーポイント、呼び出し、リネーム。**17 言語は構造的のみ**：Bash、Crystal、Cairo、Dockerfile、Docker Compose、GitHub Actions、HCL、Lua、Markdown、Move、Nim、Solidity、SQL、Verilog、Vyper、YAML、Zig。

📊 **[完全な言語機能マトリックス](../language-matrix.md)** — 言語別ステータスと根拠。

---

## ⚙️ チューニング

| 環境変数 | デフォルト | 効果 |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216`（16 MiB） | インジェスト中にこのサイズを超えるソースファイルをスキップ。ワーカー RAM の最悪ケースを `num_threads × MAX` に制限。 |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` 検出の再帰深度。深くネストされた .NET モノリポの場合は増やしてください。 |

---

## 📜 ライセンスと謝辞

[PolyForm Noncommercial 1.0.0](../../LICENSE.md)。個人利用、研究、趣味プロジェクト、非商業組織は明示的に許可されています。**商業利用はこのライセンスでは認められていません**——商業的権利については、アップストリーム GitNexus の作者 Abhigyan Patwari にお問い合わせください。

以下をベースに構築：
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — 元の設計、CLI サーフェス、概念モデル
- [tree-sitter](https://tree-sitter.github.io/) — 堅牢なインクリメンタル AST パース
- [rkyv](https://rkyv.org/) — ゼロコピーデシリアライゼーションフレームワーク
- [Tantivy](https://github.com/quickwit-oss/tantivy) — 全文検索エンジン
- [Rayon](https://github.com/rayon-rs/rayon) — マルチコア並列 AST パースのためのデータ並列性
- [xxhash (xxh3_64)](https://xxhash.com/) — コンテンツベースのインクリメンタルインデックス用非暗号ハッシュ
- [DashMap](https://github.com/xacrimon/dashmap) — グラフアセンブリ用並行ハッシュマップ
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — サブミリ秒グラフアクセスのためのゼロコピーメモリマッピング
- [msgspec](https://github.com/jcrist/msgspec) — プロセス間通信用高性能 JSON シリアライゼーション

エージェントオンボーディング（URL ブートストラップ、Claude Code スキル、プラグインインストール）：`docs/skills/ecp-onboard/`。並行性不変条件と再検証：`../../scripts/audit/audit-concurrency.sh`。

## 🚦 リリースステータス

検証済みインストールパス：`cargo install --git ...`（`ecp` をソースからビルド）。リリースインストーラーにはすでにチェックサムとプロベナンス検証フローが含まれていますが、バイナリダウンロードパスのエンドツーエンド検証には公開済みタグとリリースアセットが必要です。エージェント向けオンボーディングスキル：[docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md)。アシスト設定 / セットアップフローは現在も改良中です。

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

</div>
