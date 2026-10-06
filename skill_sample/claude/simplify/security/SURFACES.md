# Security surfaces — the router

Layer 2 of the security review. Read this file when a review reaches the Security section, not before.

| Layer | Where | Cost | Read when |
|-------|-------|------|-----------|
| 1 — rungs | [`../CHECKLIST.md`](../CHECKLIST.md) § Security | 13 short rungs | every diff that touches a route, auth, tenancy, a permission, a credential, a webhook, a model-callable tool, or a caller-supplied URL |
| 2 — router | this file | one table | once the Security section opens |
| 3 — depth | the files beside this one | one file per present surface | only for a surface the repo HAS and the diff TOUCHES |

Layer 1 says **what to look for**. Layer 3 says **how to prove it**, and which look-alikes to leave alone.

## How to use it

1. **Once per session** — run every **Repo probe** in one batched call. A probe with no hits deletes that surface for this repo: record it absent and never open its file again this session.
2. **Per review** — for each surface still standing, check its **Diff trigger** against the diff. An untripped trigger means the file stays shut.
3. Read only the depth files whose trigger fired. Verify the diff against that file's checks.
4. Before reporting anything, check it against that file's **Known-safe** table. A shape listed there is a false positive.
5. In the report, name what you skipped and which filter skipped it: absent from the repo, or untouched by the diff.

## Reach for ecp first

Structural questions (which routes exist, what reaches this sink, who calls this guard) go to `ecp`. Text questions (a header name, an env key, a config literal) stay with grep.

| Security question | Command |
|---|---|
| What is the whole route surface? | `ecp routes --format toon` |
| Who handles this route, and what chain reaches it? | `ecp routes /api/o/<org>/knowledge/web` — prints handlers plus the `EntryPoint` chain |
| Where does the code talk to the outside? | `ecp tool-map --category http` (also `db`, `redis`, `queue`) — the sink inventory, per call site |
| Does an entry point reach this sink? | `ecp impact --target <sink> --direction up --depth 6` |
| …for a whole list of sinks at once | `printf '%s\n' <sinks> \| ecp impact --batch --direction up` |
| Which callers of this handler build a dangerous statement? | `ecp pattern -p 'f"SELECT $$$A"' --callers-of Owner.method --lang py` |
| Does this statement shape exist anywhere? | `ecp pattern -p 'requests.get($URL)' --lang py` |
| What did this diff move? | `ecp review --since origin/main` — runs impact, egress, shape-check and resolver-diff in one shot |

Five traps, each of which cost a real query here:

- **`--callers-of` needs the qualified name.** `start_web` is rejected; `KnowledgePipeline.start_web` resolves. `ecp find <name> --mode fuzzy` gives you the owner.
- **`ecp impact` caller counts are a lower bound.** Grep a guard function whose blast radius looks too small.
- **`found:false` with a `result` field is a stale graph.** Reindex before you report "no such route".
- **`ecp pattern` reads expressions and statements, not JSX attributes.** `highlight($A, $B)` matches inside `.tsx`; `dangerouslySetInnerHTML={{ __html: $X }}` returns `total: 0` in a file that plainly has four. Any `total: 0` you could have found with grep is a pattern-language limit, so confirm every empty pattern result with one grep before you record a surface absent.
- **A loose probe matches the mitigation and calls it the surface.** Grepping `pickle` returns every `np.save(..., allow_pickle=False)` — the control, counted as the vulnerability. Probe for the dangerous call, not for the topic's vocabulary.

## The surfaces

Two filters, and they do different amounts of work.

The **repo probe** answers "does this codebase have the surface at all". On a mature product it mostly returns present, so read a present result as "not eliminated", never as "worth reading".

The **diff trigger** says what has to appear in the diff before the depth file is worth opening. It is the filter that does the work on every review.

