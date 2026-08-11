#!/bin/sh
# torq installer — downloads the release binary for this platform.
#   curl -fsSL https://raw.githubusercontent.com/Saswatsusmoy/TorQ/main/install.sh | sh
#
# Overrides (for testing or custom mirrors):
#   TORQ_REPO, TORQ_VERSION (tag like v0.1.0; default latest), TORQ_BASE_URL,
#   TORQ_INSTALL_DIR (default ~/.local/bin)
set -eu

REPO="${TORQ_REPO:-Saswatsusmoy/TorQ}"
VERSION="${TORQ_VERSION:-latest}"
INSTALL_DIR="${TORQ_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) TARGET="aarch64-apple-darwin" ;;
    Darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
    Linux-aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
    Linux-x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
    *) echo "torq: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

if [ -n "${TORQ_BASE_URL:-}" ]; then
    BASE="$TORQ_BASE_URL"
elif [ "$VERSION" = "latest" ]; then
    BASE="https://github.com/$REPO/releases/latest/download"
else
    BASE="https://github.com/$REPO/releases/download/$VERSION"
fi

URL="$BASE/torq-$TARGET.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "torq: fetching $URL"
curl -fsSL "$URL" -o "$TMP/torq.tar.gz"
tar -xzf "$TMP/torq.tar.gz" -C "$TMP"

mkdir -p "$INSTALL_DIR"
install -m 0755 "$TMP/torq" "$INSTALL_DIR/torq"

echo "torq: installed to $INSTALL_DIR/torq"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "torq: add $INSTALL_DIR to your PATH, e.g.  export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac
"$INSTALL_DIR/torq" --version
