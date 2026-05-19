# Phase 02 — First-index

Goal: collect the user's choice of which repo(s) to index. **Do not run
`cgn admin index` here** — only record the choice into
`config_inventory.first_index`.

## Step 1: Detect candidate repos

The agent should NOT scan the filesystem broadly. Instead, infer candidates
from already-loaded context:

- Current working directory (if the chat is happening inside a repo)
- Any repo path the user mentioned in chat
- The repo containing this SKILL pack itself (if recipient is reading
  the file by absolute path)

If no candidate is obvious, ask the user directly: "Which repository
should I index first?"

## Step 2: Apply persona → recommendation

| persona.scope_pref | Recommendation |
|---|---|
| `group-heavy` | Index 2–3 sibling repos in a single batch (user lists them) |
| `single-repo` | Index the current repo only |
| `unknown` | Ask the user; default to "current directory" |

## Step 3: Present 3-choice menu

```
[Phase: first-index / Step 2 of 5]

Based on your persona ({scope_pref}), recommendation:

  ✓ Recommended: index {recommended_repo_list}
     Why: {reason}

  Alternative A: index only the current directory
  Alternative B: skip indexing for now (you can run `cgn admin index` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice (DO NOT execute)

Record into `config_inventory.first_index`:

```yaml
first_index:
  repos: [<chosen list>]
  status: queued     # NOT 'done' — apply happens in Phase 05
```

## Step 5: Advance to Phase 03

Jump to `guides/03-group.md`. If `persona.scope_pref = single-repo` AND
only one repo was selected, **skip directly to** `guides/04-mcp.md`
(no group needed).