| Surface | Repo probe | Diff trigger — open the file when the diff… | Depth file | Rung |
|---|---|---|---|---|
| Routes and authorization | `ecp routes --format toon` | adds or moves a route, changes a guard or decorator, or changes how a handler picks its tenant | [`routes-and-authz.md`](routes-and-authz.md) | 1–4, 13 |
| Identity, session, linking | `ecp impact --target issue_session --direction up` (or `ecp find "session cookie oauth" --mode bm25`) | touches login, OAuth, a cookie attribute, or code that decides two identities are one person | [`identity-and-session.md`](identity-and-session.md) | 5, 6 |
| Inbound webhooks | `ecp routes --method POST \| grep -i "webhook\|callback"` | adds a platform adapter, changes signature handling, or changes dedup | [`webhooks.md`](webhooks.md) | 7 |
| LLM agents and tools | `ecp find "tool executor schema" --mode bm25` | adds or changes a model-callable tool, its schema, or what enters the prompt | [`llm-agents.md`](llm-agents.md) | 8 |
| Query and command construction | `ecp tool-map --category db` | builds a query, an argv, a path, or a template from a value it did not write | [`injection.md`](injection.md) | 9 |
| Outbound fetch | `ecp tool-map --category http` | adds a fetch, or changes where a fetched URL comes from | [`outbound-fetch.md`](outbound-fetch.md) | 10 |
| Secrets and cryptography | `ecp find "sign verify derive key" --mode bm25` | signs, verifies, derives, generates a token, or reads a new secret from config | [`secrets-and-crypto.md`](secrets-and-crypto.md) | 11 |
| Cost and limits | `ecp routes` non-empty **and** `ecp tool-map --category http,queue` non-empty | adds a route that spends, or a quantity the caller sizes | [`cost-and-limits.md`](cost-and-limits.md) | 12 |
| Rendering untrusted content | `grep -rn "dangerouslySetInnerHTML\|innerHTML\|v-html\|mark_safe" <src>` — grep, not `ecp pattern`: see the traps below | puts a value into markup, an `href`, or a redirect | [`untrusted-render.md`](untrusted-render.md) | — |
| Files and uploads | `grep -rn "zipfile\|tarfile\|request.files\|UploadFile" --include=*.py .` | opens a path built from input, accepts an upload, or expands an archive | [`files-and-uploads.md`](files-and-uploads.md) | — |
| Deserialization | `ecp pattern -p 'pickle.loads($X)' --lang py` or `grep -rn "yaml.load(" .` | turns outside bytes into objects, or widens what a decoder accepts | [`deserialization.md`](deserialization.md) | — |
| Cross-origin surface | `grep -rn "Access-Control-Allow\|postMessage" <src>` | changes a CORS header, a cookie attribute, an embed, or a frame message | [`cross-origin.md`](cross-origin.md) | — |
| Errors and logs | none — present in every repo | adds an error path, widens an `except`, or logs near a credential or caller data | [`errors-and-logs.md`](errors-and-logs.md) | — |

A probe that returns present for any repo of its language is a bad probe, not a present surface: counting `API_KEY`, `max_tokens` or `logger.` says nothing. If a probe here never eliminates anything across several repos, say so and narrow it.

## The one inherited rule this skill overrides

Public security-review skills tell the reviewer to skip a path that needs prior authentication, and to note the auth requirement instead. That rule suits a single-tenant service, where an authenticated caller reaches only their own data.

**In a multi-tenant product the authenticated caller sits inside the threat model.** An org owner, a member and a paying customer each hold a session, and each is the attacker against the tenant next door. An authenticated path stays in scope, and a finding states the role it needs as a precondition, never as a dismissal.

## Provenance

The vulnerability classes and several *Known-safe* tables are written from the OWASP Cheat Sheet Series (CC BY-SA 4.0), read alongside two public skills: `getsentry/skills@security-review`, whose attacker-controlled/server-controlled split shaped the false-positive discipline, and `UnitOneAI/SecuritySkills` (MIT), whose agentic and LLM material shaped [`llm-agents.md`](llm-agents.md). The text here is written for this machine, not copied; both repos stay the place to check for material these files do not carry.
