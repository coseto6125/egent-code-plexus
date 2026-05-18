# gnx-onboard SKILL — Design

> **Status:** 2026-05-18 — design accepted by user via brainstorming session.
> Implementation plan to be drafted next.

## Purpose

Ship a **personalized installation + configuration wizard** for `graph-nexus`
(distributed as a layered SKILL pack on GitHub). When a user opens the share
link (or pastes the bootstrap URL) into their AI agent, the agent walks them
from "no gnx installed" → "gnx ready + indexed + grouped + MCP wired +
recommended next steps", with every step using:

- **Already-loaded prompts** (CLAUDE.md / system prompt / chat history) as the
  primary personalization signal — no fishing in additional user files.
- **System probes** (`uname`, `command -v <pkg-mgr>`, etc.) to bound which
  install options are viable on this host.
- **Sequential menus** with the standard pattern: **accept recommended /
  change / skip**.

The wizard never invents its own config format — every applied action goes
through the `gnx` CLI.

## Constraints inherited from upstream discussion

- **Cross-agent**: must work for any LLM agent that can fetch URLs (Claude
  Code, Cursor, Gemini CLI, Codex CLI, Aider, Continue.dev, Windsurf, etc.).
- **Layered structure** (Layer 1 SKILL.md / Layer 2 guides / Layer 3 _shared).
- **No fishing for user files** beyond what is already in the agent's context.
- **CLI reference dynamically generated per gnx version**, not hand-written.
- **Batch-apply flow**: collect choices first, run background install, then
  apply all `gnx` actions once binary is verified.

## § 1 Architecture

The SKILL pack lives **inside the `gitnexus-rs` repo itself**, not in a
separate `gnx-onboard` repo. This keeps the SKILL and the `gnx` CLI on the
same release cycle (zero drift) and halves maintenance burden. The cost —
distribution outlets (c) and (d) pulling extra repo weight — is mitigated
with `--depth=1` + sparse-checkout instructions in the README.

```
gitnexus-rs/                                      ← existing repo; new content added in two locations
│
├── docs/skills/
│   ├── gnx.md                                     ← existing — agent cheatsheet for already-installed gnx
│   └── gnx-onboard/                               ← NEW — onboarding SKILL pack
│       │
│       ├── ONBOARDING.md                          ← CI build artifact (SKILL.md + guides aggregated)
│       ├── SKILL.md                               ← Layer 1 (only file with frontmatter, ~80 lines)
│       │
│       ├── guides/                                ← Layer 2 (phase guides, pure markdown, no frontmatter)
│       │   ├── 01-install.md
│       │   ├── 02-first-index.md
│       │   ├── 03-group.md
│       │   ├── 04-mcp.md
│       │   └── 05-summary.md
│       │
│       └── _shared/                               ← Layer 3 (on-demand reference cards)
│           ├── cli/
│           │   ├── manifest.json                  ← {"latest": "0.1.5", "versions": [...]}
│           │   ├── 0.1.5/
│           │   │   ├── find.md
│           │   │   ├── impact.md
│           │   │   ├── admin-index.md
│           │   │   ├── admin-group.md
│           │   │   ├── admin-mcp.md
│           │   │   └── group-find.md
│           │   └── 0.1.4/                         ← old versions retained
│           │       └── ...
│           └── refs/
│               ├── env-detect.md                   ← probe snippets + "common failures" table
│               ├── persona-inference.md            ← signal → persona-dimension rule table
│               └── recommendation-templates.md     ← "next-step" sentence library
│
├── tools/                                         ← NEW — bash tooling (no language runtime needed)
│   ├── aggregate.sh                               ← concatenates SKILL.md + guides → ONBOARDING.md
│   ├── gen-cli-ref.sh                             ← scrapes `gnx <cmd> --help` → _shared/cli/<ver>/
│   ├── lint-skill.sh                              ← T1 structural lint (see § 5)
│   └── test-persona-rules.sh                      ← T4 persona-rule self-consistency (see § 5)
│
├── tests/skill/                                   ← NEW — SKILL-specific fixtures
│   ├── persona-fixtures.yaml                      ← T4 fixture set
│   └── smoke-playbook.md                          ← T5 manual end-to-end checklist
│
└── .github/workflows/
    ├── skill-aggregate.yml                        ← NEW — regenerates ONBOARDING.md on push (touching docs/skills/gnx-onboard/)
    └── skill-cli-ref.yml                          ← NEW — regenerates _shared/cli/<ver>/ on graph-nexus release tag
```

### 4 distribution outlets, same source

