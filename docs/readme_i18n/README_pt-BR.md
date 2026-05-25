<div align="center">

# `ecp` · EgentCodePlexus

### O grafo estrutural de código criado para agentes de IA, não para humanos.

*22 mil arquivos indexados em 2,6 s · qualquer consulta respondida em &lt;175 ms · desconhecidos honestos, nunca arestas alucinadas.*

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/coseto6125/egent-code-plexus/badge)](https://scorecard.dev/viewer/?uri=github.com/coseto6125/egent-code-plexus)
![Cold index 2.6s](https://img.shields.io/badge/cold_index-2.6s%20%2F%2022k%20files-brightgreen)
![Query latency](https://img.shields.io/badge/query-%3C175ms%20cold-blue)
![Languages](https://img.shields.io/badge/languages-31%20parsed-orange)
![License](https://img.shields.io/badge/license-PolyForm%20NC-lightgrey)
![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange?logo=rust)
![Status early release](https://img.shields.io/badge/status-early%20release-yellow)

[English](../../README.md) · [繁體中文](./README_zh-TW.md) · [简体中文](./README_zh-CN.md) · [日本語](./README_ja.md) · [한국어](./README_ko.md) · [Español](./README_es.md) · **Português** · [Русский](./README_ru.md) · [हिन्दी](./README_hi.md)

</div>

---

Agentes de codificação autônomos disparam **20–50 consultas estruturais por tarefa**. Todas essas consultas atingem ferramentas criadas para humanos: painéis laterais de IDE, daemons que precisam de aquecimento, saída formatada para leitura. Essa incompatibilidade se manifesta em três modos concretos de falha:

1. **Desperdício de tokens** — um dump de `grep` retorna 400 linhas quando o agente precisava de 10 símbolos
2. **Refatorações quebradas** — um chamador ignorado passa despercebido porque o resolvedor adivinhou errado
3. **Dependências alucinadas** — quando a análise estática não consegue alcançar uma aresta, a ferramenta a inventa

O `ecp` foi criado para eliminar os três.

| Modo de falha | Resposta do `ecp` |
|---|---|
| Janela de contexto esgotada com saída de busca bruta | **TOON / JSON compacto** — apenas símbolos, linhas e arestas; sem preenchimento |
| Chamador ignorado, quebra silenciosa a jusante | **`impact`** — raio de impacto exato sobre arestas reais de chamada e extensão |
| Dependência fabricada no raciocínio do agente | **Registros `BlindSpot`** — desconhecidos honestos tipados que o agente pode contornar |
| Grafo apagado fora da linguagem principal | **31 linguagens** — código de serviço, IaC, SQL, contratos inteligentes em uma única travessia |

---

## 🎯 Princípios de design

Cada decisão de design tem uma única fonte: *o que o agente receptor realmente precisa?*

**A saída é uma estrutura de dados.** TOON e JSON compacto carregam apenas o que o agente precisa para sua próxima decisão. Sem resumos em prosa. Sem decoração visual. Sem cabeçalhos de seção consumindo o orçamento de contexto. Os formatos padrão já são a escolha certa para a maioria dos prompts de LLM.

**Sem estado. Sem aquecimento.** Cada invocação faz `mmap` de um arquivo de grafo `rkyv` de cópia zero e encerra. **~140–170 ms por consulta, incluindo a inicialização do processo.** Sem daemon para manter ativo. Sem fase de aquecimento. Sem caminho de recuperação do tipo "servidor travou, reinicie". Um agente pode disparar 50 consultas por tarefa sem pagar o custo de inicialização de processo.

**BlindSpot em vez de alucinação.** Quando o `ecp` não consegue resolver estaticamente um ponto de chamada — despacho dinâmico, reflexão, importação não resolvida — ele emite um registro `BlindSpot`: uma lacuna nomeada, tipada e explícita no grafo. Agentes conseguem navegar em torno de um desconhecido conhecido. Eles não conseguem se recuperar de uma fabricação confiante.

**Poliglota por padrão.** 31 linguagens em profundidade estrutural. Código de serviço, Dockerfiles, GitHub Actions, Terraform, SQL, Move, Solidity — uma única travessia cobre todas as camadas. Sem troca de linguagem significa sem ponto cego no grafo.

🎙️ **[Entrevistas com Agentes](../../interviews/README.md)** — Gemini CLI e Codex descrevem como utilizam o `ecp` em fluxos de tarefas autônomas ao vivo.

Construído sobre o [GitNexus](https://github.com/abhigyanpatwari/GitNexus) por [Abhigyan Patwari](https://github.com/abhigyanpatwari) — mesmo conceito de grafo estrutural, reescrito em Rust, público diferente. [PolyForm Noncommercial 1.0.0](../../LICENSE.md); veja [NOTICES.md](../../LICENSES/NOTICES.md) para atribuição obrigatória.

---

## ⚡ Comprovantes de desempenho

### 60× mais rápido no índice a frio vs. GitNexus original

Medido no codebase TypeScript do [gitnexus](https://github.com/abhigyanpatwari/GitNexus) · `../../scripts/parity/benchmark_vs_gitnexus.py`:

| Fase | ecp (Rust) | gitnexus (Node) | Aceleração |
|---|---|---|---|
| **Índice a frio** | **~970 ms** | ~58 s | **60×** |
| **Contexto de símbolo** | **~70 ms** | ~430 ms | **6×** |
| **Raio de impacto** | **~70 ms** | ~460 ms | **6×** |
| **Consulta Cypher** | **~70 ms** | ~400 ms | **5×** |

*A latência do `ecp` inclui a inicialização completa do processo (sem daemon). GitNexus (v1.6.5) medido em um repositório com índice aquecido.*

### Escala: `.sample_repo` — 22.645 arquivos, 25 linguagens, corpus poliglota de 2,1 GB

**Ingestão:**

| Métrica | Valor |
|---|---|
| Arquivos indexados | **22.645** em 25 linguagens detectadas |
| Ingestão a frio | **2,60 s** (parse + resolve + serialize) |
| Ingestão incremental | **4,9 ms** (percurso de hash xxh3_64, zero arquivos sujos) |
| Hardware | AMD Ryzen 9 9950X (16 lógicos), 39,2 GiB de RAM, Linux 6.6.87 |

**Latência por consulta, incluindo inicialização do processo:**

| Consulta | Mediana | O que cobre |
|---|---|---|
| `summary` | **1,4 ms** | mmap do registro — menor leitura |
| `routes` | **142,3 ms** | enumeração de rotas declarativa + imperativa |
| `summary --detailed` | **143,4 ms** | registro completo + pontuação de confiança por framework |
| `impact --direction down` | **145,0 ms** | BFS sobre arestas Calls / Extends |
| `inspect` | **145,6 ms** | resolução de símbolo + travessia de 1 salto |
| `find --mode bm25` | **154,5 ms** | consulta Tantivy + partição em 5 buckets |
| `cypher` (restrito) | **161,5 ms** | um padrão, uma linha |
| `cypher` (amplo) | **174,2 ms** | padrão mais amplo, mais correspondências |
| `impact --baseline HEAD~1` | **359,0 ms** | git diff + parse paralelo por arquivo + BFS |

Reproduza tudo: `python ../../scripts/benchmark/benchmark_ecp.py`.

### Comparação com concorrentes de nível Rust

`../../scripts/benchmark/benchmark_vs_competitors.py` faz benchmark contra [`codescope`](https://github.com/onur-gokyildiz-bhi/codescope) (com SurrealDB) e `coraline` (com SQLite) em 6 fases: `cold-index`, `symbol-find`, `callers`, `file-context`, `route-map`, `cypher`. Fases ausentes → `N/A` (ausência é um sinal). Os resultados regeneram `docs/benchmark-vs-competitors.md`.

```bash
python scripts/benchmark/benchmark_vs_competitors.py
python scripts/benchmark/benchmark_vs_competitors.py --corpus path/to/repo --iterations 5 --no-plot
```

---

## 🆚 vs. GitNexus original

Mesmo conceito de grafo estrutural, público diferente. Não é um substituto direto — escolha com base em quem lê a saída e o que faz com ela.

| Dimensão | EgentCodePlexus | GitNexus |
|---|---|---|
| Consumidor principal | Agentes de IA autônomos de código | Desenvolvedores humanos + integração com IDE |
| Tempo de execução | CLI sem estado de disparo único (sem aquecimento) | Servidor MCP de longa duração |
| Desempenho | **< 2,5 s índice a frio / < 175 ms consulta** | ~60 s índice a frio / ~400 ms consulta |
| Aresta não resolvida | Registro `BlindSpot` (desconhecido honesto) | Estimativa heurística |
| Saída padrão | TOON / JSON compacto (econômico em tokens) | Renderização Wiki / UI |
| Linguagens | 31 (14 profundas + 17 estruturais) | 14 (profundas, 9 dimensões) |
| Armazenamento | Rust + `rkyv` mmap de cópia zero | Node.js + LadybugDB |

**Análise completa, filosofia e matriz de decisão → [docs/vs-gitnexus.md](../vs-gitnexus.md)**

---

## 📦 Instalação

Binários pré-compilados são fornecidos com cada GitHub Release. Os scripts do instalador recorrem a uma build cargo a partir do código-fonte apenas quando nenhum asset compatível está disponível.

```bash
# Linux / macOS
curl -sSfL https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.sh | sh

# Windows PowerShell
iwr https://github.com/coseto6125/egent-code-plexus/releases/latest/download/install.ps1 -UseBasicParsing | iex

# Cargo direto (sem wrapper do instalador)
cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked
```

Build a partir do código-fonte com otimização para CPU:

```bash
repo=https://github.com/coseto6125/egent-code-plexus
RUSTFLAGS="-C target-cpu=native" cargo install --git "$repo" egent-code-plexus --bin ecp --locked --profile release-dist
```

---

## 🚀 Início rápido

Sem daemon para iniciar. Sem configuração necessária. Um comando do zero até um grafo consultável.

```bash
# Indexar (incremental; a primeira consulta também indexa automaticamente se o índice estiver ausente)
ecp admin index --repo .

# Encontrar um símbolo — exato por padrão
ecp find loginUser
ecp find login --mode bm25            # ranking BM25, particionado em 5 buckets de saída

# Raio de impacto — o que quebra se eu alterar isso?
ecp impact validateUser --direction upstream

# Contexto completo do símbolo (assinatura, corpo, chamadores, chamados, impacto de 1 salto)
ecp inspect validateUser

# Mapa de rotas HTTP (declarativo @Get + imperativo app.get())
ecp routes
ecp routes /api/users --method POST   # cadeia rota → handler → chamador

# Uso de arquivo: quem lê / escreve neste caminho?
ecp impact --literal session_meta.json
```

Todos os comandos de leitura aceitam `--format text|json|toon`. Os padrões são os mais econômicos em tokens por comando (majoritariamente `toon`; `find` usa `text` por padrão; `cypher`/`summary` usam `json` por padrão).

---

## 🛠️ Interface da CLI

Dois níveis: **comandos de agente** no nível superior (consultar / refatorar / verificar) e **comandos de administração** sob `ecp admin` (registro / hooks / destrutivos). Execute `ecp --help` e `ecp admin --help` para as matrizes completas de flags.

**Comandos de agente:**

| Comando | Finalidade |
|---|---|
| `inspect <name>` | Símbolo → metadados, decoradores, assinatura, chamadores, chamados, impacto de 1 salto, métodos / propriedades / variantes de enum contidos |
| `find <pattern>` | Exato · `--mode fuzzy` · `--mode bm25` (5 buckets: source / tests / reference / document / config) |
| `find-schema-bindings <field>` | Arestas heurísticas MirrorsField + candidatos a blind-spot entre classes / serviços |
| `find-transaction-patterns [--class <Name>]` | Pares de nomes compensate/undo/rollback de Saga; ≥0.75 → POSSIBLY_RELATED, <0.75 → BLIND_SPOT |
| `impact <name> --direction <up\|down>` | BFS de raio de impacto com filtragem por confiança; `--since <ref>` para impacto de conjunto de mudanças |
| `rename --symbol <old> --new-name <new>` | Renomeação multi-arquivo com reconhecimento de AST em 14 linguagens. Sempre use `--dry-run` primeiro. |
| `cypher '<query>'` | Saída de escape openCypher; `m.content` retorna o corpo do código-fonte |
| `summary` | Visão geral do registro, cobertura de frameworks, catálogo de blind-spots acionável por LLM, atualidade do grafo |
| `routes [<path>]` | Enumeração de rotas HTTP (declarativa + imperativa); com `<path>`: cadeia handler + chamador |
| `contracts` | Inventário de contratos de API entre repositórios (routes / queue / RPC) |
| `diff` | Delta do resolvedor: degradação de tier de binding + mudanças em rotas / contratos |
| `tool-map` | Pontos de chamada externos HTTP / DB / Redis / queue via análise de binding de importação |
| `shape-check` | Desvio entre padrões de acesso do consumidor HTTP e formatos de resposta de Route |
| `peers` | Colaboração multi-sessão: `status / diff / say / inbox / log / thread / watch / gc` |
| `review` | Auditoria única: impact + summary + tool-map + shape-check + diff, apenas sinais de alta confiança |

**Comandos de administração** (`ecp admin <cmd>`):

| Comando | Finalidade |
|---|---|
| `index --repo <path>` | Construir / atualizar o grafo; incremental via cache de conteúdo xxh3_64. `--force` para rebuild completo. |
| `drop / prune / rename-branch` | Ciclo de vida do índice: deletar, remover diretórios de branch obsoletos, renomear branch em disco |
| `install-hook` | Hook de transação de referência Git (rastreia automaticamente trocas de branch) |
| `config` | Assistente TOML interativo para `.ecp/config.toml` |
| `mcp serve` / `mcp tools` | Servidor MCP (stdio); `tools` lista a superfície exposta |

Todos os comandos resolvem `.ecp/graph.bin` a partir do CWD, a menos que `--graph <path>` seja fornecido. Cada comando voltado para agente é não interativo; cada fluxo de saída é analisável.

### Sincronização de peers multi-sessão

Quando múltiplas sessões de LLM editam o mesmo repositório em paralelo, o `ecp peers` expõe o estado sujo de cada sessão no nível de símbolo e habilita mensagens diretas entre sessões. Registre-se via `ECP_SESSION_ID`, `CODEX_SESSION_ID`, `CODEX_THREAD_ID` ou `CLAUDE_CODE_SESSION_ID`.

```bash
# Iniciar o observador (um por sessão; necessário para eventos push de inbox)
ecp peers watch --start

# Quem mais está editando agora?
ecp peers status                                  # text
ecp peers status --format json                    # {session_id, pid, watcher: alive|dead|not-started}

# Inspecionar os símbolos sujos de um peer
ecp peers diff <peer-session-id> [<symbol>]

# Enviar mensagens
ecp peers say "fazendo rebase no main, aguarde pushes por 5min"    # broadcast
ecp peers say --to <peer-session-id> "pego o auth.rs?"             # direcionado

# Ler e gerenciar
ecp peers inbox
ecp peers log --limit 20
ecp peers thread <msg-id>

# Limpeza
ecp peers watch --stop && ecp peers gc
```

O campo `watcher` distingue `alive` | `dead` | `not-started` — falhas não se disfarçam de "recurso não utilizado."

### Veredictos de revisão de código comprováveis

`ecp review --verdicts` pré-computa veredictos respaldados pelo grafo a partir das seções de `ecp diff`. Passe o JSON diretamente como contexto de revisão — pule a re-derivação pelo LLM dos relacionamentos de chamador a partir de um diff bruto.

```bash
ecp review --since main --verdicts --format json
```

| Severidade | Regra |
|---|---|
| `RISK` | Existem chamadores entre arquivos, símbolo público removido ou blindspot na região do diff |
| `WARN` | Apenas chamadores dentro do arquivo, ou rota modificada |
| `INFO` | Nenhum chamador encontrado, ou nova superfície pública adicionada |

Tipos de veredicto: `SIGNATURE_OR_BODY_CHANGED` · `NEW_PUBLIC_SURFACE` · `REMOVED_PUBLIC_SURFACE` · `ROUTE_CONTRACT_CHANGED` · `BLINDSPOT_IN_DIFF_REGION`

Cada veredicto cita a seção exata do diff e o fato do grafo que o desencadeou. Especificação completa: [docs/specs/2026-05-22-review-verdicts.md](../specs/2026-05-22-review-verdicts.md).

---

## 🔌 Integração com agentes

**Prefira o caminho nativo** quando disponível — ele conecta hooks de reindexação automática e skills de fluxo de trabalho que ensinam o agente *quando* as consultas ao grafo valem o round-trip. **O MCP é o fallback universal** para qualquer host que fala o protocolo.

| Agente | Caminho | Conecta |
|---|---|---|
| Claude Code | nativo | hooks + skills + MCP opcional |
| Codex CLI | nativo | skills (ferramentas nativas pendentes) |
| Gemini CLI | nativo | skill nativa **ou** MCP |
| Cursor · Windsurf · Cline · Copilot · qualquer host MCP | MCP | servidor MCP |

Configuração guiada: `ecp admin → Agent Integrations → <host>`. Caminho scriptável para automação: `ecp admin <host> install <component>`. Inspecionar qualquer host: `ecp admin <host> status`.

### Claude Code

```bash
ecp admin claude install hooks          # settings.json: reindexação automática + enriquecimento de contexto
ecp admin claude install skills all     # pacotes de skill ecp + simplify (ou: ecp | simplify)
ecp admin claude install mcp-server     # opcional — hooks + skills + CLI já são suficientes
```

Os hooks alimentam contexto do grafo em todo Grep/Glob/Bash sem uma chamada de ferramenta explícita. A skill `ecp` ensina fluxos de trabalho de símbolo / impacto / rota / contrato / renomeação. `simplify` conduz revisão de código orientada ao grafo.

### Gemini CLI

```bash
ecp admin gemini install native-skill   # vincula via `gemini skills link`
ecp admin gemini install mcp-server     # registra via `gemini mcp add`
```

`native-skill` e `mcp-server` são mutuamente exclusivos — instalar um remove o outro.

### Codex CLI

```bash
ecp admin codex install skills all      # ecp + simplify; ferramentas nativas pendentes de conexão com Codex
```

**Skills de fluxo de trabalho:**

| Skill | Usar quando |
|---|---|
| `ecp` | O agente decide se fluxos de trabalho com reconhecimento do grafo superam grep / leituras de arquivo para símbolos, chamadores, rotas, contratos |
| `simplify` | Revisão de código começando pelo impacto do ecp, blind spots, saída, desvio de shape, deltas do resolvedor |

### Fallback MCP (Cursor, Windsurf, Cline, qualquer host MCP)

| Host | Arquivo de configuração |
|---|---|
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Cline (VS Code) | `cline_mcp_settings.json` (painel MCP → "Edit MCP Settings") |
| Host MCP genérico | específico do host |

```json
{
  "mcpServers": {
    "ecp": { "command": "ecp", "args": ["admin", "mcp", "serve"] }
  }
}
```

```bash
ecp admin mcp tools    # verificar superfície exposta antes de conectar
ecp admin mcp serve    # sem estado, disparo único por chamada (sem custo de aquecimento)
```

---

## 🏗️ Arquitetura

```
crates/
├── ecp-core        # Grafo de cópia zero (rkyv + mmap), cache incremental, consultas ao grafo
├── ecp-analyzer    # Parsers tree-sitter, detector de rotas HTTP, confiança de framework
├── ecp-mcp         # Servidor MCP (stdio) — expõe comandos principais como ferramentas
└── ecp-cli         # Binário `ecp`, motor BM25 Tantivy, saída otimizada em tokens
```

Parse → resolve → serialize passa por um canal MPSC para uma única thread construtora que monta o grafo e grava um `.ecp/graph.bin` de cópia zero. Os caminhos de leitura (`inspect`, `cypher`, `impact`, …) fazem mmap diretamente neste arquivo — sem etapa de desserialização. O cache de conteúdo xxh3_64 mantém reconstruções incrementais em menos de um segundo em um repositório com 22 mil arquivos.

---

## 🌐 Cobertura de linguagens

31 linguagens analisadas no nível estrutural. **14 de profundidade completa** (TypeScript, JavaScript, Python, Java, Kotlin, C#, Go, Rust, PHP, Ruby, Swift, C, C++, Dart) — imports, bindings nomeados, exports, herança, tipos, construtores, config, frameworks, pontos de entrada, chamadas e renomeação. **17 apenas estruturais**: Bash, Crystal, Cairo, Dockerfile, Docker Compose, GitHub Actions, HCL, Lua, Markdown, Move, Nim, Solidity, SQL, Verilog, Vyper, YAML, Zig.

📊 **[Matriz Completa de Capacidades por Linguagem](../language-matrix.md)** — status por linguagem e justificativa.

---

## ⚙️ Ajuste fino

| Variável de ambiente | Padrão | Efeito |
|---|---|---|
| `ECP_MAX_FILE_BYTES` | `16777216` (16 MiB) | Ignora arquivos-fonte acima deste tamanho durante a ingestão. Limita o uso máximo de RAM do worker em `num_threads × MAX`. |
| `ECP_CSPROJ_MAX_DEPTH` | `4` | Profundidade de recursão para descoberta de `*.csproj`. Aumente para monorepos .NET com estrutura de diretórios profunda. |

---

## 📜 Licença e agradecimentos

[PolyForm Noncommercial 1.0.0](../../LICENSE.md). Uso pessoal, pesquisa, projetos de hobby e organizações sem fins lucrativos são explicitamente permitidos. **O uso comercial não é concedido por esta licença** — entre em contato com o autor do GitNexus original, Abhigyan Patwari, para direitos comerciais.

Construído sobre:
- [GitNexus](https://github.com/abhigyanpatwari/GitNexus) — design original, superfície da CLI e modelo conceitual
- [tree-sitter](https://tree-sitter.github.io/) — parsing de AST incremental e robusto
- [rkyv](https://rkyv.org/) — framework de desserialização de cópia zero
- [Tantivy](https://github.com/quickwit-oss/tantivy) — motor de busca de texto completo
- [Rayon](https://github.com/rayon-rs/rayon) — paralelismo de dados para parsing de AST concorrente multi-core
- [xxhash (xxh3_64)](https://xxhash.com/) — hashing não criptográfico para indexação incremental baseada em conteúdo
- [DashMap](https://github.com/xacrimon/dashmap) — mapas hash concorrentes para montagem do grafo
- [memmap2](https://github.com/RazrFalcon/memmap2-rs) — mapeamento de memória de cópia zero para acesso ao grafo em submilissegundos
- [msgspec](https://github.com/jcrist/msgspec) — serialização JSON de alto desempenho para comunicação entre processos

Integração de agentes (bootstrap de URL, skill do Claude Code, instalação de plugin): `docs/skills/ecp-onboard/`. Invariantes de concorrência e re-verificação: `../../scripts/audit/audit-concurrency.sh`.

## 🚦 Status de lançamento

Caminho de instalação verificado: `cargo install --git ...`, que compila o `ecp` a partir do código-fonte. Os instaladores de release já contêm o fluxo de verificação de checksum e proveniência, mas requerem uma tag publicada e assets de release antes que o caminho de download do binário seja verificado de ponta a ponta. Skill de integração voltada para agentes: [docs/skills/ecp-onboard/ONBOARDING.md](../skills/ecp-onboard/ONBOARDING.md). O fluxo de configuração/setup assistido ainda está sendo refinado.

---

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=coseto6125/egent-code-plexus&type=Date)](https://star-history.com/#coseto6125/egent-code-plexus&Date)

</div>
