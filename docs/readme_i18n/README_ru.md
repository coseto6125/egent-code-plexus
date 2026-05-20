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

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [Español](./README_es.md) · **Русский** · [हिन्दी](./README_hi.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Português (BR)](./README_pt-BR.md)

---

## ── суть ──

Кодовые агенты выполняют 20–50 запросов на задачу. `grep` возвращает строки; автономному агенту нужны символы, вызывающие, рёбра графа и честный сигнал, когда статический граф не может ответить.

`ecp` — это слой структурного знания, который:

- **без состояния.** Каждый вызов делает `mmap` zero-copy графа `rkyv` и завершается. Никакого демона, который надо поддерживать тёплым, никакого «сервер упал, перезапусти».
- **честный.** Когда место вызова не разрешается статически (динамический диспатч, неразрешённый import, рефлексия), `ecp` выдаёт запись `BlindSpot`. Агент, действующий на основе галлюцинированной зависимости, обходится дороже, чем тот, кто получает «не знаю» и обходит проблему.
- **дешёвый по токенам.** Вывод по умолчанию — TOON (компактный key:value). Каждый флаг доступен через `--help`. Каждая команда не интерактивна, а её `stdout` поддаётся парсингу. Никакого UI-мусора, съедающего контекстное окно.
- **полиглотный.** 31 язык разбирается на структурном уровне — сервисный код, Dockerfile, GitHub Actions, Terraform, SQL и смарт-контракты перестают быть чёрными ящиками, как только вы покидаете основной язык.

