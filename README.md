# corvo-wan-p2p

Peer-to-peer chat in your terminal, over the internet. Two people talk
directly, end-to-end encrypted, with no server in between. Share your code with
a friend, they add it, and you chat. Messages wait on your machine until you're
both online.

**Status:** early development. Nothing to install yet.

## Development

```bash
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
