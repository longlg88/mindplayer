#!/usr/bin/env bash
#
# MindPlayer installer — installs prebuilt release binaries (no build needed).
#
#   # one-liner: download + install the latest released TUI to ~/.local/bin
#   curl -fsSL https://raw.githubusercontent.com/longlg88/mindplayer/main/install.sh | bash
#
# Options:
#   --app          download + install the latest macOS app into /Applications
#   --build        build the TUI from source instead of downloading a release
#   --bin-dir DIR  install the TUI into DIR (default: ~/.local/bin)
#   --uninstall    remove the installed TUI binary
#   -h, --help     show this help
#
# Env: PREFIX=/usr/local         bin dir = $PREFIX/bin
#      APP_DIR=~/Applications     install the app there (no sudo)
#      MINDPLAYER_VERSION=vX.Y.Z  install that release instead of the latest
#
set -euo pipefail

REPO="longlg88/mindplayer"
REPO_URL="https://github.com/$REPO"
BIN_NAME="mindplayer"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || echo "")"

# --- args -----------------------------------------------------------------
BIN_DIR=""; UNINSTALL=0; DO_APP=0; FROM_SOURCE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --app) DO_APP=1; shift ;;
    --build|--from-source) FROM_SOURCE=1; shift ;;
    --bin-dir) BIN_DIR="${2:?--bin-dir needs a path}"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) sed -n '2,18p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$BIN_DIR" ]; then
  BIN_DIR="${PREFIX:+$PREFIX/bin}"; BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
fi
DEST="$BIN_DIR/$BIN_NAME"

need() { command -v "$1" >/dev/null 2>&1; }

# --- uninstall ------------------------------------------------------------
if [ "$UNINSTALL" -eq 1 ]; then
  if [ -e "$DEST" ]; then rm -f "$DEST"; echo "removed $DEST"; else echo "nothing at $DEST"; fi
  exit 0
fi

# --- platform -------------------------------------------------------------
case "$(uname -s)" in
  Darwin) OS=macos ;;
  Linux)  OS=linux ;;
  *) OS=unknown ;;
esac
case "$(uname -m)" in
  arm64|aarch64) ARCH=arm64 ;;
  x86_64|amd64)  ARCH=x86_64 ;;
  *) ARCH=unknown ;;
esac
# Published TUI assets: macos-arm64, linux-x86_64.
ASSET_ARCH="$ARCH"; [ "$OS" = "linux" ] && ASSET_ARCH=x86_64

# --- helpers --------------------------------------------------------------
latest_tag() {
  if need gh; then
    gh release view --repo "$REPO" --json tagName -q .tagName 2>/dev/null && return 0
  fi
  need curl || return 1
  curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/'
}

fetch() { # url dest
  if need curl; then curl -fL --retry 3 -o "$2" "$1"
  elif need wget; then wget -qO "$2" "$1"
  else echo "error: need curl or wget" >&2; return 1; fi
}

build_from_source() {
  need cargo || { echo "error: Rust (cargo) not found. Install from https://rustup.rs and retry." >&2; exit 1; }
  local src
  if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/Cargo.toml" ]; then
    src="$SCRIPT_DIR"
  else
    need git || { echo "error: git needed to fetch source" >&2; exit 1; }
    src="${MINDPLAYER_SRC:-$HOME/.cache/mindplayer/src}"
    if [ -d "$src/.git" ]; then
      git -C "$src" fetch --depth 1 --tags --force origin main
      git -C "$src" reset --hard FETCH_HEAD
      git -C "$src" clean -fdq
    else
      mkdir -p "$(dirname "$src")"; git clone --depth 1 "$REPO_URL" "$src"
    fi
  fi
  echo "▶ building $BIN_NAME from source ($src) …"
  cargo build --release --manifest-path "$src/Cargo.toml" -p mindplayer-tui
  mkdir -p "$BIN_DIR"; install -m 0755 "$src/target/release/$BIN_NAME" "$DEST"
  echo "✓ installed $BIN_NAME → $DEST (from source)"
}

install_tui_release() {
  local tag ver asset url tmp
  tag="${MINDPLAYER_VERSION:-$(latest_tag)}"
  [ -n "$tag" ] || { echo "error: could not find the latest release; try --build" >&2; exit 1; }
  ver="${tag#v}"
  asset="mindplayer-${ver}-${OS}-${ASSET_ARCH}.tar.gz"
  url="$REPO_URL/releases/download/$tag/$asset"
  tmp="$(mktemp -d)"
  echo "▶ downloading $asset ($tag) …"
  if ! fetch "$url" "$tmp/$asset"; then
    echo "  no prebuilt binary for $OS-$ASSET_ARCH — falling back to source build" >&2
    rm -rf "$tmp"; build_from_source; return
  fi
  tar -xzf "$tmp/$asset" -C "$tmp"
  mkdir -p "$BIN_DIR"; install -m 0755 "$tmp/$BIN_NAME" "$DEST"
  rm -rf "$tmp"
  echo "✓ installed $BIN_NAME $ver → $DEST"
}

install_app_release() {
  [ "$OS" = "macos" ] || { echo "error: --app is macOS-only" >&2; exit 1; }
  local tag ver asset url tmp app dir dest
  tag="${MINDPLAYER_VERSION:-$(latest_tag)}"
  [ -n "$tag" ] || { echo "error: could not find the latest release" >&2; exit 1; }
  ver="${tag#v}"
  asset="mindplayer-app-${ver}-macos-${ARCH}.tar.gz"
  url="$REPO_URL/releases/download/$tag/$asset"
  tmp="$(mktemp -d)"
  echo "▶ downloading $asset ($tag) …"
  fetch "$url" "$tmp/$asset" || { echo "error: no app asset $asset in $tag" >&2; rm -rf "$tmp"; exit 1; }
  tar -xzf "$tmp/$asset" -C "$tmp"
  app="$(/usr/bin/find "$tmp" -maxdepth 1 -name '*.app' | head -1)"
  [ -n "$app" ] || { echo "error: archive had no .app" >&2; rm -rf "$tmp"; exit 1; }
  dir="${APP_DIR:-/Applications}"; dest="$dir/$(basename "$app")"
  echo "▶ installing $(basename "$app") → $dest (replacing any existing copy)"
  if [ -e "$dest" ]; then rm -rf "$dest" 2>/dev/null || sudo rm -rf "$dest"; fi
  if ! { mkdir -p "$dir" 2>/dev/null && cp -R "$app" "$dest" 2>/dev/null; }; then
    sudo cp -R "$app" "$dest"
  fi
  # Locally-downloaded unsigned apps get quarantined; clear it so it launches.
  xattr -dr com.apple.quarantine "$dest" 2>/dev/null || true
  rm -rf "$tmp"
  echo "✓ installed the macOS app → $dest"
  echo "  launch:  open \"$dest\""
}

# --- run ------------------------------------------------------------------
if [ "$DO_APP" -eq 1 ]; then
  install_app_release
else
  if [ "$FROM_SOURCE" -eq 1 ]; then build_from_source; else install_tui_release; fi
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo; echo "⚠ $BIN_DIR is not on your PATH. Add to your shell rc:"; echo "    export PATH=\"$BIN_DIR:\$PATH\"" ;;
  esac
  echo
  echo "Done. Run it from the project whose sessions you want:"
  echo "    cd ~/your/project && $BIN_NAME        # or:  $BIN_NAME ~/your/project"
fi