| Outlet | Audience | Mechanism |
|---|---|---|
| (a) URL bootstrap | Any LLM agent | User pastes `Fetch <raw URL>/docs/skills/gnx-onboard/SKILL.md and follow it` into chat |
| (b) ShareOnboardingGuide | Claude Code, lowest friction | Run from `docs/skills/gnx-onboard/` cwd → short-code link → recipient opens in Claude Code |
| (c) Plugin (`/plugin install`) | Claude Code power user | Pulls full repo into `~/.claude/plugins/gitnexus-rs/`; SKILL is a subpath. **Recommended:** README documents `--depth=1` + sparse-checkout to pull only `docs/skills/gnx-onboard/` |
| (d) `git clone` | Offline / hacker | Sparse-checkout `docs/skills/gnx-onboard/` to corresponding agent's skill / rule directory |

All 4 outlets derive from the same `docs/skills/gnx-onboard/` source. The
aggregator (`tools/aggregate.sh`) generates `ONBOARDING.md` for (b);
`_shared/` is **not** aggregated (size). (a) / (c) / (d) consume the
source directly.

### Why same-repo (decision log entry)

- **Zero version drift**: gnx CLI and the SKILL ship in lockstep; § 4 D
  divergence path becomes a near-empty branch.
- **One PR, one CI**: aggregator + CLI-ref regen pipelines reuse existing
  repo's GitHub Actions setup.
- **`gen-cli-ref.sh` is trivial**: builds `gnx` locally with `cargo build`
  and points the generator at the freshly-built binary — no need to
  download a release or run `cargo install`.
- Trade-off: (c)/(d) outlets pull ~hundreds of MB of Rust source. Mitigated
  via `git clone --depth=1 --filter=blob:none --sparse <url> && cd ... &&
  git sparse-checkout set docs/skills/gnx-onboard` — documented in README.

## § 2 Components & responsibilities

### Layer 1 — `SKILL.md` (~80 lines, only frontmatter)

```yaml
---
name: gnx-onboard
description: Personalized installation + configuration wizard for graph-nexus.
  Walks the user from "no gnx installed" → "gnx ready + indexed + MCP wired
  + recommended next steps".
when-to-use: User says "install gnx" / "set up graph-nexus" / "onboard me
  to gnx", OR opened an ONBOARDING share link / pasted a bootstrap URL.
---
```

Body contains exactly four things:

1. **Core directives** (3–4 items): every step recommends + user picks
   accept/change/skip; use only already-loaded prompts + system probes,
   never fish for additional user files; every apply action calls the
   `gnx` CLI; **never block on background install — collect later phases'
   choices in parallel**.
2. **Persona inference summary** (pointer to `_shared/refs/persona-inference.md`).
3. **Jump table**: intent → which guide to load next.
4. **Strict ordering rules**: phase ordering, parallelism boundaries,
   when each phase must verify the previous.

SKILL.md is read on every interaction; **it must stay short and only
decide "where to go next" — not how to do anything**.

### Layer 2 — `guides/0X-*.md` (~100–150 lines each)

Each guide owns one phase. Boundaries:

| Guide | Responsibility | Output |
|---|---|---|
| `01-install.md` | Detect OS/arch/pkg manager; recommend best install path; **trigger background download**; verify `gnx --version` at T6 | Installed binary |
| `02-first-index.md` | Ask which repo(s) to index; record choice (no `gnx` call yet) | Choice queued |
| `03-group.md` | Detect monorepo / sibling-repo patterns; recommend group layout; record choice | Choice queued |
| `04-mcp.md` | Detect installed IDEs (Claude Code / Cursor / Zed); record which IDE configs to write | Choice queued |
| `05-summary.md` | Drain queued choices in batch at T7; persist `~/.gnx/onboarding-summary.md`; emit recommendation list | Final state |

**Every guide is self-contained for that phase.** It does not assume the
reader has seen other guides. The LLM reads guide N only when entering
phase N.

### Layer 3 — `_shared/` (on-demand)

- **`cli/<version>/<cmd>.md`** — ~20–40 lines per command card. Auto-generated
  by `tools/gen-cli-ref.sh` from `gnx <cmd> --help`. Pull when needing exact
  flag syntax.
- **`refs/env-detect.md`** — shared OS/pkg-manager probe snippets used by
  install + mcp guides. Avoids re-inventing the probes per guide.
- **`refs/persona-inference.md`** — signal-to-persona rule table (machine-
  readable markdown), tested by `tools/test-persona-rules.sh`.
- **`refs/recommendation-templates.md`** — phase 5's "next steps" sentence
  library, indexed by persona dimension.

### Repo root — `ONBOARDING.md` + CI

