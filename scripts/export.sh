#!/bin/sh
# Build a self-contained release bundle for Linux x86_64 under export/ (gitignored):
#   export/cowork-<version>-x86_64-linux/   cowork, room, install.sh, README.txt, SHA256SUMS
#   export/cowork-<version>-x86_64-linux.tar.gz
# The bundle installs on Ubuntu 20.04 or newer without Rust. Usage: ./scripts/export.sh
set -e
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
version="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
arch="$(uname -m)"
name="cowork-${version}-${arch}-linux"
out="export/$name"

echo "building cowork $version (release)..."
cargo build --release --quiet 2>/dev/null || cargo build --release
rm -rf "$out" "export/$name.tar.gz"
mkdir -p "$out"
install -m 755 target/release/cowork "$out/cowork"
install -m 755 target/release/room "$out/room"
install -m 644 LICENSE "$out/LICENSE"

cat > "$out/install.sh" <<'INSTALL'
#!/bin/sh
# Install cowork on Ubuntu (20.04 or newer) or any x86_64 Linux with glibc 2.30+.
# Usage:
#   ./install.sh              installs into ~/.local/bin (no root needed)
#   sudo ./install.sh --system   installs into /usr/local/bin for every user
#   COWORK_INSTALL_DIR=/some/dir ./install.sh
set -e
here="$(cd "$(dirname "$0")" && pwd)"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) ;;
  *) echo "this bundle is for Linux x86_64; found $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
if [ "$1" = "--system" ]; then
  dest="/usr/local/bin"
else
  dest="${COWORK_INSTALL_DIR:-$HOME/.local/bin}"
fi
if ! mkdir -p "$dest" 2>/dev/null || [ ! -w "$dest" ]; then
  echo "cannot write to $dest; run with sudo, or omit --system to install into ~/.local/bin" >&2
  exit 1
fi
install -m 755 "$here/cowork" "$dest/cowork"
install -m 755 "$here/room" "$dest/room"
if ! "$dest/cowork" --version >/dev/null 2>&1; then
  echo "installed, but $dest/cowork does not run here. It needs glibc 2.30 or newer (Ubuntu 20.04+)." >&2
  exit 1
fi
echo "installed $("$dest/cowork" --version) to $dest (plus the compatibility alias 'room')"
case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "note: $dest is not on your PATH. Add this line to ~/.bashrc or ~/.profile, then open a new shell:"
     echo "  export PATH=\"$dest:\$PATH\"" ;;
esac
echo "next: cd into a git repository and run: cowork init"
INSTALL
chmod 755 "$out/install.sh"

cat > "$out/README.txt" <<TXT
cowork $version for Linux x86_64 (built on $(. /etc/os-release 2>/dev/null && echo "$NAME $VERSION_ID" || echo linux), needs glibc 2.30+, so Ubuntu 20.04 or newer)

Contents
  cowork       the CLI
  room         the same program under its previous name (alias)
  install.sh   copies both into ~/.local/bin (or /usr/local/bin with --system)
  LICENSE      MIT
  SHA256SUMS   checksums of the binaries

Install
  tar xzf $name.tar.gz
  cd $name
  ./install.sh                # or: sudo ./install.sh --system
  cowork init                 # inside a git repository

Uninstall: remove cowork and room from the install directory. Per-user state lives in
~/.local/share/room/ (kept under the tool's original name on purpose).

Source and documentation: https://github.com/JuneLXD/cowork-cli
TXT
(cd "$out" && sha256sum cowork room > SHA256SUMS)
tar -C export -czf "export/$name.tar.gz" "$name"
echo "bundle: $out"
echo "tarball: export/$name.tar.gz ($(du -h "export/$name.tar.gz" | cut -f1))"
