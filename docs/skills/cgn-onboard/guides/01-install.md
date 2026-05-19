# Phase 01 — Install

Goal: produce a verified `cgn` binary on PATH. Start the install in the
background and advance to Phase 02 without waiting.

## Step 1: Probe the system (single call)

Run the **bundled probe script** in `_shared/refs/env-detect.md` —
paste the whole `bash <<'PROBE' … PROBE` block into ONE Bash tool
call. It runs all probes concurrently and emits one JSON object in
~100ms (vs ~10s if you call `command -v` one tool at a time).

Stash the result:

```
config_inventory.system_probe = <parsed JSON>
```

All downstream phases (02 / 03 / 04 / 05) re-use `config_inventory.system_probe`.
**Do not re-run `command -v` / `test -d` individually anywhere in the wizard.**
If the user installs something mid-wizard, re-run the whole probe to
refresh the snapshot.

## Step 2: Apply persona × probe → recommendation

Read fields off `config_inventory.system_probe`:

| persona.install_pref | probe fields | Recommendation |
|---|---|---|
| `cargo-binstall` | `installers.cargo_binstall = true` | `cargo binstall code-graph-nexus` |
| `cargo-binstall` | `installers.cargo_binstall = false`, `installers.cargo = true` | `cargo install code-graph-nexus` (slower; source build) + suggest installing cargo-binstall next time |
| `brew` | `installers.brew = true` | `brew install <tap>/code-graph-nexus` (substitute the actual tap name from the README) |
| `github-release-tarball` (or fallback) | `installers.curl = true` | `curl -L https://github.com/<owner>/code-graph-nexus/releases/latest/download/cgn-<target>.tar.gz \| tar -xz -C ~/bin/` |
| (cgn already installed) | `cgn.installed = true` | Verification only; skip download. Use `cgn.version` to display "Detected cgn 0.2.0". |

## Step 3: Present 3-choice menu

Format (translate to `lang_pref`):

```
[Phase: install / Step 1 of 5]

Based on your persona ({install_pref}, {os}-{arch}), recommendation:

  ✓ Recommended: {recommended_command}
     Why: {reason}

  Alternative A: {alt_a_command}
     Why: {reason_a}

  Alternative B: {alt_b_command}
     Why: {reason_b}

  Skip: I've already installed it (I'll jump to verification)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Start background install

If choice ≠ skip:

- Spawn the chosen command in the background (use the agent's
  `run_in_background` shell execution mode).
- Do NOT wait for completion. Record the background task ID into
  `config_inventory.install_task_id`.
- Immediately tell the user: "Install running in background. Continuing
  to Phase 02 — your binary will be verified before any `cgn` commands
  are executed."

If choice == skip:

- Run `cgn --version` synchronously and record the output. If it fails,
  loop back to Step 3.

## Step 5: Advance to Phase 02 (do NOT block on install)

Jump to `guides/02-first-index.md`. The Phase 01 background install
keeps running while later phases collect their choices.

## Failure handling

If the install command fails (whether discovered at T6 verification or
earlier), do not auto-retry. Consult the **install** rows in the
common-cause table in `_shared/refs/env-detect.md` and offer the user:

- **Retry** the same command (verbatim)
- **Change method** — re-present the 3-choice menu, excluding the failed option
- **Skip** — mark `config_inventory.install_status = failed` and let
  Phase 05 surface the failure in the final summary

Never silently switch methods.
