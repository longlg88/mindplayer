#!/usr/bin/env bash
#
# MindPlayer local installer.
#
# Builds the release binary and installs `mindplayer` into a bin dir on your
# PATH (default: ~/.local/bin). Re-running upgrades in place.
#
# Usage:
#   ./install.sh                 # build + install to ~/.local/bin
#   PREFIX=/usr/local ./install.sh   # install to /usr/local/bin (may need sudo)
#   ./install.sh --bin-dir DIR   # install to a specific dir
#   ./install.sh --uninstall     # remove the installed binary
#
set -euo pipefail

# Resolve repo root = the dir this script lives in (so it works from anywhere).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_NAME="mindplayer"

# --- args -----------------------------------------------------------------
BIN_DIR=""
UNINSTALL=0
while [ $# -gt 0 ]; do
  case "$1" in
    --bin-dir) BIN_DIR="${2:?--bin-dir needs a path}"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

# Default bin dir: $PREFIX/bin if PREFIX is set, else ~/.local/bin.
if [ -z "$BIN_DIR" ]; then
  if [ -n "${PREFIX:-}" ]; then
    BIN_DIR="$PREFIX/bin"
  else
    BIN_DIR="$HOME/.local/bin"
  fi
fi

DEST="$BIN_DIR/$BIN_NAME"

# --- uninstall ------------------------------------------------------------
if [ "$UNINSTALL" -eq 1 ]; then
  if [ -e "$DEST" ]; then
    rm -f "$DEST"
    echo "removed $DEST"
  else
    echo "nothing to remove at $DEST"
  fi
  exit 0
fi

# --- preflight ------------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust from https://rustup.rs and retry." >&2
  exit 1
fi

echo "▶ building $BIN_NAME (release) …"
cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml" -p mindplayer-tui

SRC="$SCRIPT_DIR/target/release/$BIN_NAME"
if [ ! -x "$SRC" ]; then
  echo "error: build did not produce $SRC" >&2
  exit 1
fi

# --- install --------------------------------------------------------------
mkdir -p "$BIN_DIR"
install -m 0755 "$SRC" "$DEST"
echo "✓ installed $BIN_NAME → $DEST"

# --- PATH hint ------------------------------------------------------------
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo
    echo "⚠ $BIN_DIR is not on your PATH. Add this to your shell rc:"
    echo "    export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac

echo
echo "Run it from the directory whose sessions you want:  $BIN_NAME"
echo "Optional CLIs it drives: codex, claude, kiro-cli (whichever you have on PATH)."
