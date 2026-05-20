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

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · **Español** · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Português (BR)](./README_pt-BR.md)

---

## ── el caso ──

Los agentes de código realizan entre 20 y 50 búsquedas por tarea. `grep` devuelve cadenas; un agente autónomo necesita símbolos, llamadores, aristas y una señal honesta cuando el grafo estático se agota.

`ecp` es la capa de conocimiento estructural que es:

- **sin estado.** Cada invocación hace `mmap` de un grafo `rkyv` zero-copy y termina. Sin daemon que mantener caliente, sin modo de fallo «el servidor murió, reinícialo».
- **honesta.** Cuando un sitio de llamada no se puede resolver estáticamente (despacho dinámico, import sin resolver, reflexión), `ecp` emite un registro `BlindSpot`. Un agente que actúa sobre una dependencia alucinada cuesta más que uno que recibe un «no sé» y rodea el problema.
- **barata en tokens.** Salida por defecto en TOON (clave:valor compacto). Cada flag aparece en `--help`. Cada comando es no-interactivo y su `stdout` es parseable. Sin ruido de UI consumiendo la ventana de contexto.
- **políglota.** 31 lenguajes parseados a nivel estructural — código de servicio, Dockerfiles, GitHub Actions, Terraform, SQL y smart contracts dejan de ser agujeros negros en cuanto sales del lenguaje principal.

