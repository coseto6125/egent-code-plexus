#!/usr/bin/env sh
# gnx-rs 一鍵安裝（Linux / macOS）
#
#   curl -sSfL https://github.com/e-nor/gitnexus-rs/releases/latest/download/install.sh | sh
#   curl -sSfL https://github.com/e-nor/gitnexus-rs/releases/download/v0.1.0/install.sh | sh
#
# 環境變數：
#   GNX_VERSION   指定版本（不含 v 前綴）。預設 latest。
#   GNX_INSTALL_DIR  安裝目錄。預設 $HOME/.local/bin，root 時 /usr/local/bin。
#   GNX_NO_VERIFY=1  跳過 SHA256 驗證（不建議）。

set -eu

REPO="e-nor/gitnexus-rs"
BIN="gnx"
GNX_VERSION="${GNX_VERSION:-latest}"

# ---- 安裝目錄 ----
if [ -z "${GNX_INSTALL_DIR:-}" ]; then
  if [ "$(id -u)" -eq 0 ]; then
    GNX_INSTALL_DIR="/usr/local/bin"
  else
    GNX_INSTALL_DIR="$HOME/.local/bin"
  fi
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
    echo "error: unsupported platform $os/$arch" >&2
    echo "       supported: linux x86_64/aarch64, macOS x86_64/aarch64" >&2
    exit 1
    ;;
esac

# ---- 解析版本 ----
if [ "$GNX_VERSION" = "latest" ]; then
  # 從 redirect 解析 latest tag，免 GitHub API 額度
  tag="$(curl -sSfLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" | sed 's|.*/tag/||')"
  if [ -z "$tag" ]; then
    echo "error: unable to resolve latest version" >&2
    exit 1
  fi
else
  tag="v${GNX_VERSION#v}"
fi
version="${tag#v}"

# ---- 下載 ----
name="${BIN}-${tag}-${target}"
archive="${name}.tar.gz"
url="https://github.com/$REPO/releases/download/${tag}/${archive}"
sha_url="${url}.sha256"

tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t gnx)"
trap 'rm -rf "$tmpdir"' EXIT

echo "==> Downloading $BIN $version ($target)"
echo "    $url"
curl -sSfL "$url"     -o "$tmpdir/$archive"

if [ "${GNX_NO_VERIFY:-0}" != "1" ]; then
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
mkdir -p "$GNX_INSTALL_DIR"
install -m 0755 "$tmpdir/$name/$BIN" "$GNX_INSTALL_DIR/$BIN"

echo
echo "✓ Installed $BIN $version → $GNX_INSTALL_DIR/$BIN"
echo

# ---- PATH 提示 ----
case ":$PATH:" in
  *":$GNX_INSTALL_DIR:"*) ;;
  *)
    echo "  ⚠  $GNX_INSTALL_DIR is not in PATH. Add it:"
    case "$(basename "${SHELL:-/bin/sh}")" in
      bash) echo "       echo 'export PATH=\"$GNX_INSTALL_DIR:\$PATH\"' >> ~/.bashrc" ;;
      zsh)  echo "       echo 'export PATH=\"$GNX_INSTALL_DIR:\$PATH\"' >> ~/.zshrc" ;;
      fish) echo "       fish_add_path $GNX_INSTALL_DIR" ;;
      *)    echo "       export PATH=\"$GNX_INSTALL_DIR:\$PATH\"" ;;
    esac
    echo
    ;;
esac

echo "  Verify provenance:"
echo "    gh attestation verify $GNX_INSTALL_DIR/$BIN --owner e-nor"
