#!/usr/bin/env bash
# Bootstrap Wave 1 language sample repos into .sample_repo/<lang>/
# Run from repo root: bash scripts/parity/bootstrap_sample_repos.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SAMPLE_DIR="${REPO_ROOT}/.sample_repo"
mkdir -p "${SAMPLE_DIR}"

# Emit progress messages to stderr so stdout can be captured for stats
log() { echo "[bootstrap] $*" >&2; }

clone_if_missing() {
    local lang="$1"; shift
    local dest="${SAMPLE_DIR}/${lang}"
    if [[ -d "${dest}/.git" ]]; then
        log "${lang}: already cloned — skipping"
        return 0
    fi
    # URL is conventionally the last arg of each caller; the old `$1`
    # form after `shift` would print the first flag (e.g. `--depth`)
    # instead, so logs read "lua: cloning --depth ..." rather than the
    # actual repo URL.
    local url="${*: -1}"
    log "${lang}: cloning ${url} ..."
    git clone "$@" "${dest}"
    log "${lang}: done"
}

# ── Wave 1 repos ──────────────────────────────────────────────────────────────

clone_if_missing lua \
    --depth 1 \
    https://github.com/kikito/middleclass.git

clone_if_missing solidity \
    --depth 1 \
    https://github.com/OpenZeppelin/openzeppelin-contracts.git

clone_if_missing bash \
    --depth 1 \
    https://github.com/Bash-it/bash-it.git

clone_if_missing zig \
    --depth 1 \
    https://github.com/karlseguin/http.zig.git

clone_if_missing crystal \
    --depth 1 \
    https://github.com/kemalcr/kemal.git

clone_if_missing dockerfile \
    --depth 1 \
    https://github.com/docker-library/postgres.git

# Move (aptos-core is huge — sparse checkout, only aptos-move/framework/)
MOVE_DEST="${SAMPLE_DIR}/move"
if [[ -d "${MOVE_DEST}/.git" ]]; then
    log "move: already cloned — skipping"
else
    log "move: sparse-cloning aptos-core (aptos-move/framework/ only) ..."
    git clone \
        --depth 1 \
        --filter=blob:none \
        --sparse \
        https://github.com/aptos-labs/aptos-core.git \
        "${MOVE_DEST}"
    git -C "${MOVE_DEST}" sparse-checkout set aptos-move/framework
    log "move: done"
fi

# ── Disk usage summary ────────────────────────────────────────────────────────
log "Disk usage:"
du -sh "${SAMPLE_DIR}"/* 2>/dev/null || true
