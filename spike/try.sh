#!/bin/sh
# Temporary M0 connection test: downloads iroh-spike for this machine and runs it.
#   curl -fsSL https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/main/spike/try.sh | sh
# Nothing is installed: it runs from a temporary folder that is deleted afterwards.
set -eu

base="https://github.com/Abinsanth/corvo-wan-p2p/releases/download/spike"

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-musl ;;
  *) echo "This test runs on macOS and Linux. On Windows, use try.ps1 (see the README)." >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) echo "Unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac

# A recognisable name, so a leftover folder is easy to find (see "Remove" in the README).
dir=$(mktemp -d "${TMPDIR:-/tmp}/iroh-spike.XXXXXX")
# HUP too: closing the terminal window must also clean up.
trap 'rm -rf "$dir"' EXIT HUP INT TERM
echo "Downloading the connection test for $arch-$os..."
curl -fsSL "$base/iroh-spike-$arch-$os.tar.gz" | tar -xzf - -C "$dir"

# Arguments skip the question, e.g. `sh try.sh dial <key>`.
if [ $# -gt 0 ]; then
  "$dir/iroh-spike" "$@"
  exit
fi

# Piped into sh, stdin is the script itself, so ask on the terminal.
printf 'Paste the key you were sent and press Enter.\nIf nobody sent you one, just press Enter to wait for them: '
read -r key < /dev/tty
if [ -n "$key" ]; then
  "$dir/iroh-spike" dial "$key"
else
  "$dir/iroh-spike" listen
fi
