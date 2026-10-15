# corvo-wan-p2p

Peer-to-peer chat in your terminal, over the internet. Two people talk
directly, end-to-end encrypted, with no server in between. Share your code with
a friend, they add it, and you chat. Messages wait on your machine until you're
both online.

**Status:** early development. There's a test pre-release, `v0.1.0-rc.1`.

## Install

On macOS or Linux (x86_64 or ARM):

```bash
curl -fsSL https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.sh | CORVO_WAN_P2P_VERSION=v0.1.0-rc.1 sh
```

On Windows 10 or 11 (x86_64), in PowerShell:

```powershell
$env:CORVO_WAN_P2P_VERSION = 'v0.1.0-rc.1'; irm https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.ps1 | iex
```

The installer downloads the build for your system, checks its SHA-256 and
installs it without sudo or admin rights. On macOS and Linux it goes to
`~/.local/bin`; on Windows to `%LOCALAPPDATA%\Programs\corvo-wan-p2p`, which
is added to your PATH (open a new terminal afterwards). Set
`CORVO_WAN_P2P_INSTALL_DIR` to install somewhere else. `CORVO_WAN_P2P_VERSION`
picks a release; leave it out for the latest full release (there isn't one
yet, only the pre-release). Run the same command again to update.

Then run `corvo-wan-p2p`. If the shell says the command isn't found, the
install folder isn't on your PATH yet: the installer printed the line to add.

Or build from source with Rust ([rustup.rs](https://rustup.rs)):

```bash
git clone https://github.com/Abinsanth/corvo-wan-p2p.git
cd corvo-wan-p2p
cargo build --release
./target/release/corvo-wan-p2p
```

### Uninstall

Delete the program, then, to also forget your account and chats on this
computer, its data folder. Keep your 12 words if you want the account back
later.

```bash
rm ~/.local/bin/corvo-wan-p2p
rm -rf ~/.config/corvo-wan-p2p
```

On Windows, delete `%LOCALAPPDATA%\Programs\corvo-wan-p2p` and
`%APPDATA%\corvo-wan-p2p`.

## Development

```bash
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
