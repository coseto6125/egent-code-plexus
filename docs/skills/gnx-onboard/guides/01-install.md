# Phase 01 — Install

Goal: produce a verified `gnx` binary on PATH. Start the install in the
background and advance to Phase 02 without waiting.

## Step 1: Probe the system

Run the probes from `_shared/refs/env-detect.md`:

```bash
uname -sm
command -v cargo
command -v cargo-binstall
command -v brew
command -v curl
```

Record results in `config_inventory.install_probe`:

- `os`, `arch` from `uname -sm`
- `has_cargo_binstall`, `has_brew`, `has_curl` booleans
- `gnx_already_installed`: `command -v gnx && gnx --version`

## Step 2: Apply persona × probe → recommendation

| persona.install_pref | probes | Recommendation |
|---|---|---|
| `cargo-binstall` | `has_cargo_binstall = true` | `cargo binstall graph-nexus` |
| `cargo-binstall` | `has_cargo_binstall = false`, `has_cargo = true` | `cargo install graph-nexus` (slower; source build) + suggest installing cargo-binstall next time |
| `brew` | `has_brew = true` | `brew install <tap>/graph-nexus` (substitute the actual tap name from the README) |
| `github-release-tarball` (or fallback) | `has_curl = true` | `curl -L https://github.com/<owner>/graph-nexus/releases/latest/download/gnx-<target>.tar.gz \| tar -xz -C ~/bin/` |
| (gnx already installed) | `gnx_already_installed = true` | Verification only; skip download |

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
  to Phase 02 — your binary will be verified before any `gnx` commands
  are executed."

If choice == skip:

- Run `gnx --version` synchronously and record the output. If it fails,
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