- `ONBOARDING.md` is a **build artifact**. CI runs `tools/aggregate.sh` on
  every push to `main` and commits the result back.
- Aggregator concatenates `SKILL.md` + `guides/*.md` (in order) with section
  dividers; **does not include `_shared/`** (size).
- ShareOnboardingGuide is invoked from the repo cwd, picking up the
  freshly-built `ONBOARDING.md` → returns a short-code link.

## § 3 Data flow

### Phase transition

```
T0  User opens share link / pastes URL bootstrap
        │
        ▼
T1  Agent loads SKILL.md (Layer 1)
        ├─ reads directives + jump table
        └─ extracts persona signals from already-loaded prompts
                ↓ (rules from _shared/refs/persona-inference.md)
            persona = { lang_pref, install_pref, scope_pref, ide_pref, ... }
        │
        ▼
T2  Agent loads guides/01-install.md
        ├─ runs system probes (snippets from _shared/refs/env-detect.md)
        ├─ crosses persona × system → 3-choice recommendation
        ├─ user picks accept/change-A/change-B/skip
        ├─ if not skip → start background download (run_in_background)
        │     (cargo binstall / brew / curl + tarball)
        └─ DOES NOT BLOCK — immediately advance to T3
        │
        ▼
T3  Phase 02 first-index: agent asks which repo(s) to index;
        records choice into in-memory config_inventory.
        Background download still running.
        │
        ▼
T4  Phase 03 group: agent infers monorepo pattern; recommends group
        layout; records choice. Background download still running.
        │
        ▼
T5  Phase 04 mcp: agent detects installed IDEs; confirms which configs
        to write; records choices. Background download still running.
        │
        ▼
T6  ─── Batch-apply gate ─────────────────────────────────
        Wait for background download to complete + verify gnx --version.
        On failure → § 4 path B (hypotheses + retry / change / skip).
        On success → proceed to T7.
        │
        ▼
T7  Batch-apply queued choices in order:
        gnx admin index --repo <choices from T3>
        gnx admin group add --repo <...> <group-name>  (per T4)
        write IDE MCP configs (per T5)
        verify each command succeeded before moving to next
        │
        ▼
T8  Phase 05 summary:
        write ~/.gnx/onboarding-summary.md (YAML frontmatter + checklist)
        emit "next steps" recommendation list to chat
```

### Persona inference

**Inputs** (strict allow-list):

- System prompt (includes `~/.claude/CLAUDE.md` for Claude Code recipients).
- Conversation history.
- Any memory / injected context already in the LLM's window.

**Rule table** (excerpted from `_shared/refs/persona-inference.md`):

| Signal | Persona dimension | Default |
|---|---|---|
| CLAUDE.md mentions `繁體中文` / `Traditional Chinese` | `lang_pref = zh-TW` | Wizard speaks 繁中 |
| Chat mentions `cargo` / `rust` / `cargo workspace` | `install_pref = cargo-binstall` | Recommend `cargo binstall graph-nexus` |
| Chat mentions `monorepo` / `multi-repo` / `workspace` | `scope_pref = group-heavy` | Don't skip group phase; recommend full group setup |
| Prior context shows Cursor / Zed / VS Code usage | `ide_pref = <name>` | Write MCP config for that IDE |
| No specific signal | `*_pref = unknown` | Conservative default (e.g. GitHub release tarball for install) |

Persona is **re-evaluated at the start of each phase** — the user may
reveal new signals during interaction.

### Standard 3-choice menu format

Every recommendation point uses this template verbatim (avoids drift):

```
[Phase: install / Step 2 of 5]

Based on your persona (Rust power user, macOS arm64), recommendation:

  ✓ Recommended: cargo binstall graph-nexus
     Why: cargo-binstall detected on your system; no compile, ~30s

  Alternative A: brew install <tap>/graph-nexus
     Why: you favor system package managers; Homebrew detected

  Alternative B: download GitHub release tarball directly
     Why: fallback when neither is available

  Skip: I've already installed it (I'll jump to verification)

Reply: accept / a / b / skip  (or describe in words)
```

The agent maps `accept` / letter / `skip` to the corresponding branch.
Free-text input is interpreted before deciding which branch to take.

### Persistence boundary

| Data | Where | Why |
|---|---|---|
| Persona table | Agent in-memory (this session only) | No write to user disk; respects "no fishing" rule |
| `config_inventory` (per-phase choices) | Agent in-memory → written at T8 | Avoids leaving half-finished artifacts |
| `~/.gnx/onboarding-summary.md` | Recipient's local disk | Future sessions can grep for "last install state" |
| `~/bin/gnx`, `~/.gnx/graph-nexus*/`, `registry.json` | Recipient's local disk | Owned by gnx CLI itself; wizard only triggers it |

