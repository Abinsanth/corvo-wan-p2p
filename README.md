# corvo-wan-p2p

Peer-to-peer chat in your terminal, over the internet. Two people talk
app to app, end-to-end encrypted, with no server holding their messages. Share
your code with a friend, they add it, and you chat. Messages wait on your
machine until you're both online.

**Status:** `v0.1.1`.

## Install

On macOS or Linux (x86_64 or ARM):

```bash
curl -fsSL https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.sh | sh
```

On Windows 10 or 11 (x86_64), in PowerShell:

```powershell
irm https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.ps1 | iex
```

The installer downloads the build for your system, checks its SHA-256 and
installs it without sudo or admin rights. On macOS and Linux it goes to
`~/.local/bin`, which it adds to your PATH in `~/.zshrc` (zsh, in `$ZDOTDIR`
if you set one) or `~/.bash_profile` / `~/.bashrc` (bash), once, marked with
a comment. For fish and other shells it prints what to run instead. On
Windows it goes to `%LOCALAPPDATA%\Programs\corvo-wan-p2p`, added to your
user PATH. Set `CORVO_WAN_P2P_NO_MODIFY_PATH=1` to leave your profile (or
PATH) alone. Set
`CORVO_WAN_P2P_INSTALL_DIR` to install somewhere else. `CORVO_WAN_P2P_VERSION`
picks a release, like `v0.1.0`, pre-releases included; leave it out for the
latest full release.

Then open a new terminal and run `corvo-wan-p2p`. Inside the app, `/update`
installs newer versions, pre-releases only if you're on one (a copy built
from source updates with `git pull` and `cargo build` instead). It asks
first, then a window shows the steps: downloading, checking the download,
installing. If a step fails, or you quit partway, the old version stays.

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
   way you like. `/code` shows it again and offers to save it to
   `~/corvo-wan-p2p-code.txt`, which holds only the code, so it's easy to copy
   from there. If one character of a code is mistyped, the app always
   notices. (Codes from the test versions before `0.1.0-rc.3` started with
   `corvo:` and no longer work; contacts you already have stay.)

To chat:

1. Your friend runs `/add <your code>` and says hi.
2. You see them under *Requests* in the sidebar. Run `/accept <name>` (or
   `/chat <name>` to read what they wrote, then `/accept`) to make them a
   contact.
3. Messages are delivered whenever you're both online. Until then they wait on
   the sender's computer and go out on their own when you reconnect.

In a chat, their messages are on the left (`14:02 hey`) and yours on the
right (`see you soon 14:03 ✓✓`); the tab says who it is. A green dot next to
a contact means they're online. After your own messages:

| Mark | Meaning |
|---|---|
| `·` | waiting: not delivered yet |
| `✓` | delivered to their computer |
| `✓✓` | read |

Each start opens with "corvo" in ASCII art and the version (any key closes
it; it goes by itself after 2 seconds). The first start after making or
recovering an account adds a few tips.

The bar at the bottom shows your name and code, whether you're online, how
the open chat's connection goes (**direct** between the two computers, or
**via relay**, and how long a round trip takes), how many contacts are
online, and how many of your messages are still waiting.

## Commands

| Command | What it does |
|---|---|
| `/profile` | your name and code, and a menu to change your name or passphrase, see your 12 words or delete your account |
| `/code` | show your code in a box, and offer to save it to `~/corvo-wan-p2p-code.txt` |
| `/add <code>` | add a friend by their code and open a chat |
| `/contacts` | a table of your contacts A–Z (online or last seen), then your requests |
| `/requests` | a table of people who messaged you but aren't contacts yet |
| `/blocked` | a table of the people you've blocked |
| `/accept [name]` | accept a request (the one in this tab if no name) |
| `/block [name]` | block a request or contact (this tab if no name) |
| `/unblock <name>` | unblock someone |
| `/remove <name>` | delete a contact (asks whether to delete the history too) |
| `/info <name>` | their code, name, online / last seen |
| `/chat <name>` | open a chat (Tab completes names) |
| `/close` | close this chat tab |
| `/clear` | clear the screen (Ctrl-L) |
| `/update` | check for a newer version and install it |
| `/about` | the version, where your data lives, the source and the license |
| `/help` | a short list of the commands and keys |
| `/quit` | quit (Ctrl-C) |

A command typed without what it needs, or with the wrong words, shows how
it's typed, what it does and an example. Start a message with `//` to send
text that begins with `/`.

Someone you block can't reach you, and you look offline to them. If you
unblock them and they write again, they come back as a request. A stranger's
request holds at most 20 messages until you accept it.

## Your profile

`/profile` opens a **Profile** tab right after the corvo tab. It shows your
name and code, and a menu:

```
› Change name
  Change passphrase
  Show 12 words
  Delete account
```

↑/↓ choose and Enter opens the item in a window:

- **Change name:** your name now, and a field for the new one. Enter saves it.
- **Change passphrase:** the current one, the new one and the new one again,
  all hidden.
- **Show 12 words:** asks your passphrase, then shows the words. `S` saves
  them to `~/corvo-wan-p2p-recovery.txt`, or says where that file already is.
  Closing the window wipes the words from memory.
- **Delete account** (in red): see below.

Esc closes a window, and Esc again closes the tab. Moving to another tab
closes it too. What you type in these windows never goes into the input
history.

### Delete account

Removes your account from this computer completely. The window says what goes,
then asks your passphrase, then asks you to type `delete`, then asks
"Uninstall the app too? y/n". Esc stops it at any step.

It deletes everything the app keeps: the data folder (keys, messages,
contacts, logs), the recovery file and the code file. Your friends keep their
copies of your messages, and your 12 words still bring back the same code
(with no history) if you kept them somewhere else.

With `y` it also deletes the program. On Windows, or for a copy you built from
source, it prints what to delete instead. The app then quits and says what was
done, or what's left and where if something couldn't be deleted. The PATH line
the installer added stays; remove it yourself if you like (see
[Uninstall](#uninstall)).

## One code per computer

Use your code on **one computer at a time**. If the same account runs on two
computers at once, messages go to one or the other unpredictably.

**New computer, or forgot your passphrase?** On a new computer, choose
*recover* at the first start and type your 12 words. On a computer that
already has the account, run `corvo-wan-p2p --recover`: after you type `yes`,
it deletes the account (and its log) on that computer and brings it back from your words.
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
  you've copied them. The app never puts anything on your clipboard.
- **The code file**, `~/corvo-wan-p2p-code.txt`, is only there if you let
  `/code` save it. It holds just your code, which isn't secret.

## Uninstall

The easy way: `/profile`, **Delete account**, and answer `y` to "Uninstall
the app too?". That deletes your account, its files and the program (see
[Delete account](#delete-account)). Then delete the PATH line, below.

By hand: delete the program, then, to also forget your account and chats on
this computer, its data folder and the code and recovery files in your home
folder, if they're there. Keep your 12 words if you want the account back later.

```bash
rm ~/.local/bin/corvo-wan-p2p
rm -rf ~/.config/corvo-wan-p2p
rm -f ~/corvo-wan-p2p-code.txt ~/corvo-wan-p2p-recovery.txt
```

Also delete the two lines the installer added to `~/.zshrc` (or your bash
profile), under `# added by the corvo-wan-p2p installer`.

On Windows, delete `%LOCALAPPDATA%\Programs\corvo-wan-p2p`,
`%APPDATA%\corvo-wan-p2p` and the two files in `%USERPROFILE%`.

## Development

```bash
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
