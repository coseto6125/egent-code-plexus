#!/usr/bin/env sh
# code-graph-nexus 一鍵安裝（Linux / macOS）
#
#   curl -sSfL https://github.com/coseto6125/code-graph-nexus/releases/latest/download/install.sh | sh
#   curl -sSfL https://github.com/coseto6125/code-graph-nexus/releases/download/v0.2.0/install.sh | sh
#
# 環境變數：
#   CGN_VERSION       指定版本（不含 v 前綴）。預設 latest。
#   CGN_INSTALL_DIR   安裝目錄。預設 $HOME/.local/bin，root 時 /usr/local/bin。
#   CGN_NO_VERIFY=1   跳過 SHA256 驗證（不建議）。
#   CGN_FORCE_CARGO=1 跳過 release binary，強制走 cargo install --git。
#
# 沒有 GitHub Release 或目標平台沒 prebuilt 時，會自動 fallback 到
# `cargo install --git`（需要 cargo / rustup）。

set -eu

REPO="coseto6125/code-graph-nexus"
BIN="cgn"
CGN_VERSION="${CGN_VERSION:-latest}"
CGN_FORCE_CARGO="${CGN_FORCE_CARGO:-0}"

# ---- 安裝目錄 ---- (resolved up-front so cargo fallback respects it too)
if [ -z "${CGN_INSTALL_DIR:-}" ]; then
  if [ "$(id -u)" -eq 0 ]; then
    CGN_INSTALL_DIR="/usr/local/bin"
  else
    CGN_INSTALL_DIR="$HOME/.local/bin"
  fi
fi

cargo_fallback() {
  reason="$1"
  if ! command -v cargo >/dev/null 2>&1; then
    echo "error: $reason" >&2
    echo "       and \`cargo\` not found in PATH — install Rust from https://rustup.rs," >&2
    echo "       then re-run this script (or wait for a prebuilt release)." >&2
    exit 1
  fi
  echo "==> $reason"
  echo "==> Falling back to \`cargo install --git\` (source build, may take a few minutes)"

  # Build into a private --root then move the binary to CGN_INSTALL_DIR.
  # `cargo install --git <url> <package>` requires the workspace package
  # name now that the workspace has multiple binaries; `--root <dir>`
  # places the binary at <dir>/bin/<bin> regardless of CGN_INSTALL_DIR
  # shape (`~/bin`, `~/.local/bin`, `/usr/local/bin` all work).
  build_root="$(mktemp -d 2>/dev/null || mktemp -d -t cgn-build)"
  trap 'rm -rf "$build_root"' EXIT
  if [ "${CGN_VERSION}" = "latest" ]; then
    cargo install --root "$build_root" --git "https://github.com/$REPO" code-graph-nexus --bin "$BIN" --locked
  else
    cargo install --root "$build_root" --git "https://github.com/$REPO" code-graph-nexus --tag "v${CGN_VERSION#v}" --bin "$BIN" --locked
  fi
  mkdir -p "$CGN_INSTALL_DIR"
  install -m 0755 "$build_root/bin/$BIN" "$CGN_INSTALL_DIR/$BIN"

  echo
  echo "✓ Installed $BIN via cargo → $CGN_INSTALL_DIR/$BIN"
  exit 0
}

if [ "${CGN_FORCE_CARGO}" = "1" ]; then
  cargo_fallback "CGN_FORCE_CARGO=1 set"
fi

# ---- 偵測 OS / ARCH → target triple ----
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"

case "$os/$arch" in
  linux/x86_64)         target="x86_64-unknown-linux-gnu" ;;
  linux/aarch64|linux/arm64) target="aarch64-unknown-linux-gnu" ;;
  darwin/x86_64)        target="x86_64-apple-darwin" ;;
  darwin/arm64|darwin/aarch64) target="aarch64-apple-darwin" ;;
  *)
    cargo_fallback "unsupported prebuilt platform $os/$arch (linux/macOS x86_64/aarch64 only)"
    ;;
esac

# ---- 解析版本 ----
if [ "$CGN_VERSION" = "latest" ]; then
  # 從 redirect 解析 latest tag，免 GitHub API 額度。
  # 若沒有 release（首次發佈前），URL 仍會 200 但 redirect 不含 /tag/。
  tag="$(curl -sSfLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null | sed -n 's|.*/tag/||p')"
  if [ -z "$tag" ]; then
    cargo_fallback "no published GitHub Release yet for $REPO"
  fi
else
  tag="v${CGN_VERSION#v}"
fi
version="${tag#v}"

# ---- 下載 ----
name="${BIN}-${tag}-${target}"
archive="${name}.tar.gz"
url="https://github.com/$REPO/releases/download/${tag}/${archive}"
sha_url="${url}.sha256"

tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t cgn)"
trap 'rm -rf "$tmpdir"' EXIT

echo "==> Downloading $BIN $version ($target)"
echo "    $url"
if ! curl -sSfL "$url" -o "$tmpdir/$archive"; then
  cargo_fallback "release asset for $target not found (tag $tag)"
fi

if [ "${CGN_NO_VERIFY:-0}" != "1" ]; then
  curl -sSfL "$sha_url" -o "$tmpdir/$archive.sha256"
  echo "==> Verifying SHA256"
  if command -v shasum >/dev/null 2>&1; then
    ( cd "$tmpdir" && shasum -a 256 -c "$archive.sha256" )
  elif command -v sha256sum >/dev/null 2>&1; then
    ( cd "$tmpdir" && sha256sum -c "$archive.sha256" )
  else
    echo "warning: no shasum/sha256sum; skipping verification" >&2
  fi
fi

# ---- 解壓 + 安裝 ----
tar -xzf "$tmpdir/$archive" -C "$tmpdir"
mkdir -p "$CGN_INSTALL_DIR"
install -m 0755 "$tmpdir/$name/$BIN" "$CGN_INSTALL_DIR/$BIN"

echo
echo "✓ Installed $BIN $version → $CGN_INSTALL_DIR/$BIN"
echo

# ---- PATH 提示 ----
case ":$PATH:" in
  *":$CGN_INSTALL_DIR:"*) ;;
  *)
    echo "  ⚠  $CGN_INSTALL_DIR is not in PATH. Add it:"
    case "$(basename "${SHELL:-/bin/sh}")" in
      bash) echo "       echo 'export PATH=\"$CGN_INSTALL_DIR:\$PATH\"' >> ~/.bashrc" ;;
      zsh)  echo "       echo 'export PATH=\"$CGN_INSTALL_DIR:\$PATH\"' >> ~/.zshrc" ;;
      fish) echo "       fish_add_path $CGN_INSTALL_DIR" ;;
      *)    echo "       export PATH=\"$CGN_INSTALL_DIR:\$PATH\"" ;;
    esac
    echo
    ;;
esac

echo "  Verify provenance:"
echo "    gh attestation verify $CGN_INSTALL_DIR/$BIN --owner coseto6125"
