#!/bin/sh
set -e

# Prist installer for Linux and macOS.
# Downloads the matching prebuilt binary release from GitHub.

PRIST_REPO="SnaidenP/prist"
PRIST_HOME="${PRIST_HOME:-$HOME/.prist}"
BIN_DIR="$PRIST_HOME/bin"

info() {
    printf "\033[0;34m→\033[0m %s\n" "$1"
}

success() {
    printf "\033[0;32m✓\033[0m %s\n" "$1"
}

error() {
    printf "\033[0;31m✗\033[0m %s\n" "$1" >&2
    exit 1
}

# 1. Detect OS
OS="$(uname -s)"
case "$OS" in
    Linux*)     TARGET_OS="unknown-linux-gnu";;
    Darwin*)    TARGET_OS="apple-darwin";;
    *)          error "Unsupported operating system: $OS. Prist install script supports Linux and macOS.";;
esac

# 2. Detect Architecture
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|amd64)   TARGET_ARCH="x86_64";;
    arm64|aarch64)  TARGET_ARCH="aarch64";;
    *)              error "Unsupported architecture: $ARCH";;
esac

TARGET="${TARGET_ARCH}-${TARGET_OS}"
info "Detected platform: $OS ($TARGET)"

# 3. Create Prist directories
mkdir -p "$BIN_DIR"
mkdir -p "$PRIST_HOME/envs"
mkdir -p "$PRIST_HOME/shared"

# 4. Resolve latest release tag
LATEST_TAG=$(curl -sL "https://api.github.com/repos/$PRIST_REPO/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
if [ -z "$LATEST_TAG" ]; then
    LATEST_TAG="v0.3.0"
fi

TARBALL_NAME="prist-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/$PRIST_REPO/releases/download/$LATEST_TAG/$TARBALL_NAME"

info "Downloading Prist $LATEST_TAG ($TARBALL_NAME)..."
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

if curl -sL "$DOWNLOAD_URL" -o "$TMP_DIR/$TARBALL_NAME"; then
    tar -xzf "$TMP_DIR/$TARBALL_NAME" -C "$TMP_DIR"
    cp "$TMP_DIR/prist" "$BIN_DIR/prist"
    chmod +x "$BIN_DIR/prist"
    success "Prist binary installed to $BIN_DIR/prist"
else
    error "Failed to download release asset from $DOWNLOAD_URL"
fi

# 5. Shell PATH setup
SHELL_NAME="$(basename "${SHELL:-sh}")"
PROFILE=""

case "$SHELL_NAME" in
    bash)   PROFILE="$HOME/.bashrc";;
    zsh)    PROFILE="$HOME/.zshrc";;
    fish)   PROFILE="$HOME/.config/fish/config.fish";;
    *)      PROFILE="$HOME/.profile";;
esac

PATH_LINE="export PATH=\"$BIN_DIR:\$PATH\""

if [ -f "$PROFILE" ]; then
    if ! grep -q "$BIN_DIR" "$PROFILE"; then
        echo "" >> "$PROFILE"
        echo "# Prist Flutter Version Manager" >> "$PROFILE"
        echo "$PATH_LINE" >> "$PROFILE"
        success "Added $BIN_DIR to $PROFILE"
    fi
fi

echo ""
success "Prist $LATEST_TAG installed successfully!"
echo "  Run 'prist --help' to get started."
echo "  Restart your shell or run: $PATH_LINE"
