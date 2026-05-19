# Recommendation Templates

Phase 05 (summary) emits a "next steps" list tailored to the user's
persona. This file is the source library. The agent picks 3–5 lines
matching the persona, never invents new ones outside this list.

## How to read this file

Each section is keyed by persona dimension + value. Within a section,
each `- ` bullet is one recommendation candidate. Use `{<placeholder>}`
for inputs the agent fills in (e.g., `{repo_name}` = the first repo the
user indexed).

## By scope_pref

### scope_pref = group-heavy

- Run `cgn group find <group> "<symbol>" --merge rrf` to do cross-repo BM25 search with RRF fusion.
- Run `cgn group contracts <group>` to inventory routes / queue / RPC contracts across the group.
- Run `cgn group impact <group> --baseline origin/main` to see the full blast radius of a multi-repo change before merging.

### scope_pref = single-repo

- Run `cgn find "<symbol>" --repo .` to look up the canonical definition.
- Run `cgn impact <symbol> --direction upstream --repo .` to see callers.
- Run `cgn routes --repo .` to list HTTP routes mapped to handlers.

## By ide_pref

### ide_pref = claude-code

- Open a Claude Code session in `{repo_name}` and ask "summarize the auth module"; the cgn MCP tools should appear automatically.
- Type `/cgn` in Claude Code to see the cheatsheet skill loaded.

### ide_pref = cursor

- Restart Cursor after the MCP config was written so it picks up the new server.
- Cursor's MCP servers appear in Settings → Features → MCP.

### ide_pref = zed

- Zed's assistant panel will list `cgn_*` tools once the config is reloaded.

### ide_pref = vscode / continue

- Continue.dev reads `~/.continue/config.json`. Restart VS Code to pick up the new MCP server.

## By install_pref (post-install hygiene)

### install_pref = cargo-binstall

- `cargo binstall --self-update` keeps cargo-binstall current so future cgn upgrades stay fast.
- Run `cgn --version` periodically; cargo-binstall does NOT auto-upgrade cgn itself.

### install_pref = brew

- `brew upgrade code-graph-nexus` will pull the latest tagged release.

### install_pref = github-release-tarball

- Bookmark `gh release view --repo <owner>/code-graph-nexus` to spot new releases.

## Universal (always offer 1)

- Bookmark this summary file (`~/.cgn/onboarding-summary.md`) — a future agent session can read it to know what was set up.
- Run `cgn coverage --repo @all --detailed` to inspect registry health.
- Run `cgn admin mcp tools` to list the MCP tools currently exposed.

## When persona is fully `unknown`

Pick 2 from the **Universal** list + the 3 lines under `scope_pref = single-repo`.
