#!/bin/sh
# Installs corvo-wan-p2p (https://github.com/Abinsanth/corvo-wan-p2p) on macOS or Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.sh | sh
#
# Installs the latest release to ~/.local/bin, or to $CORVO_WAN_P2P_INSTALL_DIR
# if set, and adds that folder to PATH in your shell's profile (~/.zshrc,
# ~/.bash_profile or ~/.bashrc) unless CORVO_WAN_P2P_NO_MODIFY_PATH is set.
# Set CORVO_WAN_P2P_VERSION (like v0.1.0-rc.1) for a particular release,
# pre-releases included. Never uses sudo.
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
    if [ -z "${CORVO_WAN_P2P_INSTALL_DIR:-}" ] && [ -z "${HOME:-}" ]; then
        fail "HOME isn't set: set it, or CORVO_WAN_P2P_INSTALL_DIR to the folder to install to"
    fi
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
    add_to_path "$install_dir"
}

# The profile file your shell reads at start, or nothing for shells this
# script doesn't edit (fish and others).
profile_file() {
    case "$(basename "${SHELL:-}")" in
        zsh)
            # ZDOTDIR is often set in ~/.zshenv, which only zsh reads: ask it.
            zdotdir="$(zsh -c 'printf %s "${ZDOTDIR:-$HOME}"' 2> /dev/null)" || zdotdir=""
            echo "${zdotdir:-${ZDOTDIR:-$HOME}}/.zshrc"
            ;;
        bash)
            if [ "$(uname -s)" != Darwin ]; then
                echo "$HOME/.bashrc"
                return
            fi
            # macOS terminals start login shells, which read only the first of
            # these that exists; making a new .bash_profile would hide the others.
            for f in .bash_profile .bash_login .profile; do
                if [ -f "$HOME/$f" ]; then
                    echo "$HOME/$f"
                    return
                fi
            done
            echo "$HOME/.bash_profile"
            ;;
    esac
}

# Makes `corvo-wan-p2p` work in new terminals: adds the folder to PATH in the
# shell's profile, once. Skipped with CORVO_WAN_P2P_NO_MODIFY_PATH=1. Says
# exactly what to do by hand when it can't.
add_to_path() {
    dir="$1"
    case ":$PATH:" in
        *":$dir:"*)
            echo "run: $APP"
            return
            ;;
    esac
    # In single quotes, so the folder stays as it is whatever is in its name.
    line="export PATH='$(printf '%s' "$dir" | sed "s/'/'\\\\''/g")':\"\$PATH\""
    # What installers before 0.1.0 wrote.
    old_line="export PATH=\"$dir:\$PATH\""
    shell="$(basename "${SHELL:-}")"
    profile=""
    if [ -n "${HOME:-}" ]; then
        profile="$(profile_file)"
    fi
    if [ -n "$profile" ] && [ -f "$profile" ] && { grep -qxF "$line" "$profile" || grep -qxF "$old_line" "$profile"; }; then
        echo "$dir is already in $profile: open a new terminal, then run: $APP"
        return
    fi
    # A profile that can't be written gets the hint below, not the shell's error.
    if [ -z "${CORVO_WAN_P2P_NO_MODIFY_PATH:-}" ] && [ -n "$profile" ] &&
        { printf '\n# added by the %s installer\n%s\n' "$APP" "$line" >> "$profile"; } 2> /dev/null; then
        echo
        echo "added $dir to your PATH in $profile."
        echo "Open a new terminal (or run: . \"$profile\"), then run: $APP"
        return
    fi
    echo
    echo "$dir isn't on your PATH."
    if [ "$shell" = fish ]; then
        echo "Run this once in fish, then open a new terminal:"
        echo
        echo "    fish_add_path '$(printf '%s' "$dir" | sed "s/[\\\\']/\\\\&/g")'"
    elif [ -n "$profile" ]; then
        echo "Add this line to $profile, then open a new terminal:"
        echo
        echo "    $line"
    else
        echo "Add this line to your shell's profile, then open a new terminal:"
        echo
        echo "    $line"
    fi
}

main "$@"
