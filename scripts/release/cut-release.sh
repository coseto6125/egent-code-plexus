#!/usr/bin/env bash
# cut-release.sh — version-bump + changelog + Release PR for a workspace that is
# NOT published to crates.io.
#
# Why this exists instead of release-plz: release-plz computes the next version
# by running `cargo package` on every crate, which hard-fails here because
# ecp-analyzer pulls in vendored tree-sitter grammars via path/git deps with no
# crates.io `version` (intentional — we don't publish to crates.io). git_only /
# publish=false do not make release-plz skip that `cargo package` step. So we do
# the version math ourselves from git tags + conventional commits, which is the
# only part release-plz was giving us. The heavy lifting (5-platform builds,
# GitHub Release, npm/PyPI publish) stays in release.yml, triggered by the v* tag
# this script (or its companion tag step) pushes.
#
# Flow (mirrors the release-plz division of labour):
#   1. cut-release.sh           → open/refresh a Release PR (bump + CHANGELOG)
#   2. human reviews + merges the PR
#   3. cut-release.sh --tag      → on the merged commit, push the v{version} tag
#                                  which hands off to release.yml
#
# Usage:
#   scripts/release/cut-release.sh [--bump auto|patch|minor|major] [--dry-run] [--pr] [--tag]
#
#   --bump   how to derive the next version (default: auto — feat ⇒ minor,
#            otherwise fix/perf ⇒ patch; breaking change footer ⇒ major).
#   --dry-run  print what would happen; touch nothing. (default when no action flag)
#   --pr     create the bump commit on a release branch + open the Release PR.
#   --tag    push the v{version} git tag for the CURRENT Cargo.toml version
#            (run after the Release PR is merged to main).

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

CARGO_TOML="Cargo.toml"
CHANGELOG="CHANGELOG.md"
TAG_PREFIX="v"

BUMP="auto"
ACTION="dry-run"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bump) BUMP="$2"; shift 2 ;;
    --dry-run) ACTION="dry-run"; shift ;;
    --pr) ACTION="pr"; shift ;;
    --tag) ACTION="tag"; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# ── current version (single source of truth: [workspace.package].version) ─────
current_version() {
  # First `version = "x.y.z"` after the [workspace.package] header.
  awk '
    /^\[workspace\.package\]/ { inpkg=1; next }
    inpkg && /^\[/ { inpkg=0 }
    inpkg && /^version *=/ {
      gsub(/[^0-9.]/, "", $0); print; exit
    }
  ' "$CARGO_TOML"
}

# ── last released tag (vX.Y.Z), empty if none ────────────────────────────────
last_tag() {
  git tag --list "${TAG_PREFIX}*" --sort=-version:refname | head -1
}

# ── decide bump level from conventional commits since last tag ────────────────
auto_bump_level() {
  local since="$1" subjects
  subjects="$(git log "${since}..HEAD" --pretty=format:'%s%n%b' 2>/dev/null || true)"
  if grep -qiE '(^|\n)BREAKING CHANGE|!:' <<<"$subjects"; then
    echo "major"; return
  fi
  if grep -qE '^feat(\(|:)' <<<"$subjects"; then
    echo "minor"; return
  fi
  if grep -qE '^(fix|perf|refactor)(\(|:)' <<<"$subjects"; then
    echo "patch"; return
  fi
  echo "none"
}

# ── apply a bump level to X.Y.Z ──────────────────────────────────────────────
apply_bump() {
  local ver="$1" level="$2" major minor patch
  IFS='.' read -r major minor patch <<<"$ver"
  case "$level" in
    major) echo "$((major+1)).0.0" ;;
    minor) echo "${major}.$((minor+1)).0" ;;
    patch) echo "${major}.${minor}.$((patch+1))" ;;
    *) echo "$ver" ;;
  esac
}

# ── rewrite all version sites: workspace.package + 3 internal dep constraints ─
bump_cargo_versions() {
  local old="$1" new="$2"
  # workspace.package version
  sed -i -E "s/^(version = )\"${old//./\\.}\"/\1\"${new}\"/" "$CARGO_TOML"
  # internal dep constraints (path + version) — keep the path, swap the version
  local dep
  for dep in ecp-core ecp-analyzer ecp-mcp; do
    sed -i -E \
      "s|^(${dep} = \{ path = \"crates/${dep}\", version = )\"${old//./\\.}\"|\1\"${new}\"|" \
      "$CARGO_TOML"
  done
}

