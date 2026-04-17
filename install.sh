#!/bin/sh
# vai installer — downloads the latest vai CLI binary from GitHub releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/jjordy/vai/main/install.sh | sh
set -eu

REPO="jjordy/vai"
INSTALL_DIR="${VAI_INSTALL_DIR:-$HOME/.local/bin}"

# Refuse to run as root
if [ "$(id -u)" = "0" ]; then
  echo "Error: do not run this installer as root." >&2
  echo "Run it as a regular user without sudo." >&2
  exit 1
fi

# Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)  TARGET_OS="unknown-linux-gnu" ;;
  Darwin) TARGET_OS="apple-darwin" ;;
  *) echo "Error: unsupported OS: $OS. Supported: Linux, macOS." >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64)  TARGET_ARCH="x86_64" ;;
  aarch64|arm64) TARGET_ARCH="aarch64" ;;
  *) echo "Error: unsupported architecture: $ARCH. Supported: x86_64, arm64." >&2; exit 1 ;;
esac

TARGET="${TARGET_ARCH}-${TARGET_OS}"

# Fetch latest release tag
VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep '"tag_name"' \
  | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
if [ -z "$VERSION" ]; then
  echo "Error: could not determine latest release version." >&2
  exit 1
fi

ARCHIVE="vai-${VERSION}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
URL="${BASE_URL}/${ARCHIVE}"
SUMS_URL="${BASE_URL}/SHA256SUMS"

echo "Installing vai ${VERSION} for ${TARGET}..."

# Download to a temporary directory
TMP=$(mktemp -d)
# shellcheck disable=SC2064
trap "rm -rf '$TMP'" EXIT

curl -fsSL "$URL" -o "$TMP/$ARCHIVE"
curl -fsSL "$SUMS_URL" -o "$TMP/SHA256SUMS"

# Verify checksum
cd "$TMP"
if command -v sha256sum >/dev/null 2>&1; then
  grep "$ARCHIVE" SHA256SUMS | sha256sum -c --quiet
elif command -v shasum >/dev/null 2>&1; then
  grep "$ARCHIVE" SHA256SUMS | shasum -a 256 -c --quiet
else
  echo "Warning: sha256sum and shasum not found — skipping checksum verification." >&2
fi
cd - >/dev/null

tar xzf "$TMP/$ARCHIVE" -C "$TMP"

# Install binary
mkdir -p "$INSTALL_DIR"
mv "$TMP/vai" "$INSTALL_DIR/vai"
chmod +x "$INSTALL_DIR/vai"

echo "vai ${VERSION} installed. Run 'vai login' to authenticate."

# Warn if INSTALL_DIR is not on PATH
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*)
    ;;
  *)
    echo ""
    echo "Warning: $INSTALL_DIR is not on your PATH."
    echo "Add the following line to your ~/.bashrc or ~/.zshrc:"
    echo ""
    echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo ""
    echo "Then run:  source ~/.bashrc   (or source ~/.zshrc)"
    ;;
esac
