# EgentCodePlexus

```
  ╔══════════════════════════════════════════════════╗
  ║  ecp                                             ║
  ║                                                  ║
  ║  structural code knowledge for AI agents         ║
  ║  one-shot cli  ·  zero-copy mmap  ·  ~140 ms     ║
  ╚══════════════════════════════════════════════════╝
```

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)

[![Linux](https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](https://github.com/coseto6125/egent-code-plexus/releases)
[![macOS](https://img.shields.io/badge/macOS-000000?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Windows](https://img.shields.io/badge/Windows-0078D6?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/releases)
[![Claude Code](https://img.shields.io/badge/Claude_Code-D97757?style=for-the-badge&logo=anthropic&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/claude/SKILL.md)
[![Codex CLI](https://img.shields.io/badge/Codex_CLI-412991?style=for-the-badge&logo=openai&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/skill_sample/codex/ecp/SKILL.md)
[![Cursor](https://img.shields.io/badge/Cursor-000000?style=for-the-badge&logo=cursor&logoColor=white)](https://github.com/coseto6125/egent-code-plexus/blob/main/docs/skills/ecp-onboard/guides/04-mcp.md)

```
  cold index   ──  2.60 s   (60× upstream gitnexus)
  query p50    ──  142 ms   ( 6× upstream gitnexus)
  languages    ──  31       (14 deep + 17 structural)
  edge policy  ──  honest unknown, never hallucinated
```

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [Español](./README_es.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md) · [日本語](./README_ja.md) · **한국어** · [Português (BR)](./README_pt-BR.md)

---

## ── 동기 ──

코드 에이전트는 하나의 태스크당 20–50회의 조회를 수행합니다. `grep`은 문자열만 돌려주지만, 자율 에이전트가 정말 필요한 것은 심볼·호출자·간선, 그리고 정적 그래프가 답할 수 없을 때 솔직하게 "모른다"고 말하는 신호입니다.

`ecp`는 다음 특성을 갖는 구조적 지식 레이어입니다.

- **상태 없음.** 호출할 때마다 `rkyv` 제로카피 그래프를 `mmap`한 뒤 종료합니다. 따뜻하게 유지해야 할 데몬도, "서버가 죽었으니 재시작" 같은 실패 모드도 없습니다.
- **정직함.** 호출 지점을 정적으로 해석할 수 없을 때(동적 디스패치, 미해결 import, 리플렉션) `ecp`는 `BlindSpot` 레코드를 발행합니다. 환각한 의존성에 기반해 행동하는 에이전트는, "모른다"는 답을 받고 우회하는 에이전트보다 훨씬 비쌉니다.
- **토큰 절약.** 기본 출력은 TOON(컴팩트 key:value). 모든 플래그는 `--help`로 노출되고, 모든 명령은 비대화형·`stdout`은 파싱 가능합니다. 컨텍스트 윈도우를 잡아먹는 UI 잡음이 없습니다.
- **다중 언어.** 31개 언어를 구조 수준에서 파싱합니다 — 서비스 코드, Dockerfile, GitHub Actions, Terraform, SQL, 스마트 컨트랙트가 주 언어를 벗어나는 순간 블랙홀이 되는 일을 없앱니다.

[Abhigyan Patwari](https://github.com/abhigyanpatwari)의 [GitNexus](https://github.com/abhigyanpatwari/GitNexus) 위에 만들어졌습니다 — 개념 모델은 동일하지만, 다른 독자를 위해 Rust로 재작성된 버전입니다.

🎙️ **[에이전트 인터뷰](../../interviews/README.md)** — Gemini CLI와 Codex가 자율 워크플로에서 `ecp`를 평가합니다.

---

## ── 측정 결과 ──

upstream GitNexus와의 정면 비교. [gitnexus](https://github.com/abhigyanpatwari/GitNexus) 코드베이스(TypeScript) 위에서 `scripts/parity/benchmark_vs_gitnexus.py`를 사용해 측정:

| 단계 | ecp (Rust) | gitnexus (Node) | 가속비 |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

`ecp`의 수치는 프로세스 기동 시간을 포함합니다(데몬 없음). GitNexus(v1.6.5)의 수치는 이미 워밍업 & 인덱싱된 저장소에 대해 CLI로 측정한 값입니다.

<details>
<summary><b>확장성 — <code>.sample_repo</code> 단일 실행</b>(2.1 GB 다국어, OSS 프로젝트 ~40개, 25+ 언어)</summary>

**인제스트 성능**

| 단계 | 값 |
|---|---|
| 인덱싱된 파일 수 | **22,645**(감지된 25개 언어) |
| Wall-clock(cold) | **2.60 s**(parse + resolve + serialize) |
| Wall-clock(incremental) | **4.9 ms**(xxh3_64 해시 walk, 더티 파일 0) |
| 하드웨어 | AMD Ryzen 9 9950X(논리 16), 39.2 GiB RAM, Linux 6.6.87 |

**쿼리별 레이턴시**(프로세스 기동 포함)

| 쿼리 | 중앙값 | 비고 |
|---|---|---|
| `coverage`(registry 개요) | **1.4 ms** | 가장 작은 읽기 — registry mmap만 |
| `routes`(저장소 전체 HTTP route 맵) | **142.3 ms** | 선언형 + 명령형 모두 열거 |
| `coverage --detailed`(프레임워크 + blind-spot) | **143.4 ms** | 전체 registry + 프레임워크별 스코어 |
| `impact <symbol> --direction down` | **145.0 ms** | Calls / Extends 간선 위의 BFS |
| `inspect <symbol>`(시그니처 + callers + callees) | **145.6 ms** | 심볼 해석 + 1-hop traversal |
| `find <name> --mode bm25`(어휘 검색) | **154.5 ms** | Tantivy 쿼리 + 5-버킷 분할 |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161.5 ms** | 단일 패턴, 단일 행 |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174.2 ms** | 더 넓은 패턴, 더 많은 매치 |
| `impact --baseline HEAD~1`(changeset blast radius) | **359.0 ms** | git diff + 파일별 병렬 parse + BFS |

재현: `python scripts/benchmark/benchmark_ecp.py`.

</details>

---

## ── vs. upstream gitnexus ──

개념 모델은 같지만 대상이 다릅니다. `ecp`는 drop-in 대체재가 **아닙니다** — 누가 그래프를 읽는지에 따라 고르세요.

| 차원 | EgentCodePlexus | GitNexus |
|---|---|---|
| 주 소비자 | 자율 AI 코드 에이전트 | 인간 개발자 + IDE 통합 |
| Runtime | 무상태 one-shot CLI(워밍업 0) | 장기 구동 MCP 서버 |
| 성능 | **< 2.5 s cold index / < 150 ms query** | ~60 s cold index / ~400 ms query |
| 미해결 간선 | `BlindSpot` 레코드(정직한 미상) | 휴리스틱 추측 |
| 기본 출력 | TOON / 컴팩트 JSON(토큰 저렴) | Wiki / UI 렌더링 |
| 언어 | 31(14 깊이 + 17 구조) | 14(깊이, 9 차원) |
| 저장 | Rust + `rkyv` 제로카피 mmap | Node.js + LadybugDB |

8개 차원 전체 분석 + 의사결정 매트릭스 → [docs/vs-gitnexus.md](../vs-gitnexus.md).

---

## ── 30초 데모 ──

```bash
$ ecp impact validateUser --direction upstream --format toon
```

```text
target          validateUser
  kind          Method
  file          src/auth/validate.py:42
risk_level      HIGH
direct_callers  3
  routes/api/login.py:18    POST /api/login   → loginUser
  routes/api/oauth.py:24    POST /api/oauth   → oauthLogin
  jobs/sync.py:91           sync_users (cron)
transitive      12 symbols across 4 files
blind_spots     1
  jobs/sync.py:103          dynamic dispatch via getattr (unresolved)
```

이게 라운드트립 전부입니다 — 프로세스 한 번, mmap 한 번, ~140 ms. 읽기 계열 명령은 `--format text|json|toon`을 받습니다. 기본값은 명령마다 토큰이 가장 저렴한 인코딩입니다.

---

## ── 설치 ──

사전 빌드된 바이너리는 매 GitHub Release마다 배포됩니다. 설치 스크립트는 매칭되는 release 자산이 없을 때만 cargo source build로 폴백합니다.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# 명시적인 cargo 경로(설치 래퍼 없는 동일 source build)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

<details>
<summary>CPU 튜닝된 source build</summary>

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

</details>

---

## ── 빠른 시작 ──

```bash
# 1. 현재 저장소 인덱싱(점진적; 첫 쿼리도 자동 인덱싱)
ecp admin index --repo .

# 2. 심볼 찾기 — 기본은 정확한 이름
ecp find loginUser
ecp find login --mode bm25       # BM25 랭킹, top-K를 source/tests/ref/doc/config 버킷으로 분할

# 3. Blast radius — 이걸 바꾸면 누가 깨질까?
ecp impact validateUser --direction upstream

# 4. 심볼의 전체 컨텍스트(시그니처, 본체, callers, callees, 1-hop impact)
ecp inspect validateUser

# 5. 저장소의 모든 HTTP route(선언형 @Get + 명령형 app.get())
ecp routes
ecp routes /api/users --method POST     # route → handler → caller 체인
```

---

## ── cli surface ──

두 계층 — 최상위 레벨의 **agent commands**(query / refactor / verify)와 `ecp admin` 아래의 **admin commands**(registry / hooks / 파괴적). 전체 플래그 매트릭스는 `ecp --help`와 `ecp admin --help`로 확인하세요.

| 명령 | 용도 |
|---|---|
| `inspect <name>` | 심볼 1개 → 메타데이터, 데코레이터, 시그니처, callers, callees, 1-hop impact |
| `find <pattern>` | 심볼 위치 찾기 — exact(기본) · `--mode fuzzy` 부분 문자열 · `--mode bm25` 어휘 랭킹. bm25는 출력을 source / tests / reference / document / config 버킷으로 분할 |
| `impact <name> --direction <up\|down>` | confidence 필터링 포함 blast-radius traversal. `--baseline <ref>`로 changeset impact. |
| `rename --symbol <old> --new-name <new>` | 14개 언어에 걸친 AST-aware 다중 파일 rename. 항상 `--dry-run` 먼저. |
| `cypher '<query>'` | openCypher escape hatch. `m.content`는 소스 본문을 반환. |
| `coverage` | Registry 개요, 프레임워크 커버리지, blind-spot 카탈로그, 그래프 신선도. |
| `routes [<path>]` | HTTP route(선언형 + 명령형) 열거. `<path>` 지정 시 handler + callers 표시. |
| `contracts` | 저장소 간 API contract 인벤토리(routes / queue / RPC). |
| `diff` | Resolver delta — binding tier-degradation + routes / contracts 간선 수준 변경. |
| `tool-map` | 외부 HTTP / DB / Redis / queue 클라이언트 호출을 파일별 import-binding 분석으로 추출. |
| `shape-check` | HTTP consumer 접근 패턴과 Route 응답 형태 간 드리프트. |
| `peers` | 다중 세션 피어 협업(status / diff / log / gc). |
| `review` | LLM-workflow 감사 집계기 — impact + coverage + tool-map + shape-check + diff를 한 번에 실행해 고신뢰 신호만 필터링. |

<details>
<summary><b>Admin namespace</b> — <code>ecp admin &lt;cmd&gt;</code>(registry / hooks / 파괴적)</summary>

| 명령 | 용도 |
|---|---|
| `index --repo <path>` | 그래프 빌드 / 갱신. xxh3_64 콘텐츠 캐시로 점진적. `--force`로 전체 리빌드. |
| `drop / prune / rename-branch` | 인덱스 라이프사이클: 삭제, 오래된 branch dir 정리, on-disk 브랜치 리네임. |
| `install-hook` | git reference-transaction 훅 설치(브랜치 전환 자동 추적). |
| `config` | `.ecp/config.toml`용 대화형 TOML 위저드. |
| `mcp serve` / `mcp tools` | LLM 호스트용 MCP server(stdio). `tools`는 노출되는 tool 표면을 나열. |
| `claude install / codex install / gemini install` | 스크립트 가능한 호스트 통합(skills, hooks, MCP 항목). |
| `verify-resolver` | resolver 덤프를 language oracle과 diff(ecp-dev QA). |

</details>

모든 명령은 `--graph <path>`가 주어지지 않으면 CWD에서 `.ecp/graph.bin`을 해석합니다. 에이전트용 명령은 설계상 비대화형 — 모든 플래그가 `--help`에서 노출되고, 모든 출력 스트림이 파싱 가능합니다. `ecp admin`을 서브커맨드 없이 실행하면 대화형 admin TUI가 열립니다.

---

## ── MCP server ──

`ecp`는 코어 명령을 MCP tool로 노출하는 MCP server를 함께 제공합니다. MCP를 말할 줄 아는 호스트(Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI)는 `ecp`를 등록하고 자율적으로 tool을 호출할 수 있습니다.

```bash
ecp admin mcp tools          # 노출될 tool 미리 보기
ecp admin mcp serve          # 서버 실행(기본은 spawn 모드)
```

Claude Code용 수동 호스트 설정 예시(`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

사람 운영자용 점진적 경로:

```text
ecp admin → Agent Integrations → MCP → <host> → install
```

AI 에이전트용 스크립트 경로:

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

<details>
<summary><b>Codex CLI 네이티브 통합</b>(MCP와 분리 — openai/codex 포크용 패치 준비)</summary>

Codex 네이티브 경로는 작동 중인 Codex 설치를 편집하지 않습니다. `openai/codex` 포크에 적용할 패치를 작성합니다.

점진적 경로:

```text
ecp admin → Agent Integrations → Codex CLI → install → native-tools
```

번들된 skills(동일한 점진 경로):

```text
ecp admin → Agent Integrations → Codex CLI → install → skills → all | ecp | simplify
```

에이전트용 스크립트 경로:

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

번들된 skills는 command help만으로는 추론할 수 없는 워크플로 선택을 가르칩니다:

| Skill | 언제 |
|---|---|
| `ecp` | 에이전트가 graph-aware한 symbol / impact / route / contract / rename 워크플로가 grep / 파일 읽기보다 더 나은지 판단해야 할 때. |
| `simplify` | 에이전트가 변경된 코드를 리뷰 중이고, 원시 diff를 읽기 전에 `ecp impact`, blind spot, egress, shape drift, resolver delta에서 시작해야 할 때. |

`native-tools` 컴포넌트가 쓰는 파일:

```text
~/.config/ecp/host-integration/codex-cli.patch
```

Codex CLI 포크에서 적용:

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

이미 네이티브 마커가 있는 포크를 검증 — status 확인 전에 `ECP_CODEX_CLI_CHECKOUT` 설정:

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

---

## ── 아키텍처 ──

```
crates/
├── ecp-core        제로카피 그래프(rkyv + mmap), 점진적 캐시, 그래프 쿼리
├── ecp-analyzer    Tree-sitter 파서, HTTP route 디텍터, 프레임워크 신뢰도
├── ecp-mcp         MCP server(stdio) — 코어 명령을 tool로 노출
└── ecp-cli         `ecp` 바이너리, Tantivy BM25 엔진, 토큰 최적화된 출력
```

Parse → resolve → serialize는 MPSC 채널을 통해 단일 builder 스레드로 흘러 들어가, 그래프를 조립하고 제로카피 `.ecp/graph.bin`을 씁니다. 읽기 경로(`inspect`, `cypher`, `impact`, …)는 이 파일을 직접 mmap합니다. xxh3_64 콘텐츠 캐시 덕분에 22k 파일 저장소도 점진적 리빌드가 서브초 단위로 유지됩니다.

---

## ── 언어 커버리지 ──

31개 언어를 구조 수준(함수 / 클래스 / 메서드 / import / call)으로 파싱합니다. 그중 14개 — 원본 GitNexus 세트 — 는 import, named binding, export, heritage, type, 생성자, config, 프레임워크, entry point, call, rename에 걸쳐 풀-뎁스 커버리지를 가집니다. 나머지 17개는 구조 전용입니다(Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig).

📊 [언어별 케이퍼빌리티 전체 매트릭스](../language-matrix.md) — 언어별 상태와 근거.

---

## ── 튜닝 ──

| 환경 변수 | 기본값 | 효과 |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216`(16 MiB) | 인제스트 중 이보다 큰 소스 파일을 건너뜀. 워커당 워스트케이스 RAM을 `num_threads × MAX`로 제한. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` 탐색용 디렉터리 재귀 깊이. 깊게 중첩된 .NET 모노레포에서 상향 조정. |

---

## ── 라이선스 ──

[PolyForm Noncommercial 1.0.0](../../LICENSE.md)으로 라이선스됩니다. 개인 사용, 연구, 취미 프로젝트, 비영리 조직은 명시적으로 허용됩니다. **본 라이선스는 상업적 사용을 부여하지 않습니다** — 상업 라이선스는 upstream GitNexus 저자 [Abhigyan Patwari](https://github.com/abhigyanpatwari)에게 문의하세요. 필요한 귀속 고지: [NOTICES.md](../../LICENSES/NOTICES.md).

<details>
<summary><b>기반</b>(감사 인사)</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — 원본 설계, CLI 표면, 개념 모델
- [tree-sitter](https://tree-sitter.github.io/) — 점진적 AST 파싱
- [rkyv](https://rkyv.org/) — 제로카피 역직렬화 프레임워크
- [Tantivy](https://github.com/quickwit-oss/tantivy) — Rust BM25 전문 검색 엔진
- [Rayon](https://github.com/rayon-rs/rayon) — 다중 코어 동시 AST 파싱용 데이터 병렬화
- [xxhash (xxh3_64)](https://xxhash.com/) — 콘텐츠 기반 점진 인덱싱용 해싱
- [DashMap](https://github.com/xacrimon/dashmap) — 그래프 어셈블리용 동시성 해시 맵
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — 제로카피 메모리 매핑
- [msgspec](https://github.com/jcrist/msgspec) — IPC용 빠른 JSON 직렬화

AI 에이전트용 온보딩(URL 부트스트랩, Claude Code skill, plugin install)은 `docs/skills/ecp-onboard/`에 있습니다. 동시성 불변식과 재검증 방법: `./scripts/audit/audit-concurrency.sh`.

</details>

---

## ── 릴리스 상태 ──

현재 검증된 설치 경로는 `cargo install --git ...`로, `ecp`를 소스에서 빌드합니다. 릴리스 인스톨러에는 이미 checksum과 provenance 검증 흐름이 포함되어 있지만, 바이너리 다운로드 경로를 엔드투엔드로 검증하려면 게시된 태그와 릴리스 자산이 필요합니다. 에이전트용 온보딩 skill은 [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md)에 문서화되어 있으며, 설치·첫 인덱스·선택적 그룹·MCP wiring·다음 단계를 안내합니다 — 보조 셋업 흐름은 계속 다듬어지고 있습니다.

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