Построен поверх [GitNexus](https://github.com/abhigyanpatwari/GitNexus) от [Abhigyan Patwari](https://github.com/abhigyanpatwari) — та же концептуальная модель, переписанная на Rust для другой аудитории.

🎙️ **[Интервью с агентами](../../interviews/README.md)** — Gemini CLI и Codex оценивают `ecp` в автономных рабочих процессах.

---

## ── чеки ──

Лоб в лоб против upstream GitNexus, измерено на кодовой базе [gitnexus](https://github.com/abhigyanpatwari/GitNexus) (TypeScript) с помощью `scripts/parity/benchmark_vs_gitnexus.py`:

| Фаза | ecp (Rust) | gitnexus (Node) | Ускорение |
|---|---|---|---|
| **Cold Index** | **~970 мс** | ~58 с | **60×** |
| **Symbol Context** | **~70 мс** | ~430 мс | **6×** |
| **Blast Radius** | **~70 мс** | ~460 мс | **6×** |
| **Cypher Query** | **~70 мс** | ~400 мс | **5×** |

Числа `ecp` включают полный старт процесса (без демона). Числа GitNexus (v1.6.5) — против уже разогретого, индексированного репозитория через его CLI.

<details>
<summary><b>Масштабируемость — один прогон по <code>.sample_repo</code></b> (2,1 ГБ полиглот, ~40 OSS-проектов, 25+ языков)</summary>

**Производительность загрузки**

| Фаза | Значение |
|---|---|
| Проиндексировано файлов | **22 645** на 25 обнаруженных языках |
| Wall-clock (cold) | **2,60 с** (parse + resolve + serialize) |
| Wall-clock (инкрементально) | **4,9 мс** (обход xxh3_64, ноль грязных файлов) |
| Железо | AMD Ryzen 9 9950X (16 логических), 39,2 ГиБ RAM, Linux 6.6.87 |

**Латентность на запрос** (включая старт процесса)

| Запрос | Медиана | Заметки |
|---|---|---|
| `coverage` (обзор registry) | **1,4 мс** | минимальное чтение — только mmap registry |
| `routes` (HTTP route-карта репозитория) | **142,3 мс** | перечисляет декларативные + императивные |
| `coverage --detailed` (фреймворки + blind-spots) | **143,4 мс** | полный registry + per-framework scoring |
| `impact <symbol> --direction down` | **145,0 мс** | BFS по Calls / Extends |
| `inspect <symbol>` (сигнатура + callers + callees) | **145,6 мс** | разрешение символа + 1-hop |
| `find <name> --mode bm25` (лексический поиск) | **154,5 мс** | запрос Tantivy + 5-ведерное партиционирование |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161,5 мс** | один паттерн, одна строка |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174,2 мс** | более широкий паттерн, больше совпадений |
| `impact --baseline HEAD~1` (blast radius changeset) | **359,0 мс** | git diff + параллельный per-file parse + BFS |

Воспроизвести: `python scripts/benchmark/benchmark_ecp.py`.

</details>

---

## ── vs. upstream gitnexus ──

Та же концептуальная модель, другая аудитория. `ecp` — **не** drop-in замена; выбирайте по тому, кто читает граф.

| Измерение | EgentCodePlexus | GitNexus |
|---|---|---|
| Основной потребитель | Автономные AI-агенты кода | Разработчики-люди + интеграция с IDE |
| Runtime | One-shot CLI без состояния (нулевой прогрев) | Долгоживущий MCP server |
| Производительность | **< 2,5 с cold index / < 150 мс query** | ~60 с cold index / ~400 мс query |
| Неразрешённое ребро | Запись `BlindSpot` (честное «не знаю») | Эвристическая догадка |
| Вывод по умолчанию | TOON / компактный JSON (дешёвый по токенам) | Wiki / UI рендеринг |
| Языков | 31 (14 глубоких + 17 структурных) | 14 (глубоких, 9 измерений) |
| Хранение | Rust + `rkyv` zero-copy mmap | Node.js + LadybugDB |

Полная разбивка по 8 измерениям + матрица решений → [docs/vs-gitnexus.md](../vs-gitnexus.md).

---

## ── 30-секундное демо ──

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

Это весь round-trip — один процесс, один mmap, ~140 мс. Команды чтения принимают `--format text|json|toon`; default на команду — кодировка, наиболее дешёвая по токенам.

---

## ── установка ──

Предсобранные бинарники публикуются с каждым GitHub Release. Скрипты установщика откатываются на cargo source build только когда нет соответствующего release-актива.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Прямой путь через cargo (тот же source build, без обёртки установщика)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

<details>
<summary>Source build с подстройкой под CPU</summary>

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

</details>

---

## ── быстрый старт ──

```bash
# 1. Индексировать текущий репозиторий (инкрементально; первый запрос тоже авто-индексирует)
ecp admin index --repo .

# 2. Найти символ — по точному имени по умолчанию
ecp find loginUser
ecp find login --mode bm25       # BM25-ранжирование, top-K по корзинам source/tests/ref/doc/config

# 3. Blast radius — кто сломается, если я изменю это?
ecp impact validateUser --direction upstream

# 4. Полный контекст символа (сигнатура, body, callers, callees, 1-hop impact)
ecp inspect validateUser

# 5. Все HTTP-маршруты репозитория (декларативные @Get + императивные app.get())
ecp routes
ecp routes /api/users --method POST     # маршрут → handler → цепочка callers
```

---

## ── cli surface ──

Два уровня — **команды агента** в верхнем уровне (query / refactor / verify) и **админ-команды** под `ecp admin` (registry / hooks / разрушающие). Запустите `ecp --help` и `ecp admin --help` для полных матриц флагов.

| Команда | Назначение |
|---|---|
| `inspect <name>` | Один символ → метаданные, декораторы, сигнатура, callers, callees, 1-hop impact |
| `find <pattern>` | Локализация символов — exact (default) · `--mode fuzzy` подстрока · `--mode bm25` лексическое ранжирование; bm25 партиционирует вывод в корзины source / tests / reference / document / config |
| `impact <name> --direction <up\|down>` | Обход blast-radius с фильтрацией по confidence. `--baseline <ref>` для impact по changeset. |
| `rename --symbol <old> --new-name <new>` | AST-aware многофайловый rename по 14 языкам. Всегда сперва `--dry-run`. |
| `cypher '<query>'` | openCypher escape hatch; `m.content` возвращает тело исходника. |
| `coverage` | Обзор registry, покрытие фреймворков, каталог blind-spot, свежесть графа. |
| `routes [<path>]` | Перечисляет HTTP-маршруты (декларативные + императивные); с `<path>` показывает handler + callers. |
| `contracts` | Инвентарь API-контрактов между репозиториями (routes / queue / RPC). |
| `diff` | Дельта resolver — binding tier-degradation + изменения routes / contracts на уровне рёбер. |
| `tool-map` | Вызовы внешних HTTP / DB / Redis / queue клиентов через per-file анализ import-binding. |
| `shape-check` | Дрейф между паттернами доступа HTTP-консюмера и формой ответа Route. |
| `peers` | Мультисессионная коллаборация пиров (status / diff / log / gc). |
| `review` | Агрегатор аудита LLM-workflow — impact + coverage + tool-map + shape-check + diff, отфильтрованный до сигналов высокой уверенности. |

<details>
<summary><b>Admin namespace</b> — <code>ecp admin &lt;cmd&gt;</code> (registry / hooks / разрушающие)</summary>

| Команда | Назначение |
|---|---|
| `index --repo <path>` | Построить / обновить граф; инкрементально через content-кэш xxh3_64. `--force` для полного rebuild. |
| `drop / prune / rename-branch` | Жизненный цикл индекса: удалить, очистить устаревшие dir веток, переименовать ветку on-disk. |
| `install-hook` | Установить git reference-transaction hook (автоотслеживание переключения веток). |
| `config` | Интерактивный TOML-визард для `.ecp/config.toml`. |
| `mcp serve` / `mcp tools` | MCP server (stdio) для LLM-хостов; `tools` перечисляет экспонированную поверхность tools. |
| `claude install / codex install / gemini install` | Скриптуемая интеграция хоста (skills, hooks, MCP-записи). |
| `verify-resolver` | Дифф resolver-дампа против language oracle (QA для ecp-dev). |

</details>

Все команды резолвят `.ecp/graph.bin` от CWD, если не передан `--graph <path>`. Команды для агентов спроектированы неинтерактивными — каждый флаг доступен через `--help`, каждый поток вывода парсится. Запустите `ecp admin` без подкоманды, чтобы открыть интерактивный admin TUI.

---

## ── MCP server ──

`ecp` поставляется с MCP server, экспонирующим core-команды как MCP tools. Хосты, говорящие на MCP (Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI), могут зарегистрировать `ecp` и автономно вызывать tools.

```bash
ecp admin mcp tools          # посмотреть, какие tools будут экспонированы
ecp admin mcp serve          # запустить server (режим spawn по умолчанию)
```

Пример ручной конфигурации хоста для Claude Code (`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

Прогрессивный путь для операторов-людей:

```text
ecp admin → Agent Integrations → MCP → <host> → install
```

Скриптуемый путь для AI-агентов:

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

<details>
<summary><b>Нативная интеграция Codex CLI</b> (отдельно от MCP — готовит патч для форка openai/codex)</summary>

Нативный путь Codex не редактирует работающую установку Codex; он пишет патч, который вы применяете к форку `openai/codex`.

Прогрессивный путь:

```text
ecp admin → Agent Integrations → Codex CLI → install → native-tools
```

Бандленные skills (тот же прогрессивный путь):

```text
ecp admin → Agent Integrations → Codex CLI → install → skills → all | ecp | simplify
```

Скриптуемый путь для агентов:

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

Бандленные skills учат выбору workflow, который command help сам по себе не выводит:

| Skill | Когда использовать |
|---|---|
| `ecp` | Агенту нужно решить, лучше ли graph-aware workflows для symbol / impact / route / contract / rename, чем grep / чтение файлов. |
| `simplify` | Агент ревьюит изменённый код и должен начать с `ecp impact`, blind-spot, egress, shape drift, resolver delta перед чтением сырого diff. |

Компонент `native-tools` пишет:

```text
~/.config/ecp/host-integration/codex-cli.patch
```

Применить в вашем форке Codex CLI:

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

Проверить форк, в котором уже стоит native-маркер — задайте `ECP_CODEX_CLI_CHECKOUT` перед проверкой статуса:

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

---

## ── архитектура ──

```
crates/
├── ecp-core        Zero-copy граф (rkyv + mmap), инкрементальный кэш, query графа
├── ecp-analyzer    Tree-sitter парсеры, детектор HTTP-маршрутов, confidence по фреймворкам
├── ecp-mcp         MCP server (stdio) — экспонирует core-команды как tools
└── ecp-cli         Бинарник `ecp`, движок BM25 Tantivy, токено-оптимизированный вывод
```

Parse → resolve → serialize проходит через MPSC-канал в один builder thread, который собирает граф и пишет zero-copy `.ecp/graph.bin`. Пути чтения (`inspect`, `cypher`, `impact`, …) mmap'ят этот файл напрямую. Content-кэш xxh3_64 держит инкрементальные rebuild на уровне сабсекунды на репо в 22k файлов.

---

## ── покрытие языков ──

31 язык разбирается на структурном уровне (функции / классы / методы / imports / calls). 14 из них — оригинальный набор GitNexus — получают full-depth покрытие по imports, named bindings, exports, heritage, types, constructors, config, frameworks, entry points, calls, rename. Оставшиеся 17 — structural-only (Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig).

📊 [Полная матрица возможностей по языкам](../language-matrix.md) — статус и обоснование по каждому языку.

---

## ── тюнинг ──

| Переменная окружения | Default | Эффект |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 МиБ) | Пропускать source-файлы больше этого размера при загрузке. Ограничивает worst-case worker-RAM до `num_threads × MAX`. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | Глубина рекурсии каталогов при поиске `*.csproj`. Повысьте для глубоко вложенных .NET-монорепозиториев. |

---

## ── лицензия ──

Лицензировано под [PolyForm Noncommercial 1.0.0](../../LICENSE.md). Личное использование, исследования, хобби-проекты и некоммерческие организации явно разрешены. **Коммерческое использование этой лицензией не предоставляется** — для коммерческих прав обращайтесь к upstream-автору GitNexus [Abhigyan Patwari](https://github.com/abhigyanpatwari). Необходимая атрибуция: [NOTICES.md](../../LICENSES/NOTICES.md).

<details>
<summary><b>Стоит на плечах</b> (благодарности)</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — оригинальный дизайн, CLI-поверхность, концептуальная модель
- [tree-sitter](https://tree-sitter.github.io/) — инкрементальный AST-парсинг
- [rkyv](https://rkyv.org/) — zero-copy фреймворк десериализации
- [Tantivy](https://github.com/quickwit-oss/tantivy) — поисковый движок BM25 на Rust
- [Rayon](https://github.com/rayon-rs/rayon) — параллелизм данных для многоядерного AST-парсинга
- [xxhash (xxh3_64)](https://xxhash.com/) — хеширование для контентно-зависимого инкрементального индексирования
- [DashMap](https://github.com/xacrimon/dashmap) — конкурентные хеш-таблицы для сборки графа
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — zero-copy memory mapping
- [msgspec](https://github.com/jcrist/msgspec) — быстрая JSON-сериализация для IPC

Онбординг для AI-агентов (URL-bootstrap, Claude Code skill, plugin install) живёт в `docs/skills/ecp-onboard/`. Инварианты конкурентности и как их перепроверить: `./scripts/audit/audit-concurrency.sh`.

</details>

---

## ── статус релиза ──

Текущий проверенный путь установки — `cargo install --git ...`, который собирает `ecp` из исходников. Release-установщики уже содержат поток проверки checksum и provenance, но требуют опубликованного тега и release-активов, прежде чем путь скачивания бинарника может быть верифицирован end-to-end. Onboarding-skill для агентов в [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md) проводит пользователей через установку, первую индексацию, опциональные группы, MCP wiring и следующие шаги — assisted-setup-флоу всё ещё дорабатывается.

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