Construido sobre [GitNexus](https://github.com/abhigyanpatwari/GitNexus) de [Abhigyan Patwari](https://github.com/abhigyanpatwari) — mismo modelo conceptual, reescrito en Rust para un público distinto.

🎙️ **[Entrevistas con agentes](../../interviews/README.md)** — Gemini CLI y Codex evalúan `ecp` en flujos autónomos.

---

## ── recibos ──

Cabeza a cabeza contra el upstream GitNexus, medido sobre la codebase de [gitnexus](https://github.com/abhigyanpatwari/GitNexus) (TypeScript) con `scripts/parity/benchmark_vs_gitnexus.py`:

| Fase | ecp (Rust) | gitnexus (Node) | Aceleración |
|---|---|---|---|
| **Cold Index** | **~970 ms** | ~58 s | **60×** |
| **Symbol Context** | **~70 ms** | ~430 ms | **6×** |
| **Blast Radius** | **~70 ms** | ~460 ms | **6×** |
| **Cypher Query** | **~70 ms** | ~400 ms | **5×** |

Los números de `ecp` incluyen el arranque completo del proceso (sin daemon). Los de GitNexus (v1.6.5) son contra un repo ya indexado y caliente vía su CLI.

<details>
<summary><b>Escalabilidad — corrida única sobre <code>.sample_repo</code></b> (2,1 GB, ~40 proyectos OSS, 25+ lenguajes)</summary>

**Rendimiento de ingesta**

| Fase | Valor |
|---|---|
| Archivos indexados | **22 645** en 25 lenguajes detectados |
| Reloj de pared (frío) | **2,60 s** (parse + resolve + serialize) |
| Reloj de pared (incremental) | **4,9 ms** (recorrido xxh3_64, cero archivos sucios) |
| Hardware | AMD Ryzen 9 9950X (16 lógicos), 39,2 GiB RAM, Linux 6.6.87 |

**Latencia por consulta** (incluye arranque de proceso)

| Consulta | Mediana | Notas |
|---|---|---|
| `coverage` (overview del registry) | **1,4 ms** | lectura más pequeña — solo mmap del registry |
| `routes` (mapa HTTP de todo el repo) | **142,3 ms** | enumera declarativas + imperativas |
| `coverage --detailed` (frameworks + blind-spots) | **143,4 ms** | registry completo + scoring por framework |
| `impact <symbol> --direction down` | **145,0 ms** | BFS sobre Calls / Extends |
| `inspect <symbol>` (firma + callers + callees) | **145,6 ms** | resolución de símbolo + 1-hop |
| `find <name> --mode bm25` (búsqueda léxica) | **154,5 ms** | consulta Tantivy + partición en 5 cubos |
| `cypher 'MATCH (a:Class)-[:HasMethod]->(b:Method) ...'` | **161,5 ms** | un patrón, una fila |
| `cypher 'MATCH (a:Method)-[:Calls]->(b:Method) ...'` | **174,2 ms** | patrón más amplio, más coincidencias |
| `impact --baseline HEAD~1` (blast radius del changeset) | **359,0 ms** | git diff + parse paralelo por archivo + BFS |

Reproducir: `python scripts/benchmark/benchmark_ecp.py`.

</details>

---

## ── vs. upstream gitnexus ──

Mismo modelo conceptual, audiencia distinta. `ecp` **no** es un reemplazo drop-in — elige según quién lee el grafo.

| Dimensión | EgentCodePlexus | GitNexus |
|---|---|---|
| Consumidor primario | Agentes de código autónomos | Devs humanos + integración IDE |
| Runtime | CLI one-shot sin estado (cero calentamiento) | MCP server de larga duración |
| Rendimiento | **< 2,5 s cold index / < 150 ms query** | ~60 s cold index / ~400 ms query |
| Arista sin resolver | Registro `BlindSpot` (desconocimiento honesto) | Conjetura heurística |
| Salida por defecto | TOON / JSON compacto (token-barato) | Renderizado wiki / UI |
| Lenguajes | 31 (14 profundos + 17 estructurales) | 14 (profundos, 9 dimensiones) |
| Almacenamiento | Rust + `rkyv` zero-copy mmap | Node.js + LadybugDB |

Desglose completo de las 8 dimensiones + matriz de decisión → [docs/vs-gitnexus.md](../vs-gitnexus.md).

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

Eso es todo el round-trip — un proceso, un mmap, ~140 ms. Los comandos de lectura aceptan `--format text|json|toon`; el default por comando es la codificación más barata en tokens.

---

## ── instalación ──

Los binarios precompilados se publican con cada GitHub Release. Los scripts del instalador caen a una compilación cargo desde fuente solo si no hay un asset de release disponible.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Vía cargo directo (mismo source build, sin wrapper del instalador)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

<details>
<summary>Source build con CPU tuneado</summary>

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

</details>

---

## ── inicio rápido ──

```bash
# 1. Indexa el repo actual (incremental; la primera query también auto-indexa)
ecp admin index --repo .

# 2. Localiza un símbolo — nombre exacto por defecto
ecp find loginUser
ecp find login --mode bm25       # ranking BM25, top-K en cubos source/tests/ref/doc/config

# 3. Blast radius — ¿quién se rompe si cambio esto?
ecp impact validateUser --direction upstream

# 4. Contexto completo del símbolo (firma, body, callers, callees, 1-hop impact)
ecp inspect validateUser

# 5. Todas las rutas HTTP del repo (declarativas @Get + imperativas app.get())
ecp routes
ecp routes /api/users --method POST     # ruta → handler → cadena de callers
```

---

## ── cli surface ──

Dos niveles — **comandos de agente** en el top level (query / refactor / verify) y **comandos admin** bajo `ecp admin` (registry / hooks / destructivos). Corre `ecp --help` y `ecp admin --help` para las matrices completas de flags.

| Comando | Propósito |
|---|---|
| `inspect <name>` | Un símbolo → metadata, decoradores, firma, callers, callees, 1-hop impact |
| `find <pattern>` | Localiza símbolos — exact (default) · `--mode fuzzy` substring · `--mode bm25` ranking léxico; bm25 particiona la salida en cubos source / tests / reference / document / config |
| `impact <name> --direction <up\|down>` | Traversal de blast-radius con filtrado por confianza. `--baseline <ref>` para impact del changeset. |
| `rename --symbol <old> --new-name <new>` | Rename AST-aware multi-archivo en 14 lenguajes. Siempre `--dry-run` primero. |
| `cypher '<query>'` | Escape hatch openCypher; `m.content` devuelve el cuerpo del fuente. |
| `coverage` | Overview del registry, cobertura por framework, catálogo de blind-spots, frescura del grafo. |
| `routes [<path>]` | Enumera rutas HTTP (declarativas + imperativas); con `<path>` muestra handler + callers. |
| `contracts` | Inventario cross-repo de contratos API (routes / queue / RPC). |
| `diff` | Delta del resolver — binding tier-degradation + cambios de routes / contracts a nivel de arista. |
| `tool-map` | Llamadas a clients externos HTTP / DB / Redis / queue vía análisis de import-binding por archivo. |
| `shape-check` | Drift entre patrones de acceso del consumer HTTP y la forma de respuesta de la Route. |
| `peers` | Colaboración multi-sesión entre pares (status / diff / log / gc). |
| `review` | Agregador de auditoría LLM-workflow — impact + coverage + tool-map + shape-check + diff, filtrado a señales de alta confianza. |

<details>
<summary><b>Namespace admin</b> — <code>ecp admin &lt;cmd&gt;</code> (registry / hooks / destructivos)</summary>

| Comando | Propósito |
|---|---|
| `index --repo <path>` | Construye / refresca el grafo; incremental vía caché de contenido xxh3_64. `--force` para rebuild completo. |
| `drop / prune / rename-branch` | Ciclo de vida del índice: eliminar, podar dirs de branch obsoletas, renombrar branch on-disk. |
| `install-hook` | Instala el git reference-transaction hook (auto-tracking de cambios de branch). |
| `config` | Wizard TOML interactivo para `.ecp/config.toml`. |
| `mcp serve` / `mcp tools` | MCP server (stdio) para LLM hosts; `tools` lista la superficie de tools expuesta. |
| `claude install / codex install / gemini install` | Integración scripteable de host (skills, hooks, entradas MCP). |
| `verify-resolver` | Diff del dump del resolver contra un oracle de lenguaje (QA para ecp-dev). |

</details>

Todos los comandos resuelven `.ecp/graph.bin` desde el CWD a menos que se pase `--graph <path>`. Los comandos agent-facing son no-interactivos por diseño — cada flag sale en `--help`, cada stream de salida es parseable. Ejecuta `ecp admin` sin subcomando para abrir el TUI admin interactivo.

---

## ── MCP server ──

`ecp` incluye un MCP server que expone los comandos core como tools MCP. Los hosts que hablan MCP (Claude Code, Cursor, Windsurf, Cline, Codex CLI, Gemini CLI) pueden registrar `ecp` y llamar los tools de forma autónoma.

```bash
ecp admin mcp tools          # inspecciona qué tools se van a exponer
ecp admin mcp serve          # corre el server (modo spawn por defecto)
```

Ejemplo de configuración manual del host para Claude Code (`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

Ruta progresiva para operadores humanos:

```text
ecp admin → Agent Integrations → MCP → <host> → install
```

Ruta scripteada para agentes IA:

```bash
ecp admin claude install mcp-server
ecp admin gemini install skills
```

<details>
<summary><b>Integración nativa de Codex CLI</b> (separada de MCP — prepara un patch para un fork de openai/codex)</summary>

La ruta nativa de Codex no edita la instalación de Codex en uso; escribe un patch que aplicas a un fork de `openai/codex`.

Ruta progresiva:

```text
ecp admin → Agent Integrations → Codex CLI → install → native-tools
```

Skills incluidos (misma ruta progresiva):

```text
ecp admin → Agent Integrations → Codex CLI → install → skills → all | ecp | simplify
```

Ruta scripteada para agentes:

```bash
ecp admin codex install native-tools
ecp admin codex install skills all
ecp admin codex install skills ecp
ecp admin codex install skills simplify
```

Los skills incluidos enseñan selección de flujo de trabajo que el command help no puede inferir:

| Skill | Cuándo usar |
|---|---|
| `ecp` | El agente necesita decidir si flujos graph-aware de symbol / impact / route / contract / rename son mejores que grep / lectura de archivos. |
| `simplify` | El agente está revisando código cambiado y debe empezar por `ecp impact`, blind spots, egress, shape drift y resolver deltas antes de leer diffs crudos. |

El componente `native-tools` escribe:

```text
~/.config/ecp/host-integration/codex-cli.patch
```

Aplícalo en tu fork de Codex CLI:

```bash
cd /path/to/openai-codex-fork
git apply ~/.config/ecp/host-integration/codex-cli.patch
```

Para verificar un fork que ya tiene el marcador nativo — fija `ECP_CODEX_CLI_CHECKOUT` antes de consultar status:

```bash
ECP_CODEX_CLI_CHECKOUT=/path/to/openai-codex-fork ecp admin codex status
ecp admin codex uninstall native-tools
ecp admin codex uninstall skills all
```

</details>

---

## ── arquitectura ──

```
crates/
├── ecp-core        Grafo zero-copy (rkyv + mmap), caché incremental, queries de grafo
├── ecp-analyzer    Parsers tree-sitter, detector de rutas HTTP, confianza por framework
├── ecp-mcp         MCP server (stdio) — expone comandos core como tools
└── ecp-cli         Binario `ecp`, motor BM25 de Tantivy, output optimizado para tokens
```

Parse → resolve → serialize corre a través de un canal MPSC hacia un único builder thread que ensambla el grafo y escribe un `.ecp/graph.bin` zero-copy. Las rutas de lectura (`inspect`, `cypher`, `impact`, …) hacen mmap directo de este archivo. La caché de contenido xxh3_64 mantiene los rebuilds incrementales en sub-segundo sobre un repo de 22k archivos.

---

## ── cobertura de lenguajes ──

31 lenguajes parseados a nivel estructural (funciones / clases / métodos / imports / calls). 14 de ellos — el conjunto original de GitNexus — obtienen cobertura full-depth en imports, named bindings, exports, herencia, types, constructores, config, frameworks, entry points, calls y rename. Los 17 restantes son structural-only (Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig).

📊 [Matriz completa de capacidades por lenguaje](../language-matrix.md) — status y rationale por lenguaje.

---

## ── tuning ──

| Variable de entorno | Default | Efecto |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Salta archivos source mayores a esto durante la ingesta. Limita el peor caso de RAM por worker a `num_threads × MAX`. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | Profundidad de recursión de directorios para descubrir `*.csproj`. Súbelo para monorepos .NET muy anidados. |

---

## ── licencia ──

Licenciado bajo [PolyForm Noncommercial 1.0.0](../../LICENSE.md). Uso personal, investigación, proyectos hobby y organizaciones sin fines de lucro están explícitamente permitidos. **El uso comercial no está concedido por esta licencia** — contacta al autor upstream de GitNexus [Abhigyan Patwari](https://github.com/abhigyanpatwari) para derechos comerciales. Atribución requerida: [NOTICES.md](../../LICENSES/NOTICES.md).

<details>
<summary><b>Construido sobre</b> (agradecimientos)</summary>

- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — diseño original, superficie CLI, modelo conceptual
- [tree-sitter](https://tree-sitter.github.io/) — parsing AST incremental
- [rkyv](https://rkyv.org/) — framework de deserialización zero-copy
- [Tantivy](https://github.com/quickwit-oss/tantivy) — motor de búsqueda BM25 en Rust
- [Rayon](https://github.com/rayon-rs/rayon) — paralelismo de datos para parsing AST multinúcleo
- [xxhash (xxh3_64)](https://xxhash.com/) — hashing para indexación incremental basada en contenido
- [DashMap](https://github.com/xacrimon/dashmap) — hash maps concurrentes para ensamblaje del grafo
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — memory mapping zero-copy
- [msgspec](https://github.com/jcrist/msgspec) — serialización JSON rápida para IPC

El onboarding para agentes IA (bootstrap por URL, skill de Claude Code, instalación de plugin) vive en `docs/skills/ecp-onboard/`. Invariantes de concurrencia y cómo re-verificarlos: `./scripts/audit/audit-concurrency.sh`.

</details>

---

## ── estado de release ──

La ruta de instalación verificada actualmente es `cargo install --git ...`, que compila `ecp` desde fuente. Los release installers ya contienen el flujo de verificación de checksum y procedencia, pero requieren un tag publicado y release assets antes de que la ruta de descarga del binario pueda ser end-to-end verificada. El skill de onboarding orientado a agentes en [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md) guía a los usuarios por install, first-index, groups opcionales, wiring de MCP y siguientes pasos — el flujo de setup asistido sigue refinándose.

---

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)
