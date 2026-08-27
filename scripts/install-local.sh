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

    # Stage beside the destination, then rename over it.
    #
    # Installing straight onto $DST breaks a PRISM that is RUNNING. Writing into
    # the inode a process is executing corrupts that image, and on macOS every
    # later exec of the file is SIGKILLed: measured, `prism knowledge --help`
    # returned exit 137 with zero bytes on stdout AND stderr. That is the worst
    # possible failure shape — every CLI-backed tool fails with no reason, the
    # agent cannot say why, and `report_bug` is broken too, so it cannot even
    # report it. An `rm` first narrows the window but does not close it: an exec
    # landing between the unlink and the end of the copy sees a missing or
    # half-written file.
    #
    # rename(2) within one directory is atomic. The running process keeps its
    # old inode until it exits; every new exec sees a complete binary.
    TMP="${DST}.new.$$"
    rm -f "$TMP"

    # Copy the fresh build — do NOT re-sign, the linker signature is correct
    cp "$SRC" "$TMP"
    chmod +x "$TMP"

    # Only clear quarantine/provenance attributes — leave signature alone.
    # Done on the TEMP file so the binary is fully prepared before it becomes
    # visible under its real name.
    if [ "$(uname -s)" = "Darwin" ]; then
        xattr -d com.apple.quarantine "$TMP" 2>/dev/null || true
        xattr -d com.apple.provenance "$TMP" 2>/dev/null || true
    fi

    mv -f "$TMP" "$DST"
done

echo ""
echo "Installed: $(prism --version 2>/dev/null || echo "${INSTALL_DIR}/prism")"
