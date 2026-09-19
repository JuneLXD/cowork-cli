#!/bin/sh
# Build room-cli from source and install the `room` binary into ~/.local/bin.
# Usage: ./install.sh            (from a checkout)
set -e
here="$(cd "$(dirname "$0")" && pwd)"
dest="${ROOM_INSTALL_DIR:-$HOME/.local/bin}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

echo "building room (release)..."
cargo build --release --manifest-path "$here/Cargo.toml" --quiet
mkdir -p "$dest"
install -m 755 "$here/target/release/room" "$dest/room"
echo "installed $dest/room"

case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "note: $dest is not on your PATH. Add this to your shell profile:"
     echo "  export PATH=\"$dest:\$PATH\"" ;;
esac
echo "next: cd into a git repository and run: room init"
