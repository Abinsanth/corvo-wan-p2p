#!/bin/sh
# Installs corvo-wan-p2p (https://github.com/Abinsanth/corvo-wan-p2p) on macOS or Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.sh | sh
#
# Installs the latest release to ~/.local/bin, or to $CORVO_WAN_P2P_INSTALL_DIR
# if set. Set CORVO_WAN_P2P_VERSION (like v0.1.0-rc.1) for a particular
# release, pre-releases included. Never uses sudo.
set -eu

REPO="Abinsanth/corvo-wan-p2p"
APP="corvo-wan-p2p"

fail() {
    echo "$APP install: $*" >&2
    exit 1
}

need() {
    command -v "$1" > /dev/null 2>&1 || fail "needs '$1', which isn't installed"
}

sha256() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum > /dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        fail "needs 'sha256sum' or 'shasum' to verify the download"
    fi
}

# Everything runs from here, so a download cut short by `curl | sh` runs nothing.
main() {
    need curl
    need tar
    install_dir="${CORVO_WAN_P2P_INSTALL_DIR:-$HOME/.local/bin}"
    version="${CORVO_WAN_P2P_VERSION:-}"
    if [ -n "$version" ]; then
        release_url="https://github.com/$REPO/releases/download/$version"
    else
        release_url="https://github.com/$REPO/releases/latest/download"
    fi
    # Overridable only to test this script against a local copy of a release.
    base_url="${CORVO_WAN_P2P_BASE_URL:-$release_url}"

    case "$(uname -s)" in
        Darwin) os=apple-darwin ;;
        Linux) os=unknown-linux-musl ;;
        *) fail "no build for $(uname -s); build from source: https://github.com/$REPO" ;;
    esac
    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        arm64 | aarch64) arch=aarch64 ;;
        *) fail "no build for a $(uname -m) CPU; build from source: https://github.com/$REPO" ;;
    esac
    # A shell running under Rosetta reports x86_64 on an Apple Silicon Mac.
    if [ "$os" = apple-darwin ] && [ "$(sysctl -n hw.optional.arm64 2> /dev/null)" = 1 ]; then
        arch=aarch64
    fi

    asset="$APP-$arch-$os.tar.gz"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    echo "downloading $asset"
    # A progress bar when someone's watching; logs and pipes stay quiet.
    if [ -t 2 ]; then progress=--progress-bar; else progress=--silent; fi
    curl -fSL "$progress" "$base_url/$asset" -o "$tmp/$asset" || fail "couldn't download $base_url/$asset"
    curl -fsSL "$base_url/$asset.sha256" -o "$tmp/$asset.sha256" || fail "couldn't download its checksum"
    expected="$(cut -d ' ' -f 1 < "$tmp/$asset.sha256")"
    actual="$(sha256 "$tmp/$asset")"
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset; not installing"

    tar -xzf "$tmp/$asset" -C "$tmp"
    mkdir -p "$install_dir"
    # Copy then rename, so replacing a running corvo-wan-p2p is safe.
    cp "$tmp/$APP" "$install_dir/.$APP.new"
    chmod 755 "$install_dir/.$APP.new"
    mv -f "$install_dir/.$APP.new" "$install_dir/$APP"

    echo "installed $("$install_dir/$APP" --version) to $install_dir"
    case ":$PATH:" in
        *":$install_dir:"*) echo "run: $APP" ;;
        *)
            echo
            echo "$install_dir isn't on your PATH. Add this line to your shell profile"
            echo "(~/.zshrc or ~/.bashrc), then open a new terminal:"
            echo
            echo "    export PATH=\"$install_dir:\$PATH\""
            ;;
    esac
}

main "$@"
