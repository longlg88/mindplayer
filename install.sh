#!/usr/bin/env bash
#
# MindPlayer installer — works two ways:
#
#   # 1) one-liner (no clone needed): fetches source, builds, installs the TUI
#   curl -fsSL https://raw.githubusercontent.com/longlg88/mindplayer/main/install.sh | bash
#
#   # 2) from a clone
#   ./install.sh                 # build + install the `mindplayer` TUI to ~/.local/bin
#
# Options:
#   --app            also build the macOS app and install/replace it in
#                    /Applications (always the latest) — needs Node/npm
#   --bin-dir DIR    install the binary into DIR (default: ~/.local/bin)
#   --uninstall      remove the installed binary
#   -h, --help       show this help
#
# Env: PREFIX=/usr/local sets the bin dir to $PREFIX/bin.
#      APP_DIR=~/Applications installs the .app there (no sudo) instead of /Applications.
#
set -euo pipefail

REPO_URL="https://github.com/longlg88/mindplayer"
BIN_NAME="mindplayer"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || echo "")"

# --- args -----------------------------------------------------------------
BIN_DIR=""
UNINSTALL=0
BUILD_APP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --app) BUILD_APP=1; shift ;;
    --bin-dir) BIN_DIR="${2:?--bin-dir needs a path}"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) sed -n '2,19p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

# Default bin dir: $PREFIX/bin if PREFIX is set, else ~/.local/bin.
if [ -z "$BIN_DIR" ]; then
  BIN_DIR="${PREFIX:+$PREFIX/bin}"
  BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
fi
DEST="$BIN_DIR/$BIN_NAME"

# --- uninstall ------------------------------------------------------------
if [ "$UNINSTALL" -eq 1 ]; then
  if [ -e "$DEST" ]; then rm -f "$DEST"; echo "removed $DEST"; else echo "nothing at $DEST"; fi
  exit 0
fi

need() { command -v "$1" >/dev/null 2>&1; }

# --- locate (or fetch) the source -----------------------------------------
# Run from a clone if Cargo.toml sits next to this script; otherwise (curl|bash)
# fetch the source into a cache dir and build from there.
if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/Cargo.toml" ]; then
  SRC_DIR="$SCRIPT_DIR"
else
  need git || { echo "error: git not found (needed to fetch source). Install git and retry." >&2; exit 1; }
  SRC_DIR="${MINDPLAYER_SRC:-$HOME/.cache/mindplayer/src}"
  if [ -d "$SRC_DIR/.git" ]; then
    # Force the cache to exactly match the latest main. A plain `pull --ff-only`
    # silently no-ops on a shallow clone whose history diverged, which would
    # rebuild stale code — so fetch + hard reset to guarantee the newest commit.
    echo "▶ updating source to latest main in $SRC_DIR"
    git -C "$SRC_DIR" fetch --depth 1 origin main
    git -C "$SRC_DIR" reset --hard FETCH_HEAD
    git -C "$SRC_DIR" clean -fdq
  else
    echo "▶ fetching MindPlayer → $SRC_DIR"
    mkdir -p "$(dirname "$SRC_DIR")"
    git clone --depth 1 "$REPO_URL" "$SRC_DIR"
  fi
  echo "  source now at: $(git -C "$SRC_DIR" log --oneline -1)"
fi

# --- prerequisites --------------------------------------------------------
if ! need cargo; then
  echo "error: Rust (cargo) not found. Install it, then re-run:" >&2
  echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  exit 1
fi

# --- build + install the TUI ----------------------------------------------
echo "▶ building $BIN_NAME (release) …"
cargo build --release --manifest-path "$SRC_DIR/Cargo.toml" -p mindplayer-tui

SRC_BIN="$SRC_DIR/target/release/$BIN_NAME"
[ -x "$SRC_BIN" ] || { echo "error: build did not produce $SRC_BIN" >&2; exit 1; }

mkdir -p "$BIN_DIR"
install -m 0755 "$SRC_BIN" "$DEST"
echo "✓ installed $BIN_NAME → $DEST"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo; echo "⚠ $BIN_DIR is not on your PATH. Add to your shell rc:"; echo "    export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

# --- optional: macOS app --------------------------------------------------
if [ "$BUILD_APP" -eq 1 ]; then
  echo
  if ! need npm; then
    echo "error: --app needs Node/npm. Install from https://nodejs.org and retry." >&2
    exit 1
  fi
  echo "▶ building the macOS app (Tauri) …"
  ( cd "$SRC_DIR/app" && npm install && npm run build )

  # Install (replace) the freshly-built .app into the Applications dir so the
  # user always launches the latest. Default: /Applications, override with
  # APP_DIR=~/Applications for a no-sudo, per-user install.
  APP_DIR="${APP_DIR:-/Applications}"
  BUILT_APP="$(/usr/bin/find "$SRC_DIR/app/src-tauri/target/release/bundle/macos" -maxdepth 1 -name '*.app' 2>/dev/null | head -1)"
  if [ -z "$BUILT_APP" ]; then
    echo "✗ could not find the built .app under target/release/bundle/macos" >&2
    exit 1
  fi
  APP_NAME="$(basename "$BUILT_APP")"
  APP_DEST="$APP_DIR/$APP_NAME"
  echo "▶ installing $APP_NAME → $APP_DEST (replacing any existing copy)"
  if [ -e "$APP_DEST" ]; then rm -rf "$APP_DEST"; fi
  if mkdir -p "$APP_DIR" 2>/dev/null && cp -R "$BUILT_APP" "$APP_DEST" 2>/dev/null; then
    echo "✓ installed the macOS app → $APP_DEST"
  else
    echo "  (no write access to $APP_DIR — retrying with sudo)"
    sudo rm -rf "$APP_DEST"
    sudo cp -R "$BUILT_APP" "$APP_DEST"
    echo "✓ installed the macOS app → $APP_DEST (via sudo)"
  fi
  echo "  launch it with:  open \"$APP_DEST\""
fi

echo
echo "Done. Run it from the project whose sessions you want:"
echo "    cd ~/your/project && $BIN_NAME        # or:  $BIN_NAME ~/your/project"
echo "It drives whichever of codex / claude / kiro-cli you have on PATH."
