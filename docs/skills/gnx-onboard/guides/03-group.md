# Phase 03 — Group

Goal: collect group definitions if the user has multiple repos. **Do not
run `gnx admin group add` here** — record into `config_inventory.groups`.

This phase is **skipped** when:

- `persona.scope_pref = single-repo` AND `first_index.repos` has length 1
- The user explicitly skipped Phase 02

## Step 1: Detect grouping signals

- Were multiple repos selected in Phase 02?
- Do their paths share a common parent (suggests a monorepo / workspace)?
- Did the chat mention "team", "monorepo", "service mesh", or similar?

If none of these → ask the user: "Do you have related repos you'd like
to query as a unit (e.g., a frontend + backend pair, or a microservices
suite)?"

## Step 2: Apply persona → group layout recommendation

| Pattern | Recommendation |
|---|---|
| 2–3 repos sharing parent dir | One group named after the parent dir |
| Frontend + backend mentioned | Two groups (`frontend`, `backend`), each with the relevant repo |
| User-named group | Take the user's name verbatim |

## Step 3: Present 3-choice menu

```
[Phase: group / Step 3 of 5]

Detected grouping signals: {summary}.

  ✓ Recommended: create group "{recommended_name}" with repos {repo_list}
     Why: {reason}

  Alternative A: separate groups per pair (e.g., A, B)
  Alternative B: no groups (you can `gnx admin group add` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice

```yaml
groups:
  - name: {chosen_name}
    repos: [{chosen_repos}]
    status: queued
```

## Step 5: Advance to Phase 04

Jump to `guides/04-mcp.md`.