## § 4 Error handling & exception paths

### 4 error classes × standard response

| Class | Trigger | Required agent action |
|---|---|---|
| **A. System probe fails** | `uname` / `command -v` unavailable, weird OS | Switch to "ask mode": directly ask user for OS + package manager; mark `system_probe = manual` and stop silent probing |
| **B. Install / command fails** | `cargo binstall ... → exit 1`, `gnx admin index ... → exit 1`, network timeout | (1) Paste stderr verbatim. (2) Map against "common-cause table" → offer 1–3 hypotheses. (3) Present **retry / change-method / skip**. Never auto-retry, never silently switch methods. |
| **C. User pivots mid-flow** | User says "actually let me redo phase M" while in phase N | Return to phase M's entry, preserve later phases' inventory; if a phase M change invalidates later phases (e.g., install path changes break previously-recorded binary path), explicitly list "the following phases will re-run: …" and wait for confirmation |
| **D. gnx CLI behavior diverges** | `gnx` output format changed, flag missing, version mismatch | Do not hardcode flags. First call `gnx <cmd> --help` for ground truth. If divergent → set `skill_drift_detected = true`, print diff, suggest user file a report |

### Common-cause table (used by class B)

Lives at the tail of `_shared/refs/env-detect.md`; referenced by every guide:

| Phase | Symptom | Hypotheses (priority order) |
|---|---|---|
| install | `cargo binstall` not found | (1) cargo not installed; (2) cargo-binstall subcommand missing (suggest `cargo install cargo-binstall`) |
| install | binstall fails to fetch tarball | (1) no prebuilt for that target triple → fallback to source build; (2) network / proxy; (3) GitHub release not yet propagated |
| first-index | `gnx admin index ... → not a git repo` | Wrong path / no `.git` |
| first-index | index takes >3min | Large repo / vendored / should configure `.gnxignore` |
| group | `gnx admin group add ... → repo not in registry` | Repo not yet indexed |
| mcp | IDE config written but IDE doesn't pick it up | IDE not restarted / wrong config path (Cursor has two locations) |

### Resume after interruption

The user may close the terminal and start a new agent session later. The
SKILL must handle this gracefully:

1. **New-session preamble** — directive 1 in SKILL.md: "If
   `~/.gnx/onboarding-summary.md` exists, read it first."
2. **summary file format** is machine-readable:

   ```yaml
   ---
   wizard_version: 0.1.0
   last_phase_completed: 02-first-index
   persona_snapshot: { lang_pref: zh-TW, install_pref: cargo-binstall, ... }
   ---
   ## Phase 01 install
   - [x] cargo binstall graph-nexus
   - [x] verified gnx --version → 0.1.5
   ## Phase 02 first-index
   - [x] indexed: ~/gitnexus-rs
   ...
   ```

3. Agent asks: "Resume from phase 03? Redo a specific phase? Start over?"
   — three-choice menu.

### Hard "don't" list (in SKILL.md directives)

- ❌ Don't silently retry a failed command.
- ❌ Don't switch install methods without user consent.
- ❌ Don't touch any user file other than `~/.gnx/onboarding-summary.md`
  (do not modify `~/.zshrc`, `~/.gitconfig`, `~/.cursor/...` unless phase 4
  mcp explicitly with user consent).
- ❌ Don't assume "future gnx versions should have flag X" — check live `--help`.

## § 5 Testing strategy

Drift sources (5 layers of test):

### T1 — Structural lint (every commit, cheapest)

`tools/lint-skill.sh`:

| Check | Fail condition |
|---|---|
| SKILL.md has valid frontmatter | Missing `name` / `description` / `when-to-use` |
| `guides/*.md` have no frontmatter | Any guide has frontmatter |
| All jump-table links resolve | Any link 404 |
| All guides appear in jump table | Any orphan guide |
| All `_shared/cli/<ver>/<cmd>.md` have manifest entries | Manifest gap |
| All `gnx <subcmd>` references in Layer 1+2 exist in latest CLI manifest | Stale reference |

Implementation: bash + grep + jq; runs in seconds.

### T2 — Aggregator round-trip (every commit)

```bash
./tools/aggregate.sh > /tmp/ONBOARDING.gen.md
diff -u ONBOARDING.md /tmp/ONBOARDING.gen.md   # must exit 0
```