# ── build the changelog section for this release ─────────────────────────────
changelog_section() {
  local new="$1" since="$2" date
  date="$(date -u +%Y-%m-%d)"
  echo "## ${TAG_PREFIX}${new} - ${date}"
  echo
  local types=("feat:Features" "fix:Bug Fixes" "perf:Performance" "refactor:Refactor")
  local entry type label found_any=0
  for entry in "${types[@]}"; do
    type="${entry%%:*}"; label="${entry##*:}"
    local lines
    lines="$(git log "${since}..HEAD" --pretty=format:'%s' 2>/dev/null \
      | grep -E "^${type}(\(|:)" | grep -vE '^chore\(deps\)' || true)"
    if [[ -n "$lines" ]]; then
      echo "### ${label}"
      echo
      while IFS= read -r l; do
        # strip the `type(scope): ` prefix for a cleaner bullet
        echo "- ${l#*: }"
      done <<<"$lines"
      echo
      found_any=1
    fi
  done
  [[ "$found_any" == 1 ]] || echo "- (no user-facing changes)"
}

prepend_changelog() {
  local section="$1" tmp
  tmp="$(mktemp)"
  {
    echo "# Changelog"
    echo
    echo "$section"
    if [[ -f "$CHANGELOG" ]]; then
      # drop the existing leading "# Changelog" header, keep the rest
      tail -n +2 "$CHANGELOG" | sed '/./,$!d'
    fi
  } >"$tmp"
  mv "$tmp" "$CHANGELOG"
}

# ─────────────────────────────────────────────────────────────────────────────
CUR="$(current_version)"
[[ -n "$CUR" ]] || { echo "error: cannot read current version from $CARGO_TOML" >&2; exit 1; }
LAST="$(last_tag)"
SINCE="${LAST:-$(git rev-list --max-parents=0 HEAD | tail -1)}"

LEVEL="$BUMP"
[[ "$BUMP" == "auto" ]] && LEVEL="$(auto_bump_level "$SINCE")"

if [[ "$ACTION" == "tag" ]]; then
  TAG="${TAG_PREFIX}${CUR}"
  if git rev-parse "$TAG" >/dev/null 2>&1; then
    echo "tag $TAG already exists — nothing to do" >&2; exit 0
  fi
  echo "Pushing tag $TAG for current version $CUR (hands off to release.yml)…"
  git tag -a "$TAG" -m "Release $TAG"
  git push origin "$TAG"
  echo "Pushed $TAG."
  exit 0
fi

if [[ "$LEVEL" == "none" ]]; then
  echo "No release-worthy commits since ${LAST:-repo start} (only chore/docs). Nothing to bump." >&2
  exit 0
fi

NEW="$(apply_bump "$CUR" "$LEVEL")"
echo "current=$CUR  bump=$LEVEL  next=$NEW  (since ${LAST:-repo-start})"

if [[ "$ACTION" == "dry-run" ]]; then
  echo "--- dry-run: changelog preview ---"
  changelog_section "$NEW" "$SINCE"
  echo "--- dry-run: would rewrite version $CUR → $NEW in $CARGO_TOML (4 sites) ---"
  echo "Re-run with --pr to open the Release PR, or --bump <level> to override."
  exit 0
fi

# ── --pr: branch, bump, changelog, commit, push, open PR ─────────────────────
BRANCH="release/${NEW}"
echo "Creating release branch $BRANCH…"
git switch -c "$BRANCH" 2>/dev/null || git switch "$BRANCH"

bump_cargo_versions "$CUR" "$NEW"
prepend_changelog "$(changelog_section "$NEW" "$SINCE")"
# keep Cargo.lock's workspace entries in step with the new version
cargo update --workspace --offline >/dev/null 2>&1 || true

# The landing pages carry the version, so the bump has to regenerate them or
# CI's site --check fails on the Release PR itself.
if command -v node >/dev/null 2>&1; then
  # The pages carry a content date. It lives in seo.json rather than being read
  # from a clock so that `--check` stays reproducible; a release is the moment
  # it legitimately advances.
  today="$(date -u +%Y-%m-%d)"
  node -e '
    const fs = require("fs");
    const p = "docs/ecp-landing/seo.json";
    const d = JSON.parse(fs.readFileSync(p, "utf8"));
    d.contentDate = process.argv[1];
    fs.writeFileSync(p, JSON.stringify(d, null, 2) + "\n");
  ' "$today"
  node scripts/site/build_site.mjs >/dev/null
  git add docs/ecp-landing
else
  echo "warning: node not found — landing pages left at ${CUR}; run scripts/site/build_site.mjs" >&2
fi

git add "$CARGO_TOML" "$CHANGELOG" Cargo.lock 2>/dev/null || git add "$CARGO_TOML" "$CHANGELOG"
git commit -m "release: ${CUR} -> ${NEW} — bump workspace + internal dep versions"
git push -u origin "$BRANCH"

gh pr create \
  --base main --head "$BRANCH" \
  --title "release: ${CUR} -> ${NEW}" \
  --label release \
  --body "Automated release bump by \`scripts/release/cut-release.sh\`.

- workspace + ecp-core/ecp-analyzer/ecp-mcp version: \`${CUR}\` → \`${NEW}\` (${LEVEL})
- CHANGELOG.md updated from conventional commits since ${LAST:-repo start}

On merge, run \`scripts/release/cut-release.sh --tag\` on main to push \`v${NEW}\`,
which triggers release.yml (5-platform build + GitHub Release + npm/PyPI publish)."

echo "Opened Release PR for v${NEW}. Merge it, then run: scripts/release/cut-release.sh --tag"
