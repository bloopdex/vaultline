#!/usr/bin/env sh
# Vaultline installer (Linux x86_64 — the published static musl build per
# ADR-V0-1 runs on any libc).
#
# Downloads the release binary for the declared version, verifies its
# SHA-256 against the published checksum file, and installs it to
# ~/.local/bin/vaultline.
#
# Usage:
#   sh scripts/install.sh [VERSION]
#
# The release base is the repository's published release downloads;
# VAULTLINE_RELEASES_BASE overrides it (mirrors, self-hosted proxies).
set -eu

VERSION="${1:-0.8.0}"
BASE="${VAULTLINE_RELEASES_BASE:-https://github.com/bloopdex/vaultline/releases/download}"

ASSET="vaultline-${VERSION}-x86_64-unknown-linux-musl"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "downloading $BASE/v$VERSION/SHA256SUMS"
curl -fsSL "$BASE/v$VERSION/SHA256SUMS" -o "$TMP/SHA256SUMS"
echo "downloading $BASE/v$VERSION/$ASSET"
curl -fsSL "$BASE/v$VERSION/$ASSET" -o "$TMP/$ASSET"

cd "$TMP"
grep "  ${ASSET}\$" SHA256SUMS | sha256sum -c -
chmod +x "$ASSET"

DEST="${HOME}/.local/bin"
mkdir -p "$DEST"
cp "$ASSET" "$DEST/vaultline"
echo "installed $DEST/vaultline"
"$DEST/vaultline" version
echo
echo "make sure $DEST is on your PATH, then see the quick start in the README"