Prevents hand-edits of `ONBOARDING.md` (it's a build artifact only).

### T3 — CLI ref drift (after gnx version bump)

```bash
gnx --version
./tools/gen-cli-ref.sh > /tmp/cli-ref-new/
diff -r skills/gnx-onboard/_shared/cli/<ver>/ /tmp/cli-ref-new/
```

If different → emit new `_shared/cli/<new-ver>/`, update manifest, open
PR. **Manual until CI workflow is wired up.**

### T4 — Persona-rule self-consistency

Rules are written as machine-readable markdown tables in
`_shared/refs/persona-inference.md`. Fixtures live at
`tests/persona-fixtures.yaml`:

```yaml
- signal: "CLAUDE.md contains '繁體中文'"
  expected_persona: { lang_pref: "zh-TW" }
- signal: "chat contains 'cargo' AND 'workspace'"
  expected_persona: { install_pref: "cargo-binstall", scope_pref: "group-heavy" }
- signal: "(empty)"
  expected_persona: { lang_pref: "unknown", install_pref: "github-release-tarball" }
```

`tools/test-persona-rules.sh`:
1. Parse rule table into a rule set.
2. Apply rules to each fixture's signal → derived persona.
3. Assert derived == expected.

**This tests rule-table internal consistency, not LLM behavior** —
covers: no contradictions, fixture coverage, empty-signal fallback exists.

### T5 — End-to-end smoke playbook (manual, weekly/monthly)

`tests/smoke-playbook.md`:

```
1. clean VM (no gnx, no ~/.gnx)
2. paste URL bootstrap into Claude Code
3. expect: agent reads SKILL.md → emits phase 01 3-choice menu
4. choose accept → expect background download starts + agent advances to phase 02 immediately
5. ... walk all 5 phases
6. verify: ~/.gnx/onboarding-summary.md exists and contains all 5 phases
7. verify: gnx find . in indexed repo returns results
8. verify: MCP config written to correct path
```

Runs pre-release; not per-PR (cost).

### Cross-outlet parity (pre-release)

| Outlet | Test |
|---|---|
| (a) URL bootstrap | Fetch raw SKILL.md → byte-equal to git checkout at same commit |
| (b) ShareOnboardingGuide | Upload ONBOARDING.md → fetch short-code version → byte-equal to repo ONBOARDING.md |
| (c) Plugin | Clone to `~/.claude/plugins/gnx-onboard/` → smoke playbook passes |
| (d) git clone | Clone → `tree skills/gnx-onboard/` structure equals source |

### Out of test scope (explicit)

- ❌ No cross-OS install matrix in CI (cost vs. ROI).
- ❌ No LLM-as-judge evaluation of wizard dialogue quality (noisy).
- ❌ No test that "users actually understand" the wizard wording (that's a
  writing problem, caught by smoke playbook reviewers).

## § 6 Out of scope / future work

- **Localization beyond zh-TW + en**: persona inference rule table currently
  only encodes these two; adding ja/ko/de etc. later requires new rule rows
  and recommendation-template translations.
- **Wizard for `gnx admin` advanced flags** (`--force`, `--baseline`, etc.):
  current 5-phase flow covers happy paths only.
- **Auto-update of installed gnx**: phase 01 install, not "upgrade an
  existing gnx to the latest". Future phase 06 could handle.
- **Team-mode setup**: shared group profiles across a team's machines —
  out of scope for v1.
- **Plugin marketplace publication** for outlet (c): structural work to
  package the repo as a publishable Claude Code plugin, separate from
  the SKILL content itself.

## Decision log

| Date | Decision | Rationale |
|---|---|---|
| 2026-05-18 | Canonical = GitHub repo; 4 outlets all designed | Cross-agent reach requires URL-fetchable markdown; layered structure needs real directories; ShareOnboardingGuide kept as lowest-friction Claude Code path |
| 2026-05-18 | Personalization signals = already-loaded prompts + system probes only | User wants privacy-respecting flow; no fishing for other files |
| 2026-05-18 | 3-choice menu (accept / change / skip) at every step | User preference for recommended-default with override |
| 2026-05-18 | Batch-collect-then-apply (don't block on install) | Reduce wait time; user answers later phases while binary downloads |
| 2026-05-18 | CLI reference cards auto-generated per gnx version | Avoid hand-written drift; supports older-version recipients |
| 2026-05-18 | CI is future work; manual generator runs first | Get to MVP fast; wire CI after pattern stabilizes |
| 2026-05-18 | Same-repo (live in `gitnexus-rs/docs/skills/gnx-onboard/`), not a separate `gnx-onboard` repo | Zero version drift between gnx CLI and SKILL, single CI, single PR; cost on outlets (c)/(d) mitigated by sparse-checkout instructions |
