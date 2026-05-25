<div align="center">

# `ecp` · EgentCodePlexus

### AI 에이전트를 위해 설계된 구조적 코드 그래프 — 사람이 아닌.

*22,000개 파일을 2.6초에 인덱싱 · 모든 쿼리를 &lt;175 ms 내에 응답 · 미확인 정보는 정직하게 표시, 엣지를 절대 허구로 생성하지 않음.*

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
![Cold index 2.6s](https://img.shields.io/badge/cold_index-2.6s%20%2F%2022k%20files-brightgreen)
![Query latency](https://img.shields.io/badge/query-%3C175ms%20cold-blue)
![Languages](https://img.shields.io/badge/languages-31%20parsed-orange)
![License](https://img.shields.io/badge/license-PolyForm%20NC-lightgrey)
![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange?logo=rust)
![Status early release](https://img.shields.io/badge/status-early%20release-yellow)

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [日本語](./README_ja.md) · **한국어** · [Español](./README_es.md) · [Português](./README_pt-BR.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md)

</div>

---

자율 코딩 에이전트는 **작업당 20~50개의 구조적 쿼리**를 실행합니다. 이 쿼리들은 모두 사람을 위해 만들어진 도구에 부딪힙니다: IDE 사이드바, 워밍업이 필요한 데몬, 사람이 읽기 위해 포맷된 출력. 이 불일치는 세 가지 구체적인 장애 모드로 나타납니다:

1. **토큰 낭비** — `grep` 덤프가 에이전트에게 10개의 심볼이 필요한 상황에 400줄을 반환
2. **깨진 리팩토링** — 리졸버가 잘못 추측하여 호출자 하나가 누락되어 통과됨
3. **허구의 의존성** — 정적 분석이 엣지에 도달할 수 없을 때 도구가 하나를 만들어냄

`ecp`는 이 세 가지를 모두 제거하기 위해 만들어졌습니다.

| 장애 모드 | `ecp`의 해답 |
|---|---|
| 원시 검색 출력으로 컨텍스트 윈도우 초과 | **TOON / compact JSON** — 심볼, 줄, 엣지만; 패딩 없음 |
| 누락된 호출자, 조용한 하위 장애 | **`impact`** — 실제 호출 및 확장 엣지에 대한 정확한 블라스트 반경 |
| 에이전트 추론에서의 허구 의존성 | **`BlindSpot` 레코드** — 에이전트가 우회할 수 있는 타입화된 정직한 미지수 |
| 주요 언어 외부에서 그래프 암흑 | **31개 언어** — 서비스 코드, IaC, SQL, 스마트 컨트랙트를 하나의 순회로 |

---

## 🎯 설계 원칙

각 설계 결정에는 하나의 원천이 있습니다: *수신 에이전트가 실제로 필요한 것은 무엇인가?*

**출력은 데이터 구조입니다.** TOON과 compact JSON은 에이전트가 다음 결정을 내리는 데 필요한 것만 전달합니다. 산문 요약 없음. 시각적 장식 없음. 컨텍스트 예산을 소비하는 섹션 헤더 없음. 포맷 기본값은 이미 대부분의 LLM 프롬프트에 최적화되어 있습니다.

**상태 없음. 워밍업 없음.** 각 호출은 제로 카피 `rkyv` 그래프 파일을 `mmap`하고 종료합니다. **시작 포함 쿼리당 ~140–170 ms.** 유지해야 할 데몬 없음. 워밍업 단계 없음. "서버가 충돌했습니다, 재시작하세요" 복구 경로 없음. 에이전트는 프로세스 부팅 비용 없이 작업당 50개의 쿼리를 실행할 수 있습니다.

**허구 대신 BlindSpot.** `ecp`가 콜 사이트를 정적으로 해결할 수 없을 때 — 동적 디스패치, 리플렉션, 미해결 임포트 — `BlindSpot` 레코드를 발행합니다: 그래프에서 이름 붙여진, 타입화된, 명시적인 공백. 에이전트는 알려진 미지수를 피해 탐색할 수 있습니다. 확신에 찬 허구에서는 회복할 수 없습니다.

**기본적으로 폴리글롯.** 구조적 깊이에서 31개 언어. 서비스 코드, Dockerfile, GitHub Actions, Terraform, SQL, Move, Solidity — 하나의 순회가 모든 레이어를 커버합니다. 언어 전환이 없다는 것은 그래프 맹점이 없다는 것입니다.

🎙️ **[에이전트 인터뷰](../../interviews/README.md)** — Gemini CLI와 Codex가 실제 자율 작업 흐름에서 `ecp`를 어떻게 사용하는지 설명합니다.

[Abhigyan Patwari](https://github.com/abhigyanpatwari)의 [GitNexus](https://github.com/abhigyanpatwari/GitNexus)를 기반으로 구축 — 동일한 구조적 그래프 개념, Rust로 재작성, 다른 대상. [PolyForm Noncommercial 1.0.0](../../LICENSE.md); 필수 저작권 표시는 [NOTICES.md](../../LICENSES/NOTICES.md)를 참조하세요.

---

## ⚡ 성능 증거

3개 도구 직접 비교: [`codegraph`](https://github.com/colbymchenry/codegraph) (Node + SQLite)와 업스트림 [`gitnexus`](https://github.com/abhigyanpatwari/GitNexus) (Node) — 동일한 checkout, 동일한 머신. `ecp`는 상태 없는 원샷 CLI: 아래의 모든 지연 시간은 **전체 프로세스 시작을 포함**하며, 데몬 없음, 워밍업 없음.

*버전: `ecp` 0.4.2 · `codegraph` 0.9.4 · `gitnexus` 1.6.5. 설정 가능한 경우 모든 도구를 최대 파일 크기 1 MiB로 제한 (`gitnexus`는 512 KB 하드코딩). `ecp` 는 5–7회 실행의 중앙값. 하드웨어: AMD Ryzen 9 9950X (16 논리코어), Linux.*

### `microsoft/vscode` — 14,874개 파일, 밀집된 단일 언어 TypeScript

| 지표 | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **콜드 인덱스** | **4.6 s** | 166.9 s | **DNF** — 27분 후 강제 종료 |
| 피크 RSS | **~1.0 GiB** | 1.7 GiB | 4.6 GiB (계속 증가 중) |
| 심볼 검색 / 쿼리 | **34.6 ms** | 169.5 ms | — |
| 호출자 / 영향 범위 | **27.2 ms** | 172.4 ms | — |
| 검사 / 컨텍스트 | **35.0 ms** | 415.9 ms | — |
| 영향 기준선 (git-diff) | **725.9 ms** | N/A — 해당 모드 없음 | — |
| 그래프 노드 수 | **507,257** | 315,498 | — |
| 그래프 엣지 수 | 916,380 | **986,709** | — |
| 디스크 인덱스 크기 | **87 MiB** | 671 MiB | — |
| 인덱싱된 파일 수 | **14,874** | 10,814 | — |

*`gitnexus`는 완료하지 못했습니다 — 인메모리 그래프 해석 단계에서 27분간 멈춘 후 강제 종료 (RSS 4.6 GiB, 출력 없음).*

### `abhigyanpatwari/GitNexus` — 3,232개 파일, 폴리글롯 (세 도구 모두 완료 가능한 코퍼스)

| 지표 | **`ecp`** | `codegraph` | `gitnexus` |
|---|---|---|---|
| **콜드 인덱스** | **0.74 s** | 11.2 s | 77.6 s |
| 피크 RSS | **264 MiB** | 501 MiB | 2.5 GiB |
| 검색 / 쿼리 | **9.4 ms** | 103.5 ms | — |
| 호출자 / 영향 범위 | **9.2 ms** | 104.2 ms | 297.6 ms |
| 검사 / 컨텍스트 | **9.4 ms** | — | 295.5 ms |
| 그래프 노드 수 | **49,122** | 19,604 | 30,223 |
| 그래프 엣지 수 | **48,271** | 39,155 | 47,218 |
| 디스크 인덱스 크기 | **7.7 MiB** | 37 MiB | 306 MiB |
| 인덱싱된 파일 수 | **3,232** | 2,968 | 3,232 |

**콜드 인덱스: `codegraph` 보다 15–37× 빠름; `gitnexus`는 실제 대규모 저장소에서 완료하지 못함. 모든 규모에서 최저 메모리, 최소 디스크 인덱스, 가장 밀집된 그래프.**

### 규모: `.sample_repo` — 22,645개 파일, 25개 언어, 2.1 GB 폴리글롯 코퍼스

**인제스트:**

| 지표 | 값 |
|---|---|
| 인덱싱된 파일 | 25개 감지 언어에서 **22,645**개 |
| 콜드 인제스트 | **2.60 s** (파싱 + 해석 + 직렬화) |
| 증분 인제스트 | **4.9 ms** (xxh3_64 해시 워크, 더티 파일 없음) |
| 하드웨어 | AMD Ryzen 9 9950X (논리 16코어), 39.2 GiB RAM, Linux 6.6.87 |

**쿼리당 지연 시간, 프로세스 시작 포함:**

| 쿼리 | 중앙값 | 커버 범위 |
|---|---|---|
| `summary` | **1.4 ms** | 레지스트리 mmap — 최소 읽기 |
| `routes` | **142.3 ms** | 선언적 + 명령적 라우트 열거 |
| `summary --detailed` | **143.4 ms** | 전체 레지스트리 + 프레임워크별 신뢰도 점수 |
| `impact --direction down` | **145.0 ms** | Calls / Extends 엣지에 대한 BFS |
| `inspect` | **145.6 ms** | 심볼 해석 + 1-홉 순회 |
| `find --mode bm25` | **154.5 ms** | Tantivy 쿼리 + 5-버킷 파티셔닝 |
| `cypher` (좁은 범위) | **161.5 ms** | 하나의 패턴, 하나의 행 |
| `cypher` (넓은 범위) | **174.2 ms** | 더 넓은 패턴, 더 많은 매칭 |
| `impact --baseline HEAD~1` | **359.0 ms** | git diff + 병렬 파일별 파싱 + BFS |

모든 것을 재현하려면: `python scripts/benchmark/benchmark_ecp.py`.

### Rust 계열 경쟁사 비교

`../../scripts/benchmark/benchmark_vs_competitors.py`는 [`codescope`](https://github.com/onur-gokyildiz-bhi/codescope) (SurrealDB 기반)와 `coraline` (SQLite 기반)에 대해 6단계(`cold-index`, `symbol-find`, `callers`, `file-context`, `route-map`, `cypher`)에 걸쳐 벤치마크를 수행합니다. 누락된 단계 → `N/A` (부재 자체가 신호). 결과는 `docs/benchmark-vs-competitors.md`를 재생성합니다.

```bash
python scripts/benchmark/benchmark_vs_competitors.py
python scripts/benchmark/benchmark_vs_competitors.py --corpus path/to/repo --iterations 5 --no-plot
```

---

## 🆚 상위 GitNexus와의 비교

동일한 구조적 그래프 개념, 다른 대상. 드롭인 교체품이 아님 — 출력을 읽는 대상과 그것으로 무엇을 하는지에 따라 선택하세요.

| 차원 | EgentCodePlexus | GitNexus |
|---|---|---|
| 주요 소비자 | 자율 AI 코드 에이전트 | 사람 개발자 + IDE 통합 |
| 런타임 | 상태 없는 단발 CLI (워밍업 없음) | 장시간 실행 MCP 서버 |
| 성능 | **콜드 인덱스 < 2.5s / 쿼리 < 175ms** | 콜드 인덱스 ~60s / 쿼리 ~400ms |
| 미해결 엣지 | `BlindSpot` 레코드 (정직한 미지수) | 휴리스틱 추측 |
| 기본 출력 | TOON / compact JSON (토큰 저렴) | 위키 / UI 렌더링 |
| 언어 | 31개 (14개 심층 + 17개 구조적) | 14개 (심층, 9차원) |
| 저장소 | Rust + `rkyv` 제로 카피 mmap | Node.js + LadybugDB |

**전체 분석, 철학, 의사결정 매트릭스 → [docs/vs-gitnexus.md](../vs-gitnexus.md)**

---

## 📦 설치

미리 빌드된 바이너리는 각 GitHub 릴리스와 함께 제공됩니다. 인스톨러 스크립트는 매칭되는 에셋이 없을 경우에만 cargo 소스 빌드로 대체됩니다.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Direct cargo (인스톨러 래퍼 없음)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

CPU 최적화 소스 빌드:

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 빠른 시작

시작할 데몬 없음. 설정 불필요. 명령 하나로 제로 상태에서 쿼리 가능한 그래프까지.

```bash
# 인덱스 (증분; 인덱스가 없으면 첫 번째 쿼리에서 자동으로 인덱싱)
ecp admin index --repo .

# 심볼 찾기 — 기본적으로 정확 매칭
ecp find loginUser
ecp find login --mode bm25            # BM25 랭킹, 5개 출력 버킷으로 분할

# 블라스트 반경 — 이것을 변경하면 무엇이 깨지나?
ecp impact validateUser --direction upstream

# 전체 심볼 컨텍스트 (시그니처, 본문, 호출자, 피호출자, 1-홉 영향)
ecp inspect validateUser

# HTTP 라우트 맵 (선언적 @Get + 명령적 app.get())
ecp routes
ecp routes /api/users --method POST   # 라우트 → 핸들러 → 호출자 체인

# 파일 사용: 이 경로를 누가 읽고/쓰나?
ecp impact --literal session_meta.json
```

모든 읽기 전용 명령은 `--format text|json|toon`을 허용합니다. 기본값은 명령별로 토큰이 가장 저렴한 것입니다 (대부분 `toon`; `find`는 `text` 기본; `cypher`/`summary`는 `json` 기본).

---

## 🛠️ CLI 인터페이스

두 계층: 최상위 레벨의 **에이전트 명령** (쿼리 / 리팩토링 / 검증)과 `ecp admin` 하위의 **관리자 명령** (레지스트리 / 훅 / 파괴적 작업). 전체 플래그 매트릭스는 `ecp --help`와 `ecp admin --help`를 실행하세요.

**에이전트 명령:**

| 명령 | 목적 |
|---|---|
| `inspect <name>` | 심볼 → 메타데이터, 데코레이터, 시그니처, 호출자, 피호출자, 1-홉 영향, 포함된 메서드 / 프로퍼티 / 열거형 변형 |
| `find <pattern>` | 정확 · `--mode fuzzy` · `--mode bm25` (5개 버킷: source / tests / reference / document / config) |
| `find-schema-bindings <field>` | MirrorsField 휴리스틱 엣지 + 클래스 / 서비스 전반의 블라인드 스팟 후보 |
| `find-transaction-patterns [--class <Name>]` | Saga compensate/undo/rollback 이름 쌍; ≥0.75 → POSSIBLY_RELATED, <0.75 → BLIND_SPOT |
| `impact <name> --direction <up\|down>` | 신뢰도 필터링이 있는 블라스트 반경 BFS; 변경 세트 영향을 위한 `--since <ref>` |
| `rename --symbol <old> --new-name <new>` | 14개 언어에 걸친 AST 인식 다중 파일 이름 변경. 항상 `--dry-run` 먼저. |
| `cypher '<query>'` | openCypher 이스케이프 해치; `m.content`는 소스 본문 반환 |
| `summary` | 레지스트리 개요, 프레임워크 커버리지, LLM 실행 가능한 블라인드 스팟 카탈로그, 그래프 신선도 |
| `routes [<path>]` | HTTP 라우트 열거 (선언적 + 명령적); `<path>` 포함 시: 핸들러 + 호출자 체인 |
| `contracts` | 크로스 리포 API 컨트랙트 인벤토리 (라우트 / 큐 / RPC) |
| `diff` | 리졸버 델타: 바인딩 계층 저하 + 라우트 / 컨트랙트 변경 |
| `tool-map` | 임포트 바인딩 분석을 통한 외부 HTTP / DB / Redis / 큐 콜 사이트 |
| `shape-check` | HTTP 소비자 접근 패턴과 Route 응답 형태 간의 드리프트 |
| `peers` | 멀티 세션 협업: `status / diff / say / inbox / log / thread / watch / gc` |
| `review` | 단발 감사: impact + summary + tool-map + shape-check + diff, 높은 신뢰도 신호만 |

**관리자 명령** (`ecp admin <cmd>`):

| 명령 | 목적 |
|---|---|
| `index --repo <path>` | 그래프 빌드 / 갱신; xxh3_64 컨텐츠 캐시를 통한 증분 방식. 전체 재빌드에는 `--force`. |
| `drop / prune / rename-branch` | 인덱스 수명 주기: 삭제, 오래된 브랜치 디렉토리 정리, 디스크의 브랜치 이름 변경 |
| `install-hook` | Git 레퍼런스 트랜잭션 훅 (브랜치 전환 자동 추적) |
| `config` | `.ecp/config.toml`을 위한 대화형 TOML 마법사 |
| `mcp serve` / `mcp tools` | MCP 서버 (stdio); `tools`는 노출된 인터페이스 목록 표시 |

모든 명령은 `--graph <path>`가 주어지지 않으면 CWD에서 `.ecp/graph.bin`을 해석합니다. 모든 에이전트 대면 명령은 비대화형; 모든 출력 스트림은 파싱 가능합니다.

### 멀티 세션 피어 동기화

여러 LLM 세션이 동일한 저장소를 병렬로 편집할 때, `ecp peers`는 각 세션의 심볼 수준 더티 상태를 표시하고 직접 세션 메시지를 가능하게 합니다. `ECP_SESSION_ID`, `CODEX_SESSION_ID`, `CODEX_THREAD_ID`, 또는 `CLAUDE_CODE_SESSION_ID`를 통해 등록하세요.

```bash
# 워처 시작 (세션당 하나; inbox 푸시 이벤트에 필요)
ecp peers watch --start

# 지금 누가 편집 중인가?
ecp peers status                                  # text
ecp peers status --format json                    # {session_id, pid, watcher: alive|dead|not-started}

# 피어의 더티 심볼 검사
ecp peers diff <peer-session-id> [<symbol>]

# 메시지 전송
ecp peers say "rebasing on main, hold pushes 5min"    # 브로드캐스트
ecp peers say --to <peer-session-id> "take auth.rs?"  # 타겟

# 읽기 및 관리
ecp peers inbox
ecp peers log --limit 20
ecp peers thread <msg-id>

# 정리
ecp peers watch --stop && ecp peers gc
```

`watcher` 필드는 `alive` | `dead` | `not-started`를 구분합니다 — 크래시가 "기능 미사용"으로 위장하지 않습니다.

### 증명 가능한 코드 리뷰 판정

`ecp review --verdicts`는 `ecp diff` 섹션에서 그래프 기반 판정을 사전 계산합니다. JSON을 리뷰 컨텍스트로 직접 전달 — LLM이 원시 diff에서 호출자 관계를 재도출하는 것을 건너뜁니다.

```bash
ecp review --since main --verdicts --format json
```

| 심각도 | 규칙 |
|---|---|
| `RISK` | 크로스 파일 호출자 존재, 공개 심볼 제거, 또는 diff 영역에 블라인드 스팟 |
| `WARN` | 파일 내 호출자만 존재, 또는 라우트 수정 |
| `INFO` | 호출자 없음, 또는 새로운 공개 인터페이스 추가 |

판정 종류: `SIGNATURE_OR_BODY_CHANGED` · `NEW_PUBLIC_SURFACE` · `REMOVED_PUBLIC_SURFACE` · `ROUTE_CONTRACT_CHANGED` · `BLINDSPOT_IN_DIFF_REGION`

모든 판정은 이를 트리거한 정확한 diff 섹션과 그래프 사실을 인용합니다. 전체 사양: [docs/specs/2026-05-22-review-verdicts.md](../specs/2026-05-22-review-verdicts.md).

---

## 🔌 에이전트 통합

**네이티브 경로를 선호하세요** — 사용 가능한 경우 자동 재인덱스 훅과 에이전트에게 그래프 쿼리가 왕복을 감수할 가치가 있는 *시점*을 알려주는 워크플로 스킬을 연결합니다. **MCP는 프로토콜을 지원하는 모든 호스트를 위한 범용 폴백**입니다.

| 에이전트 | 경로 | 연결 |
|---|---|---|
| Claude Code | 네이티브 | 훅 + 스킬 + 선택적 MCP |
| Codex CLI | 네이티브 | 스킬 (네이티브 도구 대기 중) |
| Gemini CLI | 네이티브 | 네이티브 스킬 **또는** MCP |
| Cursor · Windsurf · Cline · Copilot · 모든 MCP 호스트 | MCP | MCP 서버 |

안내 설정: `ecp admin → Agent Integrations → <host>`. 자동화를 위한 스크립트 가능 경로: `ecp admin <host> install <component>`. 모든 호스트 검사: `ecp admin <host> status`.

### Claude Code

```bash
ecp admin claude install hooks          # settings.json: 자동 재인덱스 + 컨텍스트 보강
ecp admin claude install skills all     # ecp + simplify 스킬 팩 (또는: ecp | simplify)
ecp admin claude install mcp-server     # 선택적 — 훅 + 스킬 + CLI로 이미 충분
```

훅은 명시적인 도구 호출 없이 모든 Grep/Glob/Bash에 그래프 컨텍스트를 제공합니다. `ecp` 스킬은 심볼 / impact / route / contract / rename 워크플로를 가르칩니다. `simplify`는 그래프 우선 코드 리뷰를 구동합니다.

### Gemini CLI

```bash
ecp admin gemini install native-skill   # `gemini skills link`를 통해 연결
ecp admin gemini install mcp-server     # `gemini mcp add`를 통해 등록
```

`native-skill`과 `mcp-server`는 상호 배타적 — 하나를 설치하면 다른 하나가 제거됩니다.

### Codex CLI

```bash
ecp admin codex install skills all      # ecp + simplify; 네이티브 도구는 Codex 연결 대기 중
```

**워크플로 스킬:**

| 스킬 | 사용 시점 |
|---|---|
| `ecp` | 에이전트가 심볼, 호출자, 라우트, 컨트랙트에 대해 그래프 인식 워크플로가 grep / 파일 읽기보다 나은지 결정할 때 |
| `simplify` | ecp impact, 블라인드 스팟, 이그레스, 형태 드리프트, 리졸버 델타에서 시작하는 코드 리뷰 |

### MCP 폴백 (Cursor, Windsurf, Cline, 모든 MCP 호스트)

| 호스트 | 설정 파일 |
|---|---|
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Cline (VS Code) | `cline_mcp_settings.json` (MCP 패널 → "Edit MCP Settings") |
| 일반 MCP 호스트 | 호스트별 |

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

```bash
ecp admin mcp tools    # 연결 전 노출된 인터페이스 확인
ecp admin mcp serve    # 호출당 상태 없는 단발 (워밍업 비용 없음)
```

---

## 🏗️ 아키텍처

```
crates/
├── ecp-core        # 제로 카피 그래프 (rkyv + mmap), 증분 캐시, 그래프 쿼리
├── ecp-analyzer    # Tree-sitter 파서, HTTP 라우트 감지기, 프레임워크 신뢰도
├── ecp-mcp         # MCP 서버 (stdio) — 코어 명령을 도구로 노출
└── ecp-cli         # `ecp` 바이너리, Tantivy BM25 엔진, 토큰 최적화 출력
```

파싱 → 해석 → 직렬화는 MPSC 채널을 통해 그래프를 조립하고 제로 카피 `.ecp/graph.bin`을 쓰는 단일 빌더 스레드로 실행됩니다. 읽기 경로(`inspect`, `cypher`, `impact`, …)는 이 파일을 직접 mmap합니다 — 역직렬화 단계 없음. xxh3_64 컨텐츠 캐시는 22,000개 파일 저장소에서 증분 재빌드를 1초 미만으로 유지합니다.

---

## 🌐 언어 커버리지

구조적 수준에서 31개 언어 파싱. **14개 완전 심층** (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) — 임포트, 명명된 바인딩, 익스포트, 상속, 타입, 생성자, 설정, 프레임워크, 진입점, 호출, 이름 변경. **17개 구조적 전용**: Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig.

📊 **[전체 언어 기능 매트릭스](../language-matrix.md)** — 언어별 상태 및 근거.

---

## ⚙️ 튜닝

| 환경 변수 | 기본값 | 효과 |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | 인제스트 중 이 크기를 초과하는 소스 파일 건너뜀. 최악의 경우 워커 RAM을 `num_threads × MAX`로 제한. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | `*.csproj` 발견 재귀 깊이. 깊게 중첩된 .NET 모노리포의 경우 올리세요. |

---

## 📜 라이선스 및 감사의 말

[PolyForm Noncommercial 1.0.0](../../LICENSE.md). 개인 사용, 연구, 취미 프로젝트, 비상업적 조직에 명시적으로 허용됩니다. **이 라이선스는 상업적 사용을 허가하지 않습니다** — 상업적 권리에 대해서는 상위 GitNexus 저작자 Abhigyan Patwari에게 연락하세요.

기반 구성요소:
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — 원본 설계, CLI 인터페이스, 개념적 모델
- [tree-sitter](https://tree-sitter.github.io/) — 강력한 증분 AST 파싱
- [rkyv](https://rkyv.org/) — 제로 카피 역직렬화 프레임워크
- [Tantivy](https://github.com/quickwit-oss/tantivy) — 전문 검색 엔진
- [Rayon](https://github.com/rayon-rs/rayon) — 멀티코어 동시 AST 파싱을 위한 데이터 병렬성
- [xxhash (xxh3_64)](https://xxhash.com/) — 컨텐츠 기반 증분 인덱싱을 위한 비암호화 해싱
- [DashMap](https://github.com/xacrimon/dashmap) — 그래프 조립을 위한 동시성 해시맵
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — 밀리초 미만의 그래프 접근을 위한 제로 카피 메모리 매핑
- [msgspec](https://github.com/jcrist/msgspec) — 프로세스 간 통신을 위한 고성능 JSON 직렬화

에이전트 온보딩 (URL 부트스트랩, Claude Code 스킬, 플러그인 설치): `../skills/ecp-onboard/`. 동시성 불변성 및 재검증: `../../scripts/audit/audit-concurrency.sh`.

## 🚦 릴리스 상태

검증된 설치 경로: `cargo install --git ...`, 이는 소스에서 `ecp`를 빌드합니다. 릴리스 인스톨러에는 이미 체크섬 및 출처 검증 흐름이 포함되어 있지만, 바이너리 다운로드 경로가 엔드 투 엔드로 검증되기 전에 공개된 태그와 릴리스 에셋이 필요합니다. 에이전트 대면 온보딩 스킬: [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md). 보조 설정/구성 흐름은 여전히 개선 중입니다.

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

</div>
