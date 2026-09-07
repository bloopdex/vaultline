#!/usr/bin/env sh
# Vaultline installer (Linux x86_64, gnu libc — the tag-driven release
# workflow also publishes the static musl build per ADR-V0-1).
#
# Downloads the release binary for the declared version, verifies its
# SHA-256 against the published checksum file, and installs it to
# ~/.local/bin/vaultline.
#
# Usage:
#   sh scripts/install.sh [VERSION]
#
# The release base defaults to a placeholder: the repository is not
# hosted yet (the hosting decision is recorded open). Set
# VAULTLINE_RELEASES_BASE to the published release download root once
# it is — the scripts stay valid until the placeholder is replaced.
set -eu

VERSION="${1:-0.8.0}"
BASE="${VAULTLINE_RELEASES_BASE:-https://PLACEHOLDER-UNTIL-THE-REPOSITORY-IS-HOSTED/releases/download}"

case "$BASE" in
    *PLACEHOLDER*)
        echo "error: the release base is not published yet (the repository is not hosted); set VAULTLINE_RELEASES_BASE" >&2
        exit 1
        ;;
esac

ASSET="vaultline-${VERSION}-x86_64-unknown-linux-gnu"
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
