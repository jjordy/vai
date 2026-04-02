#!/bin/bash
# vai installer — downloads the latest vai CLI binary from GitHub releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/jjordy/vai/main/install.sh | bash
set -euo pipefail

REPO="jjordy/vai"
INSTALL_DIR="${VAI_INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)  TARGET_OS="unknown-linux-gnu" ;;
  Darwin) TARGET_OS="apple-darwin" ;;
  *) echo "Error: unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64)  TARGET_ARCH="x86_64" ;;
  aarch64|arm64) TARGET_ARCH="aarch64" ;;
  *) echo "Error: unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TARGET="${TARGET_ARCH}-${TARGET_OS}"

# Fetch latest release tag
VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
if [ -z "$VERSION" ]; then
  echo "Error: could not determine latest release" >&2
  exit 1
fi

ARCHIVE="vai-${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"

echo "Installing vai ${VERSION} (${TARGET})..."

# Download and extract
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "$URL" -o "$TMP/$ARCHIVE"
tar xzf "$TMP/$ARCHIVE" -C "$TMP"

# Install
mkdir -p "$INSTALL_DIR"
mv "$TMP/vai" "$INSTALL_DIR/vai"
chmod +x "$INSTALL_DIR/vai"

echo "Installed vai to $INSTALL_DIR/vai"

# Check PATH
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
  echo ""
  echo "Add vai to your PATH:"
  echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi
