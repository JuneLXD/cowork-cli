#!/bin/sh
# Build cowork from source and install the `cowork` binary (and its `room` alias) into ~/.local/bin.
# Usage: ./scripts/install.sh    (from a checkout; COWORK_INSTALL_DIR, or the legacy ROOM_INSTALL_DIR, overrides the destination)
set -e
# This script lives in scripts/; the project root is its parent.
here="$(cd "$(dirname "$0")/.." && pwd)"
dest="${COWORK_INSTALL_DIR:-${ROOM_INSTALL_DIR:-$HOME/.local/bin}}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

echo "building cowork (release)..."
cargo build --release --manifest-path "$here/Cargo.toml" --quiet
mkdir -p "$dest"
install -m 755 "$here/target/release/cowork" "$dest/cowork"
install -m 755 "$here/target/release/room" "$dest/room"
echo "installed $dest/cowork (and the compatibility alias $dest/room)"

case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "note: $dest is not on your PATH. Add this to your shell profile:"
     echo "  export PATH=\"$dest:\$PATH\"" ;;
esac
echo "next: cd into a git repository and run: cowork init"
