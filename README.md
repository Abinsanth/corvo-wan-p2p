# corvo-wan-p2p

Peer-to-peer chat in your terminal, over the internet. Two people talk
directly, end-to-end encrypted, with no server in between. Share your code with
a friend, they add it, and you chat. Messages wait on your machine until you're
both online.

**Status:** early development. Nothing to install yet.

## Connection test (temporary)

A quick check that two computers can reach each other over the internet. It
downloads a small test program to a temporary folder, runs it, and deletes it.
Nothing is installed.

**macOS or Linux:** open Terminal and paste:

```bash
curl -fsSL https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/main/spike/try.sh | sh
```

**Windows:** open PowerShell and paste:

```powershell
irm https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/main/spike/try.ps1 | iex
```

It asks for a key:
- **If someone sent you a key,** paste it and press Enter. You should see
  `reply: pong`.
- **If not,** press Enter. It shows a key: send that key to the other person
  and leave the window open until they've connected (Ctrl-C to stop).

## Development

```bash
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
