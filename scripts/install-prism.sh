#!/usr/bin/env sh
set -eu

INSTALL_DIR="${PRISM_INSTALL_DIR:-$HOME/.prism/bin}"
REPO_SLUG="${PRISM_RELEASE_REPO:-Darth-Hidious/PRISM}"
BASE_URL="${PRISM_RELEASE_BASE_URL:-https://github.com/$REPO_SLUG/releases/download}"
VERSION="${PRISM_VERSION:-latest}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
  Linux) os="linux" ;;
  Darwin) os="macos" ;;
  *)
    echo "Unsupported OS: $uname_s" >&2
    exit 1
    ;;
esac

case "$uname_m" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *)
    echo "Unsupported architecture: $uname_m" >&2
    exit 1
    ;;
esac

asset="prism-${os}-${arch}.tar.gz"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT INT TERM

if [ "$VERSION" = "latest" ]; then
  download_url="https://github.com/$REPO_SLUG/releases/latest/download/$asset"
else
  download_url="$BASE_URL/$VERSION/$asset"
fi

mkdir -p "$INSTALL_DIR"

echo "Downloading $download_url"
curl -fsSL "$download_url" -o "$tmpdir/$asset"
tar -xzf "$tmpdir/$asset" -C "$tmpdir"

for bin in prism prism-node prism-tui; do
  if [ -f "$tmpdir/$bin" ]; then
    install -m 0755 "$tmpdir/$bin" "$INSTALL_DIR/$bin"
  fi
done

cat <<EOF
Installed PRISM into $INSTALL_DIR

Next:
  1. Add $INSTALL_DIR to PATH if needed.
  2. Run: prism

The Rust launcher will handle first-run setup, device login, and TUI startup.
EOF
