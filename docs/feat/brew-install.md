# Feature: `brew install ecp` (Homebrew tap)

**Status:** Deferred — depends on the project going public.
**Owner:** Unassigned.
**Trigger to pick up:** First public Release, or when user demand for macOS-native install path materializes.

## Why deferred

The current development philosophy is _internal-only, no external publish_. A Homebrew tap requires:

- A separate **public** GitHub repo (`coseto6125/homebrew-tap`) so Homebrew can clone it.
- Either a published GitHub Release (for binary install) or a `head` URL (for source build via `brew install --HEAD`).

Neither precondition is met today, so the README line _"`brew tap coseto6125/tap && brew install egent-code-plexus` — Available after the tap formula is published with the first Release"_ is a placeholder.

## What it takes when we want to do it

1. **Create the tap repo** at `github.com/coseto6125/homebrew-tap` (Homebrew convention: repo name must start with `homebrew-`).
2. **Add `Formula/ecp.rb`** in that tap repo. Skeleton:

   ```ruby
   class Ecp < Formula
     desc "Code intelligence graph for AI agents and LLMs"
     homepage "https://github.com/coseto6125/egent-code-plexus"
     license "MIT OR Apache-2.0"

     # Stable: pinned to a tagged GitHub Release artifact.
     url "https://github.com/coseto6125/egent-code-plexus/releases/download/v0.2.0/ecp-v0.2.0-x86_64-apple-darwin.tar.gz"
     sha256 "<fill from release .sha256>"

     # Bleeding-edge: brew install --HEAD ecp builds from main without a Release.
     head "https://github.com/coseto6125/egent-code-plexus.git", branch: "main"

     depends_on "rust" => :build

     def install
       # Workspace has multiple bins — point cargo at the cli crate.
       system "cargo", "install", *std_cargo_args(path: "crates/ecp-cli")
     end

     test do
       assert_match "egent-code-plexus", shell_output("#{bin}/ecp --version")
       assert_equal 9, shell_output("#{bin}/ecp mcp tools").lines.count
     end
   end
   ```

3. **Multi-platform binaries**: for the stable path, the formula needs `on_macos` / `on_linux` arms with separate `url` + `sha256` per platform. Linux brew users are rare but supported.
4. **Test**: `brew install --build-from-source ./Formula/ecp.rb` locally, then `brew tap coseto6125/tap && brew install ecp` end-to-end.
5. **CI**: optionally add a release-time job that auto-updates `Formula/ecp.rb` in the tap repo with the new `url` + computed `sha256` on each tag push.

## Decision points to revisit

- **Stable vs HEAD-only**: HEAD-only formula doesn't require Releases at all — `brew install --HEAD ecp` does a source build. Cheaper to maintain but means brew users compile from scratch each `brew upgrade`.
- **Tap repo visibility**: must be public for Homebrew to clone. Aligns with going-public step.
- **Cask vs Formula**: ecp is a Rust CLI, Formula (build from source or fetch upstream binary) is correct. Cask would be wrong (that's for GUI .app bundles).

## Acceptance criteria

- `brew tap coseto6125/tap && brew install ecp` on a clean macOS box → working `ecp --version` + `ecp mcp tools` lists 9 tools.
- README install table marks the Homebrew row as live (drop the "Available after…" caveat).
- This doc moved to `docs/feat/.done/` or deleted.

## Related

- `install.sh` / `install.ps1` already cover the curl-pipe-shell path; brew is the macOS-idiomatic alternative.
- `release.yml` publishes the Release artifacts the stable Formula depends on.
