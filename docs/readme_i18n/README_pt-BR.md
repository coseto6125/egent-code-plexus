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

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [Español](./README_es.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · **Português (BR)**

---

## ── o caso ──

Agentes de código fazem entre 20 e 50 buscas por tarefa. O `grep` devolve strings; um agente autônomo precisa de símbolos, chamadores, arestas e um sinal honesto quando o grafo estático acaba.

`ecp` é a camada de conhecimento estrutural que é:

- **sem estado.** Cada chamada faz `mmap` de um grafo `rkyv` zero-copy e termina. Sem daemon para manter aquecido, sem o modo de falha «servidor caiu, reinicie».
- **honesto.** Quando um ponto de chamada não pode ser resolvido estaticamente (dispatch dinâmico, import não resolvido, reflection), o `ecp` emite um registro `BlindSpot`. Um agente que age sobre uma dependência alucinada custa mais do que um que recebe um «não sei» e contorna o problema.
- **barato em tokens.** Saída padrão é TOON (key:value compacto). Cada flag aparece em `--help`. Cada comando é não-interativo e seu `stdout` é parseável. Sem ruído de UI consumindo a janela de contexto.
- **poliglota.** 31 linguagens parseadas no nível estrutural — código de serviço, Dockerfiles, GitHub Actions, Terraform, SQL e smart contracts deixam de ser buracos negros assim que você sai da linguagem principal.

Construído sobre o [GitNexus](https://github.com/abhigyanpatwari/GitNexus) do [Abhigyan Patwari](https://github.com/abhigyanpatwari) — mesmo modelo conceitual, reescrito em Rust para um público diferente.

🎙️ **[Entrevistas com agentes](../../interviews/README.md)** — Gemini CLI e Codex avaliam o `ecp` em fluxos autônomos.

---

## ── recibos ──

Cabeça a cabeça contra o upstream GitNexus, medido sobre a codebase do [gitnexus](https://github.com/abhigyanpatwari/GitNexus) (TypeScript) usando `scripts/parity/benchmark_vs_gitnexus.py`:

| Fase | ecp (Rust) | gitnexus (Node) | Aceleração |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

Os números do `ecp` incluem o startup completo do processo (sem daemon). Os do GitNexus (v1.6.5) são contra um repo já indexado e aquecido via seu CLI.

<details>
<summary><b>Escalabilidade — execução única sobre <code>.sample_repo</code></b> (2,1 GB poliglota, ~40 projetos OSS, 25+ linguagens)</summary>

**Desempenho de ingestão**

| Fase | Valor |
|---|---|
| Arquivos indexados | **22.645** em 25 linguagens detectadas |
| Wall-clock (frio) | **2,60 s** (parse + resolve + serialize) |
| Wall-clock (incremental) | **4,9 ms** (caminhada de hash xxh3_64, zero arquivos sujos) |
| Hardware | AMD Ryzen 9 9950X (16 lógicos), 39,2 GiB RAM, Linux 6.6.87 |

**Latência por consulta** (inclui startup do processo)

| Consulta | Mediana | Notas |
|---|---|---|
| `coverage` (overview do registry) | **1,4 ms** | menor leitura — apenas mmap do registry |
| `routes` (mapa HTTP do repo inteiro) | **142,3 ms** | enumera declarativas + imperativas |
| `coverage --detailed` (frameworks + blind-spots) | **143,4 ms** | registry completo + scoring por framework |
| `impact <symbol> --direction down` | **145,0 ms** | BFS sobre Calls / Extends |
| `inspect <symbol>` (assinatura + callers + callees) | **145,6 ms** | resolução de símbolo + traversal 1-hop |
| `find <name> --mode bm25` (busca lexical) | **154,5 ms** | consulta Tantivy + partição em 5 baldes |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161,5 ms** | um padrão, uma linha |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174,2 ms** | padrão mais amplo, mais matches |
| `impact --baseline HEAD~1` (blast radius do changeset) | **359,0 ms** | git diff + parse paralelo por arquivo + BFS |

Reproduzir: `python scripts/benchmark/benchmark_ecp.py`.

</details>

---

## ── vs. upstream gitnexus ──

Mesmo modelo conceitual, audiência diferente. O `ecp` **não** é um substituto drop-in — escolha pelo público que vai ler o grafo.

| Dimensão | EgentCodePlexus | GitNexus |
|---|---|---|
| Consumidor primário | Agentes de código autônomos | Devs humanos + integração IDE |
| Runtime | CLI one-shot sem estado (zero warm-up) | MCP server de longa duração |
| Desempenho | **< 2,5 s cold index / < 150 ms query** | ~60 s cold index / ~400 ms query |
| Aresta não resolvida | Registro `BlindSpot` (desconhecido honesto) | Chute heurístico |
| Saída padrão | TOON / JSON compacto (token-barato) | Renderização wiki / UI |
| Linguagens | 31 (14 profundas + 17 estruturais) | 14 (profundas, 9 dimensões) |
| Armazenamento | Rust + `rkyv` zero-copy mmap | Node.js + LadybugDB |

Detalhamento completo das 8 dimensões + matriz de decisão → [docs/vs-gitnexus.md](../vs-gitnexus.md).

---

## ── demo de 30 segundos ──

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

Esse é o round-trip inteiro — um processo, um mmap, ~140 ms. Comandos de leitura aceitam `--format text|json|toon`; o default por comando é a codificação mais barata em tokens.

---

## ── instalação ──

Binários pré-compilados são publicados a cada GitHub Release. Os scripts do instalador caem para um cargo source build apenas quando não há um asset de release correspondente.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Via cargo direto (mesmo source build, sem wrapper do instalador)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

<details>
<summary>Source build tuned para CPU</summary>

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

</details>

---

## ── início rápido ──

```bash
# 1. Indexa o repo atual (incremental; a primeira query também faz auto-index)
ecp admin index --repo .

# 2. Localiza um símbolo — nome exato por padrão
ecp find loginUser
ecp find login --mode bm25       # ranking BM25, top-K em baldes source/tests/ref/doc/config

# 3. Blast radius — quem quebra se eu mudar isso?
ecp impact validateUser --direction upstream

# 4. Contexto completo do símbolo (assinatura, body, callers, callees, 1-hop impact)
ecp inspect validateUser

# 5. Todas as rotas HTTP do repo (declarativas @Get + imperativas app.get())
ecp routes
ecp routes /api/users --method POST     # rota → handler → cadeia de callers
```

---

## ── cli surface ──

Dois níveis — **comandos de agente** no top level (query / refactor / verify) e **comandos admin** sob `ecp admin` (registry / hooks / destrutivos). Rode `ecp --help` e `ecp admin --help` para as matrizes completas de flags.

| Comando | Propósito |
|---|---|
| `inspect <name>` | Um símbolo → metadata, decoradores, assinatura, callers, callees, 1-hop impact |
| `find <pattern>` | Localiza símbolos — exact (padrão) · `--mode fuzzy` substring · `--mode bm25` ranking léxico; bm25 particiona a saída em baldes source / tests / reference / document / config |
| `impact <name> --direction <up\|down>` | Traversal de blast-radius com filtragem por confiança. `--baseline <ref>` para impact por changeset. |
| `rename --symbol <old> --new-name <new>` | Rename AST-aware multi-arquivo em 14 linguagens. Sempre `--dry-run` primeiro. |
| `cypher '<query>'` | Escape hatch openCypher; `m.content` devolve o corpo do fonte. |
| `coverage` | Overview do registry, cobertura por framework, catálogo de blind-spots, frescor do grafo. |
| `routes [<path>]` | Enumera rotas HTTP (declarativas + imperativas); com `<path>` mostra handler + callers. |
| `contracts` | Inventário cross-repo de contratos API (routes / queue / RPC). |
| `diff` | Delta do resolver — binding tier-degradation + mudanças de routes / contracts em nível de aresta. |
| `tool-map` | Chamadas a clients externos HTTP / DB / Redis / queue via análise de import-binding por arquivo. |
| `shape-check` | Drift entre padrões de acesso do consumer HTTP e a forma de resposta da Route. |
| `peers` | Colaboração multi-sessão entre pares (status / diff / log / gc). |
| `review` | Agregador de auditoria LLM-workflow — impact + coverage + tool-map + shape-check + diff, filtrado para sinais de alta confiança. |

<details>
<summary><b>Namespace admin</b> — <code>ecp admin &lt;cmd&gt;</code> (registry / hooks / destrutivos)</summary>

| Comando | Propósito |
|---|---|
| `index --repo <path>` | Constrói / atualiza o grafo; incremental via cache de conteúdo xxh3_64. `--force` para rebuild completo. |
| `drop / prune / rename-branch` | Ciclo de vida do índice: deletar, podar dirs de branch obsoletos, renomear branch on-disk. |
| `install-hook` | Instala o hook git reference-transaction (auto-tracking de mudanças de branch). |
| `config` | Wizard TOML interativo para `.ecp/config.toml`. |
| `mcp serve` / `mcp tools` | MCP server (stdio) para hosts LLM; `tools` lista a superfície de tools exposta. |
| `claude install / codex install / gemini install` | Integração scriptável de host (skills, hooks, entradas MCP). |
| `verify-resolver` | Diff do dump do resolver contra um oracle de linguagem (QA para ecp-dev). |

</details>

Todos os comandos resolvem `.ecp/graph.bin` a partir do CWD a menos que `--graph <path>` seja passado. Comandos voltados a agente são não-interativos por design — toda flag sai em `--help`, todo stream de saída é parseável. Execute `ecp admin` sem subcomando para abrir o TUI admin interativo.

---

## ── MCP server ──

O `ecp` traz um MCP server que expõe os comandos core como tools MCP. Hosts que falam MCP (Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI) podem registrar `ecp` e chamar as tools autonomamente.

```bash
ecp admin mcp tools          # inspeciona quais tools serão expostas
ecp admin mcp serve          # roda o server (modo spawn por padrão)
```

Exemplo de configuração manual do host para Claude Code (`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

Caminho progressivo para operadores humanos:

```text
ecp admin → Agent Integrations → MCP → <host> → install
```

Caminho scriptado para agentes IA:

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

<details>
<summary><b>Integração nativa do Codex CLI</b> (separada de MCP — prepara um patch para um fork do openai/codex)</summary>

O caminho nativo do Codex não edita a instalação do Codex em uso; escreve um patch que você aplica a um fork do `openai/codex`.

Caminho progressivo:

```text
ecp admin → Agent Integrations → Codex CLI → install → native-tools
```

Skills incluídos (mesmo caminho progressivo):

```text
ecp admin → Agent Integrations → Codex CLI → install → skills → all | ecp | simplify
```

Caminho scriptado para agentes:

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

Os skills incluídos ensinam seleção de workflow que o command help não consegue inferir:

| Skill | Quando usar |
|---|---|
| `ecp` | O agente precisa decidir se workflows graph-aware de symbol / impact / route / contract / rename batem grep / leitura de arquivos. |
| `simplify` | O agente está revisando código alterado e deve começar por `ecp impact`, blind spots, egress, shape drift e resolver deltas antes de ler diffs crus. |

O componente `native-tools` escreve:

```text
~/.config/ecp/host-integration/codex-cli.patch
```

Aplique no seu fork do Codex CLI:

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

Para verificar um fork que já tem o marcador nativo — defina `ECP_CODEX_CLI_CHECKOUT` antes de consultar status:

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

---

## ── arquitetura ──

```
crates/
├── ecp-core        Grafo zero-copy (rkyv + mmap), cache incremental, queries de grafo
├── ecp-analyzer    Parsers tree-sitter, detector de rotas HTTP, confiança por framework
├── ecp-mcp         MCP server (stdio) — expõe comandos core como tools
└── ecp-cli         Binário `ecp`, engine BM25 do Tantivy, saída otimizada para tokens
```

Parse → resolve → serialize roda através de um canal MPSC para uma única thread builder que monta o grafo e escreve um `.ecp/graph.bin` zero-copy. As rotas de leitura (`inspect`, `cypher`, `impact`, …) fazem mmap direto desse arquivo. O cache de conteúdo xxh3_64 mantém os rebuilds incrementais em sub-segundo num repo de 22k arquivos.

---

## ── cobertura de linguagens ──

31 linguagens parseadas no nível estrutural (funções / classes / métodos / imports / calls). 14 delas — o conjunto original do GitNexus — recebem cobertura full-depth em imports, named bindings, exports, herança, types, construtores, config, frameworks, entry points, calls e rename. As outras 17 são structural-only (Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig).

📊 [Matriz completa de capacidades por linguagem](../language-matrix.md) — status e racional por linguagem.

---

## ── tuning ──

| Variável de ambiente | Padrão | Efeito |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Pula arquivos source maiores que isso durante a ingestão. Limita o pior caso de RAM por worker a `num_threads × MAX`. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | Profundidade de recursão de diretórios para descoberta de `*.csproj`. Aumente para monorepos .NET muito aninhados. |

---

## ── licença ──

Licenciado sob [PolyForm Noncommercial 1.0.0](../../LICENSE.md). Uso pessoal, pesquisa, projetos hobby e organizações sem fins lucrativos são explicitamente permitidos. **Uso comercial não é concedido por esta licença** — entre em contato com o autor upstream do GitNexus [Abhigyan Patwari](https://github.com/abhigyanpatwari) para direitos comerciais. Atribuição obrigatória: [NOTICES.md](../../LICENSES/NOTICES.md).

<details>
<summary><b>Construído sobre</b> (agradecimentos)</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — design original, superfície CLI, modelo conceitual
- [tree-sitter](https://tree-sitter.github.io/) — parsing AST incremental
- [rkyv](https://rkyv.org/) — framework de desserialização zero-copy
- [Tantivy](https://github.com/quickwit-oss/tantivy) — engine de busca BM25 em Rust
- [Rayon](https://github.com/rayon-rs/rayon) — paralelismo de dados para parsing AST multinúcleo
- [xxhash (xxh3_64)](https://xxhash.com/) — hashing para indexação incremental baseada em conteúdo
- [DashMap](https://github.com/xacrimon/dashmap) — hash maps concorrentes para montagem do grafo
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — memory mapping zero-copy
- [msgspec](https://github.com/jcrist/msgspec) — serialização JSON rápida para IPC

O onboarding para agentes IA (bootstrap por URL, skill do Claude Code, instalação de plugin) vive em `docs/skills/ecp-onboard/`. Invariantes de concorrência e como re-verificá-los: `./scripts/audit/audit-concurrency.sh`.

</details>

---

## ── status de release ──

O caminho de instalação verificado atual é `cargo install --git ...`, que compila `ecp` a partir do source. Os release installers já contêm o fluxo de verificação de checksum e procedência, mas requerem uma tag publicada e release assets antes que o caminho de download do binário possa ser verificado end-to-end. O skill de onboarding voltado a agentes em [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md) guia os usuários por install, first-index, groups opcionais, wiring de MCP e próximos passos — o fluxo de setup assistido segue sendo refinado.

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
