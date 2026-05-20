# EgentCodePlexus

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)

[![Linux](https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](https://github.com/coseto6125/egent-code-plexus/releases)
[![macOS](https://img.shields.io/badge/macOS-000000?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Windows](https://img.shields.io/badge/Windows-0078D6?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Claude Code](https://img.shields.io/badge/Claude_Code-D97757?style=for-the-badge&logo=anthropic&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/claude/SKILL.md)
[![Codex CLI](https://img.shields.io/badge/Codex_CLI-412991?style=for-the-badge&logo=openai&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/codex/ecp/SKILL.md)
[![Cursor](https://img.shields.io/badge/Cursor-000000?style=for-the-badge&logo=cursor&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/docs/skills/ecp-onboard/guides/04-mcp.md)

`cold index 2.60 s · query p50 142 ms · 31 languages · BlindSpot edges (no hallucinated dispatch) · 60× upstream gitnexus`

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [Español](./README_es.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md) · **日本語** · [한국어](./README_ko.md) · [Português (BR)](./README_pt-BR.md)

---

## 動機

コードエージェントは 1 タスクあたり 20〜50 回のルックアップを行います。`grep` は文字列しか返しませんが、自律エージェントが本当に必要としているのは、シンボル・呼び出し元・エッジ、そして静的グラフでは答えられないときに「分からない」と正直に告げるシグナルです。

`ecp` は次の特徴を持つ構造的知識レイヤーです。

- **ステートレス。** 呼び出すたびに `rkyv` のゼロコピーグラフを `mmap` して終了します。ウォームアップしておくデーモンもなく、「サーバが落ちたので再起動を」という失敗モードもありません。
- **正直。** 呼び出しポイントを静的に解決できない場合（動的ディスパッチ、未解決 import、リフレクション）、`ecp` は `BlindSpot` レコードを出します。幻覚した依存関係に基づいて行動するエージェントは、「分からない」を受け取って迂回するエージェントよりもコストが高いのです。
- **トークン節約。** デフォルト出力は TOON（コンパクトな key:value）。すべてのフラグは `--help` で表示され、すべてのコマンドは非対話型・`stdout` がパース可能。コンテキストウィンドウを食う UI ノイズはありません。
- **多言語対応。** 31 言語を構造レベルで解析 — サービスコード、Dockerfile、GitHub Actions、Terraform、SQL、スマートコントラクトは、メインの言語を離れた瞬間にブラックホールになる、ということがなくなります。

[Abhigyan Patwari](https://github.com/abhigyanpatwari) 氏の [GitNexus](https://github.com/abhigyanpatwari/GitNexus) を土台にしています — 概念モデルは同じで、別の読み手に向けて Rust で書き直したものです。

🎙️ **[エージェントインタビュー](../../interviews/README.md)** — Gemini CLI と Codex が自律ワークフローで `ecp` を評価しています。

## 計測結果

upstream GitNexus との一対一比較。[gitnexus](https://github.com/abhigyanpatwari/GitNexus) のコードベース（TypeScript）上で `scripts/parity/benchmark_vs_gitnexus.py` を使用：

| フェーズ | ecp (Rust) | gitnexus (Node) | 高速化 |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

`ecp` の数字はプロセス起動を含む（デーモンなし）。GitNexus（v1.6.5）の数字は、すでにウォーム & インデックス済みのリポジトリに対して CLI 経由で計測したものです。

<details>
<summary><b>スケーラビリティ — <code>.sample_repo</code> 単独実行</b>（2.1 GB、~40 OSS プロジェクト、25+ 言語）</summary>

**インジェスト性能**

| フェーズ | 値 |
|---|---|
| インデックスされたファイル数 | **22,645**（検出 25 言語） |
| Wall-clock（Cold） | **2.60 s**（parse + resolve + serialize） |
| Wall-clock（Incremental） | **4.9 ms**（xxh3_64 ハッシュ walk、ダーティファイル 0） |
| ハードウェア | AMD Ryzen 9 9950X（16 論理）、39.2 GiB RAM、Linux 6.6.87 |

**クエリあたりのレイテンシ**（プロセス起動を含む）

| クエリ | 中央値 | 備考 |
|---|---|---|
| `coverage`（registry overview） | **1.4 ms** | 最小読み取り — registry mmap のみ |
| `routes`（リポジトリ全体の HTTP route マップ） | **142.3 ms** | 宣言型 + 命令型を列挙 |
| `coverage --detailed`（フレームワーク + blind-spot） | **143.4 ms** | フル registry + フレームワークごとのスコア |
| `impact <symbol> --direction down` | **145.0 ms** | Calls / Extends エッジ上の BFS |
| `inspect <symbol>`（シグネチャ + callers + callees） | **145.6 ms** | シンボル解決 + 1-hop traversal |
| `find <name> --mode bm25`（語彙検索） | **154.5 ms** | Tantivy クエリ + 5 バケットへの分割 |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | 1 パターン、1 行 |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | より広いパターン、より多くのマッチ |
| `impact --baseline HEAD~1`（changeset blast radius） | **359.0 ms** | git diff + ファイルごと並列 parse + BFS |

再現：`python scripts/benchmark/benchmark_ecp.py`。

</details>

## vs. upstream gitnexus

概念モデルは同じ、ターゲットが違います。`ecp` は drop-in **な置き換えではありません** — 誰がそのグラフを読むかで選んでください。

| 観点 | EgentCodePlexus | GitNexus |
|---|---|---|
| 主たる消費者 | 自律 AI コードエージェント | 人間の開発者 + IDE 統合 |
| Runtime | ステートレスな one-shot CLI（ウォームアップ不要） | 長期稼働の MCP サーバ |
| 性能 | **< 2.5 s cold index / < 150 ms query** | ~60 s cold index / ~400 ms query |
| 未解決エッジ | `BlindSpot` レコード（正直な「分からない」） | ヒューリスティックな推測 |
| デフォルト出力 | TOON / コンパクト JSON（トークン安価） | Wiki / UI レンダリング |
| 言語数 | 31（14 深い + 17 構造のみ） | 14（深い、9 次元） |
| ストレージ | Rust + `rkyv` ゼロコピー mmap | Node.js + LadybugDB |

8 次元の完全な内訳 + 意思決定マトリクス → [docs/vs-gitnexus.md](../vs-gitnexus.md)。

## 30 秒デモ

```bash
$ ecp impact parse_with_budget --direction upstream --format toon
```

```text
target          parse_with_budget
  kind          Function
  file          crates/ecp-analyzer/src/parse_budget.rs:28
risk_level      HIGH
direct_callers  22 across 22 files
  crates/ecp-analyzer/src/python/parser.rs:48      Method parse_file
  crates/ecp-analyzer/src/rust/parser.rs:142       Method parse_file
  crates/ecp-analyzer/src/typescript/parser.rs:73  Method parse_file
  crates/ecp-analyzer/src/go/parser.rs:69          Method parse_file
  ... (18 more language parsers)
transitive      231 symbols across language detection + pipeline
blind_spots     0
```

これがラウンドトリップの全てです — 1 プロセス、1 回の mmap、~140 ms。読み取り系コマンドは `--format text|json|toon` を受け付けます。デフォルトはコマンドごとに最もトークンの安いエンコーディングです。

## インストール

事前ビルド済みバイナリは各 GitHub Release で配布されます。インストーラスクリプトは、対応する release アセットが利用できない場合のみ cargo の source build にフォールバックします。

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# cargo 直接（インストーララッパーなしで同じ source build）
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

<details>
<summary>CPU 最適化された source build</summary>

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

</details>

## クイックスタート

```bash
# 1. 現在のリポジトリをインデックス（インクリメンタル；初回クエリも自動インデックス）
ecp admin index --repo .

# 2. シンボルを探す — デフォルトは厳密名
ecp find loginUser
ecp find login --mode bm25       # BM25 ランキング、top-K を source/tests/ref/doc/config バケットに分割

# 3. Blast radius — これを変えると誰が壊れる？
ecp impact validateUser --direction upstream

# 4. シンボルのフルコンテキスト（シグネチャ、本体、callers、callees、1-hop impact）
ecp inspect validateUser

# 5. リポジトリ内のすべての HTTP route（宣言型 @Get + 命令型 app.get()）
ecp routes
ecp routes /api/users --method POST     # route → handler → caller チェーン
```

## cli surface

2 階層 — トップレベルの **agent commands**（query / refactor / verify）と、`ecp admin` 配下の **admin commands**（registry / hooks / 破壊的操作）。完全なフラグマトリクスは `ecp --help` と `ecp admin --help` を実行してください。

| コマンド | 用途 |
|---|---|
| `inspect <name>` | シンボル 1 つ → メタデータ、デコレータ、シグネチャ、callers、callees、1-hop impact |
| `find <pattern>` | シンボル検索 — exact（デフォルト）· `--mode fuzzy` 部分文字列 · `--mode bm25` 語彙ランキング。bm25 は出力を source / tests / reference / document / config バケットに分割 |
| `impact <name> --direction <up\|down>` | confidence フィルタ付き blast-radius traversal。`--baseline <ref>` で changeset impact。 |
| `rename --symbol <old> --new-name <new>` | 14 言語にわたる AST-aware なマルチファイル rename。必ず `--dry-run` から。 |
| `cypher '<query>'` | openCypher のエスケープハッチ。`m.content` でソース本体を返す。 |
| `coverage` | Registry overview、フレームワーク coverage、blind-spot カタログ、グラフの鮮度。 |
| `routes [<path>]` | HTTP routes（宣言型 + 命令型）を列挙。`<path>` 指定で handler + callers を表示。 |
| `contracts` | リポジトリ横断 API contract インベントリ（routes / queue / RPC）。 |
| `diff` | Resolver delta — binding tier-degradation + routes / contracts のエッジレベル変更。 |
| `tool-map` | 外部 HTTP / DB / Redis / queue クライアントへの呼び出しを、ファイル単位 import-binding 分析で抽出。 |
| `shape-check` | HTTP consumer のアクセスパターンと Route のレスポンス形状とのドリフト。 |
| `peers` | マルチセッション ピア協調（status / diff / log / gc）。 |
| `review` | LLM-workflow 監査アグリゲータ — impact + coverage + tool-map + shape-check + diff を一括実行し、高信頼シグナルのみにフィルタ。 |

<details>
<summary><b>Admin namespace</b> — <code>ecp admin &lt;cmd&gt;</code>（registry / hooks / 破壊的）</summary>

| コマンド | 用途 |
|---|---|
| `index --repo <path>` | グラフのビルド / 更新。xxh3_64 コンテンツキャッシュでインクリメンタル。`--force` で完全リビルド。 |
| `drop / prune / rename-branch` | インデックスのライフサイクル：削除、古い branch dir のプルーン、ブランチ on-disk リネーム。 |
| `install-hook` | git reference-transaction フックをインストール（ブランチ切り替えを自動追跡）。 |
| `config` | `.ecp/config.toml` 対話型 TOML ウィザード。 |
| `mcp serve` / `mcp tools` | LLM ホスト向け MCP server（stdio）。`tools` は公開される tool サーフェスを表示。 |
| `claude install / codex install / gemini install` | スクリプタブルなホスト統合（skills、hooks、MCP エントリ）。 |
| `verify-resolver` | resolver ダンプを language oracle と diff（ecp-dev の QA 用）。 |

</details>

すべてのコマンドは、`--graph <path>` が渡されない限り CWD から `.ecp/graph.bin` を解決します。エージェント向けコマンドは設計上ノンインタラクティブ — フラグはすべて `--help` から、出力ストリームはすべてパース可能。`ecp admin` をサブコマンドなしで実行すると、対話型 admin TUI が開きます。

## MCP server

`ecp` はコアコマンドを MCP tool として公開する MCP server を同梱しています。MCP を話せるホスト（Claude Code、Cursor、Windsurf、Cline、Codex CLI、Gemini CLI）は `ecp` を登録して、自律的に tool を呼び出せます。

```bash
ecp admin mcp tools          # 公開される tool を確認
ecp admin mcp serve          # サーバを起動（デフォルトは spawn モード）
```

Claude Code 用の手動ホスト設定例（`~/.config/claude-code/mcp-servers.json`）:

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

人間オペレータ向けのプログレッシブパス：

```text
ecp admin → Agent Integrations → MCP → <host> → install
```

AI エージェント向けのスクリプテッドパス：

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

<details>
<summary><b>Codex CLI ネイティブ統合</b>（MCP とは別 — openai/codex フォーク向けのパッチを準備）</summary>

Codex ネイティブパスは、稼働中の Codex インストールを編集しません。`openai/codex` フォークに適用するパッチを書き出します。

プログレッシブパス：

```text
ecp admin → Agent Integrations → Codex CLI → install → native-tools
```

同梱の skills（同じプログレッシブパス）：

```text
ecp admin → Agent Integrations → Codex CLI → install → skills → all | ecp | simplify
```

エージェント向けスクリプテッドパス：

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

同梱の skills は、command help だけでは推測できないワークフロー選択を教えます：

| Skill | こんなとき |
|---|---|
| `ecp` | エージェントが、symbol / impact / route / contract / rename のグラフ感知ワークフローと grep / ファイル読みのどちらが良いかを判断する必要があるとき。 |
| `simplify` | エージェントが変更コードをレビューしていて、生の diff を読む前に `ecp impact`、blind spot、egress、shape drift、resolver delta から始めるべきとき。 |

`native-tools` コンポーネントが書き出すのは：

```text
~/.config/ecp/host-integration/codex-cli.patch
```

Codex CLI フォークでこのパッチを当てます：

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

すでに native マーカーがあるフォークを検証するには、status 確認前に `ECP_CODEX_CLI_CHECKOUT` を設定：

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

## アーキテクチャ

```
crates/
├── ecp-core        ゼロコピーグラフ（rkyv + mmap）、インクリメンタルキャッシュ、グラフクエリ
├── ecp-analyzer    Tree-sitter パーサ、HTTP route ディテクタ、フレームワーク信頼度
├── ecp-mcp         MCP server（stdio） — コアコマンドを tool として公開
└── ecp-cli         `ecp` バイナリ、Tantivy BM25 エンジン、トークン最適化された出力
```

Parse → resolve → serialize は MPSC チャネルを経由して単一の builder スレッドに流れ込み、そこでグラフが組み立てられて、ゼロコピーの `.ecp/graph.bin` が書き出されます。読み取りパス（`inspect`、`cypher`、`impact`、…）はこのファイルを直接 mmap します。xxh3_64 コンテンツキャッシュにより、22k ファイルのリポジトリでもインクリメンタルリビルドはサブ秒で完了します。

## 言語カバレッジ

31 言語を構造レベル（関数 / クラス / メソッド / import / call）で解析します。そのうち 14 言語 — オリジナルの GitNexus セット — は、import、named binding、export、heritage、type、コンストラクタ、config、フレームワーク、entry point、call、rename にわたるフルデプスのカバレッジを得ています。残り 17 言語は構造のみ（Bash、Crystal、Cairo、Dockerfile、Docker Compose、GitHub Actions、HCL、Lua、Markdown、Move、Nim、Solidity、SQL、Verilog、Vyper、YAML、Zig）。

📊 [言語別ケイパビリティの完全マトリクス](../language-matrix.md) — 言語ごとの状態と根拠。

## チューニング

| 環境変数 | デフォルト | 効果 |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216`（16 MiB） | インジェスト中、このサイズより大きいソースファイルをスキップ。最悪ケースの worker RAM を `num_threads × MAX` に抑える。 |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` 発見のためのディレクトリ再帰深度。深くネストした .NET モノレポでは引き上げ。 |

## ライセンス

[PolyForm Noncommercial 1.0.0](../../LICENSE.md) でライセンスされています。個人利用、研究、ホビープロジェクト、非営利組織は明示的に許可されています。**本ライセンスは商用利用を許諾しません** — 商用ライセンスについては upstream の GitNexus 著者 [Abhigyan Patwari](https://github.com/abhigyanpatwari) にお問い合わせください。必要な帰属表示：[NOTICES.md](../../LICENSES/NOTICES.md)。

<details>
<summary><b>基盤としているもの</b>（謝辞）</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — オリジナルの設計、CLI サーフェス、概念モデル
- [tree-sitter](https://tree-sitter.github.io/) — インクリメンタル AST パージング
- [rkyv](https://rkyv.org/) — ゼロコピーデシリアライズフレームワーク
- [Tantivy](https://github.com/quickwit-oss/tantivy) — Rust 製 BM25 サーチエンジン
- [Rayon](https://github.com/rayon-rs/rayon) — 多コア並列 AST パージング向けデータパラレリズム
- [xxhash (xxh3_64)](https://xxhash.com/) — コンテンツベースのインクリメンタルインデキシング向けハッシュ
- [DashMap](https://github.com/xacrimon/dashmap) — グラフアセンブリ用の並行ハッシュマップ
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — ゼロコピーメモリマッピング
- [msgspec](https://github.com/jcrist/msgspec) — IPC 向け高速 JSON シリアライズ

AI エージェント向けのオンボーディング（URL ブートストラップ、Claude Code skill、プラグインインストール）は `docs/skills/ecp-onboard/` にあります。並行性インバリアントとその再検証方法：`./scripts/audit/audit-concurrency.sh`。

</details>

## リリースステータス

現在検証済みのインストールパスは `cargo install --git ...` で、`ecp` をソースからビルドします。リリースインストーラには既に checksum と provenance 検証フローが含まれていますが、バイナリダウンロードパスをエンドツーエンドで検証可能にするには、公開されたタグとリリースアセットが必要です。エージェント向けのオンボーディング skill は [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md) に文書化されており、インストール、初回インデックス、任意のグループ、MCP wiring、次のステップへとユーザーをガイドします — アシスト型のセットアップフローは現在もリファイン中です。

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
