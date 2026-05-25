<div align="center">

# `ecp` · EgentCodePlexus

### El grafo de código estructural construido para agentes de IA, no para humanos.

*22k archivos indexados en 2.6 s · cualquier consulta respondida en &lt;175 ms · incógnitas honestas, nunca aristas alucinadas.*

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
![Cold index 2.6s](https://img.shields.io/badge/cold_index-2.6s%20%2F%2022k%20files-brightgreen)
![Query latency](https://img.shields.io/badge/query-%3C175ms%20cold-blue)
![Languages](https://img.shields.io/badge/languages-31%20parsed-orange)
![License](https://img.shields.io/badge/license-PolyForm%20NC-lightgrey)
![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange?logo=rust)
![Status early release](https://img.shields.io/badge/status-early%20release-yellow)

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · **Español** · [Português](./README_pt-BR.md) · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md)

</div>

---

Los agentes de programación autónomos ejecutan **20–50 consultas estructurales por tarea**. Todas esas consultas golpean herramientas construidas para humanos: paneles laterales de IDE, daemons que necesitan calentamiento, salidas formateadas para lectura. La incompatibilidad se manifiesta en tres modos de fallo concretos:

1. **Desperdicio de tokens** — un volcado de `grep` devuelve 400 líneas cuando el agente necesitaba 10 símbolos
2. **Refactorizaciones rotas** — un llamador omitido se cuela porque el resolvedor adivinó mal
3. **Dependencias alucinadas** — cuando el análisis estático no puede alcanzar una arista, la herramienta la inventa

`ecp` fue construido para eliminar los tres.

| Modo de fallo | Respuesta de `ecp` |
|---|---|
| Ventana de contexto colapsada por salida de búsqueda en bruto | **TOON / JSON compacto** — solo símbolos, líneas y aristas; sin relleno |
| Llamador omitido, ruptura silenciosa en cascada | **`impact`** — radio de explosión exacto sobre aristas reales de llamada y extensión |
| Dependencia fabricada en el razonamiento del agente | **Registros `BlindSpot`** — incógnitas honestas tipadas que el agente puede rodear |
| El grafo queda a oscuras fuera del lenguaje principal | **31 lenguajes** — código de servicio, IaC, SQL, contratos inteligentes en un solo recorrido |

---

## 🎯 Principios de diseño

Cada decisión de diseño tiene una sola fuente: *¿qué necesita realmente el agente receptor?*

**La salida es una estructura de datos.** TOON y JSON compacto transportan solo lo que el agente necesita para su próxima decisión. Sin resúmenes en prosa. Sin cromo visual. Sin encabezados de sección que consuman el presupuesto de contexto. Los formatos predeterminados ya son la elección correcta para la mayoría de los prompts de LLM.

**Sin estado. Cero calentamiento.** Cada invocación hace `mmap` de un archivo de grafo `rkyv` de copia cero y termina. **~140–170 ms por consulta, incluido el arranque del proceso.** Sin daemon que mantener vivo. Sin fase de calentamiento. Sin camino de recuperación "el servidor se cayó, por favor reinicia". Un agente puede ejecutar 50 consultas por tarea sin pagar el costo de arranque de un proceso.

**BlindSpot sobre alucinación.** Cuando `ecp` no puede resolver estáticamente un sitio de llamada — dispatch dinámico, reflexión, una importación no resuelta — emite un registro `BlindSpot`: una brecha nombrada, tipada y explícita en el grafo. Los agentes pueden navegar alrededor de una incógnita conocida. No pueden recuperarse de una fabricación confiada.

**Políglota por defecto.** 31 lenguajes a profundidad estructural. Código de servicio, Dockerfiles, GitHub Actions, Terraform, SQL, Move, Solidity — un solo recorrido cubre todas las capas. Sin cambio de lenguaje significa sin punto ciego en el grafo.

🎙️ **[Entrevistas a agentes](../../interviews/README.md)** — Gemini CLI y Codex describen cómo usan `ecp` en flujos de tareas autónomas en vivo.

Construido sobre [GitNexus](https://github.com/abhigyanpatwari/GitNexus) por [Abhigyan Patwari](https://github.com/abhigyanpatwari) — mismo concepto de grafo estructural, reescrito en Rust, audiencia diferente. [PolyForm Noncommercial 1.0.0](../../LICENSE.md); ver [NOTICES.md](../../LICENSES/NOTICES.md) para la atribución requerida.

---

## ⚡ Comprobantes de rendimiento

### Índice en frío 60× más rápido que el GitNexus original

Medido en la base de código TypeScript de [gitnexus](https://github.com/abhigyanpatwari/GitNexus) · `scripts/parity/benchmark_vs_gitnexus.py`:

| Fase | ecp (Rust) | gitnexus (Node) | Aceleración |
|---|---|---|---|
| **Índice en frío** | **~970 ms** | ~58 s | **60×** |
| **Contexto de símbolo** | **~70 ms** | ~430 ms | **6×** |
| **Radio de explosión** | **~70 ms** | ~460 ms | **6×** |
| **Consulta Cypher** | **~70 ms** | ~400 ms | **5×** |

*La latencia de `ecp` incluye el arranque completo del proceso (sin daemon). GitNexus (v1.6.5) medido contra un repositorio ya indexado en caliente.*

### Escala: `.sample_repo` — 22,645 archivos, 25 lenguajes, corpus políglota de 2.1 GB

**Ingesta:**

| Métrica | Valor |
|---|---|
| Archivos indexados | **22,645** en 25 lenguajes detectados |
| Ingesta en frío | **2.60 s** (parseo + resolución + serialización) |
| Ingesta incremental | **4.9 ms** (recorrido hash xxh3_64, cero archivos modificados) |
| Hardware | AMD Ryzen 9 9950X (16 lógicos), 39.2 GiB RAM, Linux 6.6.87 |

**Latencia por consulta, incluido el arranque del proceso:**

| Consulta | Mediana | Qué cubre |
|---|---|---|
| `summary` | **1.4 ms** | mmap del registro — lectura mínima |
| `routes` | **142.3 ms** | enumeración de rutas declarativas e imperativas |
| `summary --detailed` | **143.4 ms** | registro completo + puntuación de confianza por framework |
| `impact --direction down` | **145.0 ms** | BFS sobre aristas Calls / Extends |
| `inspect` | **145.6 ms** | resolución de símbolo + recorrido de 1 salto |
| `find --mode bm25` | **154.5 ms** | consulta Tantivy + partición en 5 cubos |
| `cypher` (estrecho) | **161.5 ms** | un patrón, una fila |
| `cypher` (amplio) | **174.2 ms** | patrón más amplio, más coincidencias |
| `impact --baseline HEAD~1` | **359.0 ms** | diff de git + parseo paralelo por archivo + BFS |

Reproduzca todo: `python scripts/benchmark/benchmark_ecp.py`.

### Comparación con competidores de nivel Rust

`scripts/benchmark/benchmark_vs_competitors.py` compara contra [`codescope`](https://github.com/onur-gokyildiz-bhi/codescope) (respaldado por SurrealDB) y `coraline` (respaldado por SQLite) en 6 fases: `cold-index`, `symbol-find`, `callers`, `file-context`, `route-map`, `cypher`. Fases ausentes → `N/A` (la ausencia es una señal). Los resultados regeneran `docs/benchmark-vs-competitors.md`.

```bash
python scripts/benchmark/benchmark_vs_competitors.py
python scripts/benchmark/benchmark_vs_competitors.py --corpus path/to/repo --iterations 5 --no-plot
```

---

## 🆚 vs. GitNexus original

Mismo concepto de grafo estructural, audiencia diferente. No es un reemplazo directo — elija según quién lee la salida y qué hace con ella.

| Dimensión | EgentCodePlexus | GitNexus |
|---|---|---|
| Consumidor principal | Agentes de código IA autónomos | Desarrolladores humanos + integración IDE |
| Tiempo de ejecución | CLI de un solo disparo sin estado (cero calentamiento) | Servidor MCP de larga duración |
| Rendimiento | **< 2.5s índice frío / < 175ms por consulta** | ~60s índice frío / ~400ms por consulta |
| Arista no resuelta | Registro `BlindSpot` (incógnita honesta) | Suposición heurística |
| Salida predeterminada | TOON / JSON compacto (económico en tokens) | Renderizado wiki / UI |
| Lenguajes | 31 (14 profundos + 17 estructurales) | 14 (profundos, 9 dimensiones) |
| Almacenamiento | Rust + `rkyv` mmap de copia cero | Node.js + LadybugDB |

**Análisis completo, filosofía y matriz de decisión → [docs/vs-gitnexus.md](../vs-gitnexus.md)**

---

## 📦 Instalación

Los binarios precompilados se distribuyen con cada versión de GitHub. Los scripts de instalación recurren a una compilación fuente con cargo solo cuando no hay un activo disponible para la plataforma.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Cargo directo (sin envoltorio de instalador)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

Compilación fuente ajustada a la CPU:

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 Inicio rápido

Sin daemon que iniciar. Sin configuración requerida. Un comando desde cero hasta un grafo consultable.

```bash
# Indexar (incremental; la primera consulta también auto-indexa si el índice está ausente)
ecp admin index --repo .

# Encontrar un símbolo — exacto por defecto
ecp find loginUser
ecp find login --mode bm25            # Ranking BM25, dividido en 5 cubos de salida

# Radio de explosión — ¿quién se rompe si cambio esto?
ecp impact validateUser --direction upstream

# Contexto completo del símbolo (firma, cuerpo, llamadores, llamados, impacto de 1 salto)
ecp inspect validateUser

# Mapa de rutas HTTP (declarativo @Get + imperativo app.get())
ecp routes
ecp routes /api/users --method POST   # ruta → manejador → cadena de llamadores

# Uso de archivos: ¿quién lee / escribe esta ruta?
ecp impact --literal session_meta.json
```

Todos los comandos de solo lectura aceptan `--format text|json|toon`. Los valores predeterminados son los más económicos en tokens por comando (principalmente `toon`; `find` usa `text` por defecto; `cypher`/`summary` usan `json` por defecto).

---

## 🛠️ Superficie CLI

Dos niveles: **comandos de agente** en el nivel superior (consulta / refactorización / verificación) y **comandos de administración** bajo `ecp admin` (registro / hooks / destructivos). Ejecute `ecp --help` y `ecp admin --help` para las matrices completas de opciones.

**Comandos de agente:**

| Comando | Propósito |
|---|---|
| `inspect <name>` | Símbolo → metadatos, decoradores, firma, llamadores, llamados, impacto de 1 salto, métodos / propiedades / variantes de enum contenidos |
| `find <pattern>` | Exacto · `--mode fuzzy` · `--mode bm25` (5 cubos: fuente / pruebas / referencia / documento / config) |
| `find-schema-bindings <field>` | Aristas heurísticas MirrorsField + candidatos de blind-spot entre clases / servicios |
| `find-transaction-patterns [--class <Name>]` | Pares de nombres Saga compensate/undo/rollback; ≥0.75 → POSSIBLY_RELATED, <0.75 → BLIND_SPOT |
| `impact <name> --direction <up\|down>` | BFS de radio de explosión con filtrado de confianza; `--since <ref>` para impacto de conjunto de cambios |
| `rename --symbol <old> --new-name <new>` | Renombrado multi-archivo con conciencia de AST en 14 lenguajes. Siempre `--dry-run` primero. |
| `cypher '<query>'` | Escape hatch de openCypher; `m.content` devuelve el cuerpo de la fuente |
| `summary` | Resumen del registro, cobertura de frameworks, catálogo de blind-spots accionable por LLM, frescura del grafo |
| `routes [<path>]` | Enumeración de rutas HTTP (declarativa + imperativa); con `<path>`: manejador + cadena de llamadores |
| `contracts` | Inventario de contratos de API entre repositorios (rutas / cola / RPC) |
| `diff` | Delta del resolvedor: degradación de nivel de enlace + cambios de ruta / contrato |
| `tool-map` | Sitios de llamada externos HTTP / DB / Redis / cola mediante análisis de enlace de importación |
| `shape-check` | Deriva entre patrones de acceso del consumidor HTTP y formas de respuesta de ruta |
| `peers` | Colaboración multi-sesión: `status / diff / say / inbox / log / thread / watch / gc` |
| `review` | Auditoría de un solo disparo: impact + summary + tool-map + shape-check + diff, solo señales de alta confianza |

**Comandos de administración** (`ecp admin <cmd>`):

| Comando | Propósito |
|---|---|
| `index --repo <path>` | Construir / actualizar el grafo; incremental mediante caché de contenido xxh3_64. `--force` para reconstrucción completa. |
| `drop / prune / rename-branch` | Ciclo de vida del índice: eliminar, podar directorios de rama obsoletos, renombrar rama en disco |
| `install-hook` | Hook de transacción de referencia Git (auto-rastrea cambios de rama) |
| `config` | Asistente TOML interactivo para `.ecp/config.toml` |
| `mcp serve` / `mcp tools` | Servidor MCP (stdio); `tools` lista la superficie expuesta |

Todos los comandos resuelven `.ecp/graph.bin` desde el directorio actual a menos que se proporcione `--graph <path>`. Cada comando orientado al agente es no interactivo; cada flujo de salida es parseable.

### Sincronización de pares multi-sesión

Cuando múltiples sesiones de LLM editan el mismo repositorio en paralelo, `ecp peers` expone el estado sucio a nivel de símbolo de cada sesión y habilita la mensajería directa entre sesiones. Regístrese mediante `ECP_SESSION_ID`, `CODEX_SESSION_ID`, `CODEX_THREAD_ID`, o `CLAUDE_CODE_SESSION_ID`.

```bash
# Iniciar el observador (uno por sesión; necesario para eventos push al buzón)
ecp peers watch --start

# ¿Quién más está editando ahora mismo?
ecp peers status                                  # text
ecp peers status --format json                    # {session_id, pid, watcher: alive|dead|not-started}

# Inspeccionar los símbolos sucios de un par
ecp peers diff <peer-session-id> [<symbol>]

# Enviar mensajes
ecp peers say "rebasing on main, hold pushes 5min"    # difusión
ecp peers say --to <peer-session-id> "take auth.rs?"  # dirigido

# Leer y gestionar
ecp peers inbox
ecp peers log --limit 20
ecp peers thread <msg-id>

# Limpieza
ecp peers watch --stop && ecp peers gc
```

El campo `watcher` distingue `alive` | `dead` | `not-started` — los cuelgues no se disfrazan de "función no utilizada".

### Veredictos de revisión de código demostrables

`ecp review --verdicts` precalcula veredictos respaldados por el grafo a partir de secciones de `ecp diff`. Pase el JSON directamente como contexto de revisión — omita la re-derivación por el LLM de las relaciones de llamador a partir de un diff en bruto.

```bash
ecp review --since main --verdicts --format json
```

| Severidad | Regla |
|---|---|
| `RISK` | Existen llamadores entre archivos, símbolo público eliminado, o blindspot en la región del diff |
| `WARN` | Solo llamadores dentro del archivo, o ruta modificada |
| `INFO` | No se encontraron llamadores, o nueva superficie pública añadida |

Tipos de veredicto: `SIGNATURE_OR_BODY_CHANGED` · `NEW_PUBLIC_SURFACE` · `REMOVED_PUBLIC_SURFACE` · `ROUTE_CONTRACT_CHANGED` · `BLINDSPOT_IN_DIFF_REGION`

Cada veredicto cita la sección exacta del diff y el hecho del grafo que lo desencadenó. Especificación completa: [docs/specs/2026-05-22-review-verdicts.md](../specs/2026-05-22-review-verdicts.md).

---

## 🔌 Integración con agentes

**Prefiera la ruta nativa** donde esté disponible — conecta hooks de auto-reindexación y habilidades de flujo de trabajo que enseñan al agente *cuándo* las consultas al grafo valen el viaje de ida y vuelta. **MCP es la alternativa universal** para cualquier host que hable el protocolo.

| Agente | Ruta | Conecta |
|---|---|---|
| Claude Code | nativa | hooks + skills + MCP opcional |
| Codex CLI | nativa | skills (herramientas nativas pendientes) |
| Gemini CLI | nativa | skill nativa **o** MCP |
| Cursor · Windsurf · Cline · Copilot · cualquier host MCP | MCP | Servidor MCP |

Configuración guiada: `ecp admin → Agent Integrations → <host>`. Ruta scriptable para automatización: `ecp admin <host> install <component>`. Inspeccionar cualquier host: `ecp admin <host> status`.

### Claude Code

```bash
ecp admin claude install hooks          # settings.json: auto-reindexación + enriquecimiento de contexto
ecp admin claude install skills all     # paquetes de habilidades ecp + simplify (o: ecp | simplify)
ecp admin claude install mcp-server     # opcional — hooks + skills + CLI ya son suficientes
```

Los hooks alimentan contexto del grafo en cada Grep/Glob/Bash sin una llamada de herramienta explícita. La habilidad `ecp` enseña flujos de trabajo de símbolo / impacto / ruta / contrato / renombrado. `simplify` impulsa la revisión de código centrada en el grafo.

### Gemini CLI

```bash
ecp admin gemini install native-skill   # vincula mediante `gemini skills link`
ecp admin gemini install mcp-server     # registra mediante `gemini mcp add`
```

`native-skill` y `mcp-server` son mutuamente excluyentes — instalar uno elimina el otro.

### Codex CLI

```bash
ecp admin codex install skills all      # ecp + simplify; herramientas nativas pendientes de cableado en Codex
```

**Habilidades de flujo de trabajo:**

| Habilidad | Úsela cuando |
|---|---|
| `ecp` | El agente decide si los flujos de trabajo con conciencia de grafo superan a grep / lecturas de archivos para símbolos, llamadores, rutas, contratos |
| `simplify` | Revisión de código comenzando desde impacto de ecp, blind spots, egreso, deriva de forma, deltas del resolvedor |

### Alternativa MCP (Cursor, Windsurf, Cline, cualquier host MCP)

| Host | Archivo de configuración |
|---|---|
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Cline (VS Code) | `cline_mcp_settings.json` (panel MCP → "Edit MCP Settings") |
| Host MCP genérico | específico del host |

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

```bash
ecp admin mcp tools    # verificar la superficie expuesta antes de conectar
ecp admin mcp serve    # sin estado, un solo disparo por llamada (sin costo de calentamiento)
```

---

## 🏗️ Arquitectura

```
crates/
├── ecp-core        # Grafo de copia cero (rkyv + mmap), caché incremental, consultas de grafo
├── ecp-analyzer    # Parsers tree-sitter, detector de rutas HTTP, confianza de frameworks
├── ecp-mcp         # Servidor MCP (stdio) — expone comandos principales como herramientas
└── ecp-cli         # Binario `ecp`, motor BM25 Tantivy, salida optimizada en tokens
```

Parseo → resolución → serialización fluye a través de un canal MPSC hacia un único hilo constructor que ensambla el grafo y escribe un `.ecp/graph.bin` de copia cero. Las rutas de lectura (`inspect`, `cypher`, `impact`, …) hacen mmap de este archivo directamente — sin paso de deserialización. La caché de contenido xxh3_64 mantiene las reconstrucciones incrementales por debajo de un segundo en un repositorio de 22k archivos.

---

## 🌐 Cobertura de lenguajes

31 lenguajes parseados a nivel estructural. **14 de profundidad completa** (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) — importaciones, enlaces nombrados, exportaciones, herencia, tipos, constructores, configuración, frameworks, puntos de entrada, llamadas y renombrado. **17 solo estructurales**: Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig.

📊 **[Matriz completa de capacidades de lenguajes](../language-matrix.md)** — estado y justificación por lenguaje.

---

## ⚙️ Ajuste

| Variable de entorno | Predeterminado | Efecto |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Omitir archivos fuente por encima de este tamaño durante la ingesta. Limita la RAM máxima del trabajador a `num_threads × MAX`. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | Profundidad de recursión para descubrir `*.csproj`. Aumentar para monorepos .NET profundamente anidados. |

---

## 📜 Licencia y reconocimientos

[PolyForm Noncommercial 1.0.0](../../LICENSE.md). Uso personal, investigación, proyectos de hobby y organizaciones sin fines de lucro explícitamente permitidos. **El uso comercial no está concedido por esta licencia** — contacte al autor original de GitNexus, Abhigyan Patwari, para derechos comerciales.

Construido sobre:
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — diseño original, superficie CLI y modelo conceptual
- [tree-sitter](https://tree-sitter.github.io/) — parseo AST incremental robusto
- [rkyv](https://rkyv.org/) — framework de deserialización de copia cero
- [Tantivy](https://github.com/quickwit-oss/tantivy) — motor de búsqueda de texto completo
- [Rayon](https://github.com/rayon-rs/rayon) — paralelismo de datos para parseo AST concurrente multi-núcleo
- [xxhash (xxh3_64)](https://xxhash.com/) — hashing no criptográfico para indexación incremental basada en contenido
- [DashMap](https://github.com/xacrimon/dashmap) — mapas hash concurrentes para ensamblaje del grafo
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — mapeo de memoria de copia cero para acceso al grafo en submilisegundos
- [msgspec](https://github.com/jcrist/msgspec) — serialización JSON de alto rendimiento para comunicación entre procesos

Incorporación de agentes (bootstrap de URL, habilidad de Claude Code, instalación de plugin): `docs/skills/ecp-onboard/`. Invariantes de concurrencia y re-verificación: `../../scripts/audit/audit-concurrency.sh`.

## 🚦 Estado de la versión

Ruta de instalación verificada: `cargo install --git ...`, que compila `ecp` desde el código fuente. Los instaladores de versiones ya contienen el flujo de verificación de checksum y procedencia, pero requieren un tag publicado y activos de versión antes de que la ruta de descarga del binario esté verificada de extremo a extremo. Habilidad de incorporación orientada al agente: [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md). El flujo de configuración/instalación asistida aún se está refinando.

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

</div>
