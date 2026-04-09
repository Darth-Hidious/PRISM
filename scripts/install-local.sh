#!/usr/bin/env bash
set -euo pipefail

# Build and install PRISM locally from source.
# Usage: ./scripts/install-local.sh
#
# This builds a release binary and installs it to ~/.prism/bin/
# WITHOUT breaking the linker code signature (the #1 cause of
# "Code Signature Invalid" crashes on macOS).

INSTALL_DIR="${PRISM_INSTALL_DIR:-$HOME/.prism/bin}"

echo "Building PRISM release binary..."
cargo build --release --bin prism --bin prism-node

echo "Installing to ${INSTALL_DIR}..."
mkdir -p "$INSTALL_DIR"

for bin in prism prism-node; do
    SRC="target/release/${bin}"
    DST="${INSTALL_DIR}/${bin}"

    [ -f "$SRC" ] || continue

    # Remove old binary first (avoids macOS trust cache confusion)
    rm -f "$DST"

    # Copy the fresh build — do NOT re-sign, the linker signature is correct
    cp "$SRC" "$DST"

    # Only clear quarantine/provenance attributes — leave signature alone
    if [ "$(uname -s)" = "Darwin" ]; then
        xattr -d com.apple.quarantine "$DST" 2>/dev/null || true
        xattr -d com.apple.provenance "$DST" 2>/dev/null || true
    fi
done

echo ""
echo "Installed: $(prism --version 2>/dev/null || echo "${INSTALL_DIR}/prism")"
