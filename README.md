# corvo-wan-p2p

Peer-to-peer chat in your terminal, over the internet. Two people talk
app to app, end-to-end encrypted, with no server holding their messages. Share
your code with a friend, they add it, and you chat. Messages wait on your
machine until you're both online.

**Status:** early development. There's a test pre-release, `v0.1.0-rc.2`.

## Install

On macOS or Linux (x86_64 or ARM):

```bash
curl -fsSL https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.sh | CORVO_WAN_P2P_VERSION=v0.1.0-rc.2 sh
```

On Windows 10 or 11 (x86_64), in PowerShell:

```powershell
$env:CORVO_WAN_P2P_VERSION = 'v0.1.0-rc.2'; irm https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.ps1 | iex
```

The installer downloads the build for your system, checks its SHA-256 and
installs it without sudo or admin rights. On macOS and Linux it goes to
`~/.local/bin`, which it adds to your PATH in `~/.zshrc` (zsh) or
`~/.bash_profile` / `~/.bashrc` (bash), once, marked with a comment; set
`CORVO_WAN_P2P_NO_MODIFY_PATH=1` to leave your profile alone. On Windows it
goes to `%LOCALAPPDATA%\Programs\corvo-wan-p2p`, added to your user PATH. Set
`CORVO_WAN_P2P_INSTALL_DIR` to install somewhere else. `CORVO_WAN_P2P_VERSION`
picks a release; leave it out for the latest full release (there isn't one
yet, only the pre-release).

Then open a new terminal and run `corvo-wan-p2p`. Inside the app, `/update`
installs newer versions.

Or build from source with Rust ([rustup.rs](https://rustup.rs)):

```bash
git clone https://github.com/Abinsanth/corvo-wan-p2p.git
cd corvo-wan-p2p
cargo build --release
./target/release/corvo-wan-p2p
```

## Getting started

The first time you run `corvo-wan-p2p`, it asks whether to make a **new**
account or **recover** one:

1. Choose a display name and a passphrase. The passphrase encrypts your chats
   on this computer, and you type it each time you start the app.
2. You get **12 words**. Type two of them back to show you have them. They are
   your account: anyone with them can be you, and they are the only way back
   in if you forget your passphrase or change computers. They are also saved
   to `~/corvo-wan-p2p-recovery.txt`, readable only by you. Copy them
   somewhere safe (a password manager, paper), then delete that file. The app
   reminds you at each start until you do.
3. Your **code** (`corvo1…`, 64 characters) is shown. Send it to a friend any
   way you like. If one character of a code is mistyped, the app always
   notices. (Codes from the test versions before `0.1.0-rc.3` started with
   `corvo:` and no longer work; contacts you already have stay.)

To chat:

1. Your friend runs `/add <your code>` and says hi.
2. You see them under *Requests* in the sidebar. Run `/accept <name>` (or
   `/chat <name>` to read what they wrote, then `/accept`) to make them a
   contact.
3. Messages are delivered whenever you're both online. Until then they wait on
   the sender's computer and go out on their own when you reconnect.

A green dot next to a contact means they're online. Next to your own messages:

| Mark | Meaning |
|---|---|
| `·` | waiting: not delivered yet |
| `✓` | delivered to their computer |
| `✓✓` | read |

## Commands

| Command | What it does |
|---|---|
| `/code` | show your code and send it to your clipboard, if your terminal allows it |
| `/add <code>` | add a friend by their code and open a chat |
| `/chat <name>` | open a chat (Tab completes names) |
| `/contacts` | your contacts, who's online, last seen |
| `/requests` | people who messaged you but aren't contacts yet |
| `/accept [name]` | accept a request (the one in this tab if no name) |
| `/block [name]` | block a request or contact (this tab if no name) |
| `/unblock <name>` | unblock someone |
| `/blocked` | list blocked people |
| `/remove <name>` | delete a contact (asks whether to delete the history too) |
| `/info <name>` | their code, name, online / last seen |
| `/name <new name>` | change your display name |
| `/passphrase` | change your passphrase |
| `/phrase` | show your 12 words again (asks your passphrase first) |
| `/phrase save` | write them to `~/corvo-wan-p2p-recovery.txt` again |
| `/phrase copy` | send them to your clipboard, if your terminal allows it (not in Terminal.app) |
| `/close` | close this chat tab |
| `/clear` | clear the screen (Ctrl-L) |
| `/update` | check for a newer version and install it |
| `/help` | list commands |
| `/quit` | quit (Ctrl-C) |

Start a message with `//` to send text that begins with `/`.

Someone you block can't reach you, and you look offline to them. If you
unblock them and they write again, they come back as a request. A stranger's
request holds at most 20 messages until you accept it.

## One code per computer

Use your code on **one computer at a time**. If the same account runs on two
computers at once, messages go to one or the other unpredictably.

**New computer, or forgot your passphrase?** On a new computer, choose
*recover* at the first start and type your 12 words. On a computer that
already has the account, run `corvo-wan-p2p --recover`: after you type `yes`,
it deletes the account on that computer and brings it back from your words.
Either way you get the **same code**, but no contacts or history. Friends who write to you show up as
requests; accept them again.

## Privacy

- **Messages** are end-to-end encrypted between the two apps. Nobody in
  between can read them.
- **Your IP address:** when the two apps connect directly, each side sees the
  other's IP address.
- **Relays:** when a direct connection isn't possible (some home routers
  block it), the traffic goes through a relay run by
  [n0](https://n0.computer), the makers of [iroh](https://iroh.computer).
  Their relays and discovery service see which codes connect, when, and how
  much data, never the content. Using your own relay may come later.
- **Being online:** anyone who has your code can try to connect, so they can
  tell when you're online. Blocking someone makes you look offline to them.
- **On your computer:** your account, contacts and messages are encrypted
  with a key protected by your passphrase, in `~/.config/corvo-wan-p2p`
  (`%APPDATA%\corvo-wan-p2p` on Windows). Without the passphrase, someone
  with those files can't read names, codes or messages, but can see roughly
  how many people you know, how many messages each conversation has and how
  long they are, which ones are still waiting to be sent, and when the files
  last changed.
- **The log file**, `corvo-wan-p2p.log` in the same folder (readable only by
  you), notes connection trouble: which code (its first 8 characters) and
  what went wrong, never what was said or anyone's name. Warnings only; set
  `CORVO_WAN_P2P_LOG=debug` for more while looking into a problem (still
  only the app's own notes). Past 1 MB it starts over, keeping one old file.
- **The recovery file** holds your 12 words in plain text. Delete it once
  you've copied them. `/phrase copy` sends the words to your clipboard if your terminal allows it,
  and clipboard history apps may keep them there.

## Uninstall

Delete the program, then, to also forget your account and chats on this
computer, its data folder. Keep your 12 words if you want the account back
later.

```bash
rm ~/.local/bin/corvo-wan-p2p
rm -rf ~/.config/corvo-wan-p2p
```

Also delete the two lines the installer added to `~/.zshrc` (or your bash
profile), under `# added by the corvo-wan-p2p installer`.

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
