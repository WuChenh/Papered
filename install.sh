#!/usr/bin/env sh
# Papered one-liner installer.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/WuChenh/papered/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/WuChenh/papered/main/install.sh | PAPERED_VERSION=v0.2.1 sh
#
# Installs `papered` and `papered-daemon` from the latest GitHub Release.
# Override target directory with INSTALL_DIR (default: ~/.local/bin).
# Pin a version with PAPERED_VERSION=vX.Y.Z.

set -eu
set -f

REPO="${PAPERED_REPO:-WuChenh/papered}"
GITHUB_API="https://api.github.com"
GITHUB_RELEASES="https://github.com"
VERSION="${PAPERED_VERSION:-}"
INSTALL_DIR="${INSTALL_DIR:-}"

say() { printf '\033[1;34m[papered]\033[0m %s\n' "$*"; }
err() { printf '\033[1;31m[papered]\033[0m %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || err "required tool not found: $1 (install it and retry)"; }

detect_os() {
  case "$(uname -s)" in
    Linux*)  echo "unknown-linux-gnu" ;;
    Darwin*) echo "apple-darwin" ;;
    *)       err "unsupported operating system: $(uname -s)" ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64)    echo "x86_64" ;;
    arm64|aarch64)   echo "aarch64" ;;
    *)               err "unsupported architecture: $(uname -m)" ;;
  esac
}

resolve_version() {
  if [ -n "$VERSION" ]; then
    # Strip leading 'v' if the user passed it, then add it back for tag lookup.
    case "$VERSION" in v*) ;; *) VERSION="v$VERSION";; esac
    echo "$VERSION"
    return
  fi
  need curl
  tag=$(curl -fsSL "$GITHUB_API/repos/$REPO/releases/latest" \
    | grep -m1 '"tag_name"' \
    | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/') || \
    err "failed to fetch latest release from $GITHUB_API/repos/$REPO/releases/latest"
  [ -n "$tag" ] || err "no release tag found (is the repo public and does it have a release?)"
  echo "$tag"
}

choose_install_dir() {
  if [ -n "$INSTALL_DIR" ]; then
    mkdir -p "$INSTALL_DIR" || err "cannot create $INSTALL_DIR"
    echo "$INSTALL_DIR"
    return
  fi
  # Always user-scope: ~/.local/bin is on PATH for most modern distros and
  # never requires elevated privileges. We do not touch system directories.
  mkdir -p "$HOME/.local/bin" || err "cannot create $HOME/.local/bin"
  echo "$HOME/.local/bin"
}

check_path() {
  case ":$PATH:" in
    *":$1:"*) ;;
    *)
      say "NOTE: $1 is not in your PATH. Add it:"
      say "  export PATH=\"$1:\$PATH\""
      ;;
  esac
}

# -------- main --------
need curl
need tar

OS=$(detect_os)
ARCH=$(detect_arch)
TARGET="${ARCH}-${OS}"
say "detected target: $TARGET"

TAG=$(resolve_version)
say "installing release: $TAG"

BASE_URL="$GITHUB_RELEASES/$REPO/releases/download/$TAG"
ARCHIVE="papered-${TAG}-${TARGET}.tar.gz"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT INT TERM HUP

say "fetching SHA256SUMS.txt ..."
curl -fsSL -o "$TMPDIR/SHA256SUMS.txt" "$BASE_URL/SHA256SUMS.txt" || \
  err "could not download $BASE_URL/SHA256SUMS.txt"

EXPECTED=$(awk -v a="$ARCHIVE" '$2 == a {print $1}' "$TMPDIR/SHA256SUMS.txt")
[ -n "$EXPECTED" ] || err "no checksum entry for $ARCHIVE in SHA256SUMS.txt"

say "downloading $ARCHIVE ..."
curl -fL -o "$TMPDIR/$ARCHIVE" "$BASE_URL/$ARCHIVE" || \
  err "download failed: $BASE_URL/$ARCHIVE (check that $TARGET exists in this release)"

say "verifying checksum ..."
if command -v shasum >/dev/null 2>&1; then
  ACTUAL=$(shasum -a 256 "$TMPDIR/$ARCHIVE" | awk '{print $1}')
elif command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$TMPDIR/$ARCHIVE" | awk '{print $1}')
else
  err "neither shasum nor sha256sum is available — cannot verify archive integrity"
fi

if [ "$ACTUAL" != "$EXPECTED" ]; then
  err "checksum mismatch for $ARCHIVE
  expected: $EXPECTED
  got:      $ACTUAL"
fi
say "checksum OK"

say "extracting ..."
tar -xzf "$TMPDIR/$ARCHIVE" -C "$TMPDIR"
EXTRACTED_DIR="$TMPDIR/papered-${TAG}-${TARGET}"
[ -d "$EXTRACTED_DIR" ] || err "archive did not contain expected directory: papered-${TAG}-${TARGET}"

DEST=$(choose_install_dir)
say "installing to $DEST ..."

for bin in papered papered-daemon; do
  src="$EXTRACTED_DIR/$bin"
  dst="$DEST/$bin"
  [ -f "$src" ] || err "binary missing from archive: $bin"
  if [ -f "$dst" ]; then
    rm -f "$dst" || err "cannot remove existing $dst"
  fi
  # Never escalate to sudo — install strictly under user ownership.
  cp "$src" "$dst" || err "failed to copy $bin to $DEST"
  chmod 0755 "$dst" || err "failed to chmod $dst"
done

check_path "$DEST"

say "installed papered and papered-daemon to $DEST"
say "run 'papered ui' to start the daemon and open the web UI."
