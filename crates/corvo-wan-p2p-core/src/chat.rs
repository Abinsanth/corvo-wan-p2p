//! The chat engine: one tokio task that owns the store and the iroh endpoint.
//!
//! The UI talks to it through a [`Client`]: requests go in as commands, and
//! everything that happens comes back as [`Event`]s. While running it keeps
//! trying to reach every contact (backing off while they're offline), sends
//! waiting messages as soon as a connection opens, and confirms what arrives.
//!
//! Anyone with your code can connect. Someone who isn't a contact is a
//! **guest**: their first message makes them a **request**, which is saved and
//! acked but gets no read receipts, no name and no dialing back until you
//! accept it. Guests and requests are limited, and so is everything a peer can
//! make this app hold in memory.

mod link;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh::endpoint::{Connection, RelayStatus};
use iroh::{Endpoint, Watcher};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::VERSION;
use crate::identity::{Code, NameError, Seed, clean_name};
use crate::net::ALPN;
use crate::proto::{Frame, MAX_FRAME, MAX_TEXT, PROTO_VERSION, encode};
use crate::store::{KdfParams, Passphrase, Person, Standing, Store, StoreError, StoredMessage};
use link::{Role, Verdict};

/// The most requests waiting for an answer. Strangers are refused beyond it.
pub const MAX_REQUESTS: usize = 20;
/// The most messages saved from one request. Later ones aren't acked, so they
/// wait on the sender's side.
pub const MAX_REQUEST_MESSAGES: usize = 20;
/// The most connections open at once from people who aren't contacts.
pub const MAX_GUESTS: usize = 20;
/// How many times a request may change its name before you answer it. Later
/// changes are ignored, so a stranger can't flood the screen with renames.
pub const MAX_REQUEST_RENAMES: usize = 5;
/// The most messages sent to one peer and not yet acked; the rest wait their turn.
pub const WINDOW: usize = 32;
/// Frames waiting to be written to one peer. A peer that lets this fill isn't
/// reading what we send, and is cut off. In bytes it's bounded too: at most
/// [`WINDOW`] of them are messages (each under [`MAX_FRAME`]), and the rest
/// are small (acks, read receipts, names), so about 2 MiB per peer at worst.
pub(crate) const OUT_QUEUE: usize = 128;
/// Frames and news from all connections waiting for the core.
const INTERNAL_QUEUE: usize = 64;

/// Why a connection was closed, as its QUIC close code. Only `DUPLICATE` means
/// anything to the other app.
mod close {
    pub const BYE: u32 = 0;
    pub const HANDSHAKE: u32 = 1;
    pub const REFUSED: u32 = 2;
    pub const ERROR: u32 = 3;
    pub const BAD_HELLO: u32 = 4;
    pub const VERSION: u32 = 5;
    /// The connection dropped when both sides dialed at once.
    pub const DUPLICATE: u32 = 6;
    /// They weren't reading what we sent.
    pub const FLOOD: u32 = 7;
    /// A guest that said nothing for too long.
    pub const IDLE: u32 = 8;
}

/// Timings and passphrase settings. [`Config::DEFAULT`] is what the app uses;
/// tests use shorter and cheaper ones.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// How long one attempt to reach someone may take.
    pub dial_timeout: Duration,
    /// How long the hello exchange may take once connected.
    pub handshake_timeout: Duration,
    /// The first wait before trying again; it doubles after each failure…
    pub retry_first: Duration,
    /// …up to this.
    pub retry_max: Duration,
    /// How long a link must have lasted for its end to count as going offline
    /// rather than as a failed attempt (which backs off further).
    pub stable_link: Duration,
    /// How long a guest (someone who isn't a contact) may stay connected
    /// without sending anything.
    pub guest_idle: Duration,
    /// How long, at start, the relay gets to answer before the network is
    /// said to be offline.
    pub connecting: Duration,
    /// How a new passphrase is stretched (`/passphrase`).
    pub kdf: KdfParams,
}

impl Config {
    pub const DEFAULT: Config = Config {
        dial_timeout: Duration::from_secs(30),
        handshake_timeout: Duration::from_secs(10),
        retry_first: Duration::from_secs(5),
        retry_max: Duration::from_secs(300),
        stable_link: Duration::from_secs(30),
        guest_idle: Duration::from_secs(60),
        connecting: Duration::from_secs(10),
        kdf: KdfParams::DEFAULT,
    };
}

/// Something that happened, for the UI to show. Your own commands answer
/// through their results instead, except that accepting someone who is
/// connected right now reports them [`Event::Online`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A connection to a contact is open. Only ever about contacts.
    Online(Code),
    /// The connection to a contact ended.
    Offline(Code),
    /// Someone who isn't a contact sent their first message (it follows as
    /// [`Event::Received`]). `name` is what their app sent, maybe empty.
    Request { from: Code, name: String },
    /// A new message from a contact or a request.
    Received { from: Code, id: String, sent_at: i64, text: String },
    /// Their app saved your message `id` (`✓`).
    Delivered { to: Code, id: String },
    /// They read your messages up to `up_to` (`✓✓`).
    Read { by: Code, up_to: String },
    /// The display name of a contact or a request, as their app told us.
    Name { of: Code, name: String },
    /// A contact's app speaks a different protocol version, so nothing can be
    /// exchanged until one of you updates. Messages keep waiting.
    VersionMismatch { peer: Code, theirs: u32 },
    /// Whether this app can reach the internet (it's connected to a relay).
    /// Sent once at start and again whenever it changes.
    Network { online: bool },
    /// Something went wrong that the user should hear about.
    Problem(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Name(#[from] NameError),
    #[error("that person isn't one of your contacts")]
    NotAContact,
    #[error("they haven't sent you a request")]
    NotARequest,
    #[error("you've blocked them; unblock them first")]
    Blocked,
    #[error("they aren't blocked")]
    NotBlocked,
    #[error("you can't add yourself")]
    Yourself,
    #[error("a message can't be empty")]
    Empty,
    #[error("that message is too long; the most is 16 KB")]
    TooLong,
    #[error("wrong passphrase")]
    WrongPassphrase,
    #[error("the chat engine has stopped")]
    Stopped,
    #[error("the network key doesn't belong to this account")]
    WrongKey,
}

type Reply<T> = oneshot::Sender<Result<T, ChatError>>;

enum Command {
    AddContact(Code, Reply<()>),
    Accept(Code, Reply<()>),
    Block(Code, Reply<()>),
    Unblock(Code, Reply<()>),
    Remove { who: Code, history: bool, reply: Reply<()> },
    Send(Code, String, Reply<String>),
    MarkRead(Code, Reply<()>),
    SetName(String, Reply<String>),
    People(Reply<(Vec<Person>, usize)>),
    Contacts(Reply<Vec<Person>>),
    History(Code, Reply<(Vec<StoredMessage>, usize)>),
    ReadUpto(Code, Reply<Option<String>>),
    ChangePassphrase { old: Zeroizing<String>, new: Zeroizing<String>, reply: Reply<()> },
    Phrase(Zeroizing<String>, Reply<Seed>),
    Shutdown,
}

/// What the link tasks and the accept loop tell the core.
enum Internal {
    Incoming(Connection),
    /// Hello exchanged. The link reads nothing more until `verdict` says yes.
    Ready {
        peer: Code,
        id: u64,
        conn: Connection,
        role: Role,
        hello: Frame,
        out: mpsc::Sender<Frame>,
        verdict: oneshot::Sender<Verdict>,
    },
    Frame {
        peer: Code,
        frame: Frame,
    },
    Closed {
        peer: Code,
        id: u64,
        duplicate: bool,
    },
    /// A dial got through. Before saying hello: is `peer` still a contact?
    /// (They may have been blocked or removed while it was under way.)
    Connected {
        peer: Code,
        still: oneshot::Sender<bool>,
    },
    /// Dialing, or the hello exchange, didn't work out.
    Failed {
        peer: Code,
        id: u64,
        role: Role,
    },
    /// We dialed, and their app closed the connection for speaking another
    /// protocol version: `theirs`.
    VersionRefused {
        peer: Code,
        id: u64,
        theirs: u32,
    },
    /// Whether the endpoint is connected to a relay now.
    Network(bool),
}

/// The UI's handle on the chat engine.
pub struct Client {
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::UnboundedReceiver<Event>,
    me: Code,
    task: JoinHandle<()>,
}

impl Client {
    /// Starts the engine. `endpoint` must use the secret key from `store`'s seed
    /// and accept [`ALPN`] (see `net::internet_endpoint`).
    pub fn start(store: Store, endpoint: Endpoint, config: Config) -> Result<Client, ChatError> {
        let profile = store.profile()?;
        let me = profile.seed.code();
        if endpoint.id() != me.key() {
            return Err(ChatError::WrongKey);
        }
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::unbounded_channel();
        let (internal, internal_rx) = mpsc::channel(INTERNAL_QUEUE);
        let core = Core {
            store,
            endpoint,
            config,
            me,
            name: profile.name,
            events: events_tx,
            internal,
            links: HashMap::new(),
            guests: HashSet::new(),
            quiet: HashSet::new(),
            dialing: HashSet::new(),
            retry: HashMap::new(),
            next_link: 0,
            online: None,
            renames: HashMap::new(),
            session: rand::random(),
            mismatched: HashMap::new(),
        };
        let task = tokio::spawn(core.run(commands_rx, internal_rx));
        Ok(Client { commands, events, me, task })
    }

    /// Your own code.
    pub fn me(&self) -> Code {
        self.me
    }

    /// The next thing that happened. `None` once the engine has stopped.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// Adds someone by their code and starts dialing them. If they had sent
    /// you a request, this accepts it.
    pub async fn add_contact(&self, code: Code) -> Result<(), ChatError> {
        self.ask(|r| Command::AddContact(code, r)).await
    }

    /// Makes a request a contact: from now on they're dialed, get read
    /// receipts and see your name.
    pub async fn accept(&self, code: Code) -> Result<(), ChatError> {
        self.ask(|r| Command::Accept(code, r)).await
    }

    /// Blocks anyone, known or not. Their connections are closed before hello,
    /// so you look offline to them, and anything queued to them is dropped.
    pub async fn block(&self, code: Code) -> Result<(), ChatError> {
        self.ask(|r| Command::Block(code, r)).await
    }

    /// Unblocks someone. They're a stranger again: if they message you, they
    /// come back as a request.
    pub async fn unblock(&self, code: Code) -> Result<(), ChatError> {
        self.ask(|r| Command::Unblock(code, r)).await
    }

    /// Forgets a contact or a request and stops dialing them; anything queued
    /// to them is dropped. With `history`, your messages with them are deleted
    /// too. If they message again, they come back as a request.
    pub async fn remove(&self, code: Code, history: bool) -> Result<(), ChatError> {
        self.ask(|reply| Command::Remove { who: code, history, reply }).await
    }

    /// Saves a message and sends it as soon as `to` is reachable. Returns its id.
    pub async fn send(&self, to: Code, text: &str) -> Result<String, ChatError> {
        self.ask(|r| Command::Send(to, text.to_string(), r)).await
    }

    /// You've seen everything `peer` sent so far: tells them (now, or once
    /// you're connected). Requests are never told.
    pub async fn mark_read(&self, peer: Code) -> Result<(), ChatError> {
        self.ask(|r| Command::MarkRead(peer, r)).await
    }

    /// Changes your display name and tells every contact connected. Returns it as saved.
    pub async fn set_name(&self, name: &str) -> Result<String, ChatError> {
        self.ask(|r| Command::SetName(name.to_string(), r)).await
    }

    /// Everyone you know of: contacts, requests and blocked people.
    pub async fn people(&self) -> Result<Vec<Person>, ChatError> {
        Ok(self.people_checked().await?.0)
    }

    /// [`Client::people`], and how many people couldn't be read (a damaged store).
    pub async fn people_checked(&self) -> Result<(Vec<Person>, usize), ChatError> {
        self.ask(Command::People).await
    }

    pub async fn contacts(&self) -> Result<Vec<Person>, ChatError> {
        self.ask(Command::Contacts).await
    }

    pub async fn history(&self, peer: Code) -> Result<Vec<StoredMessage>, ChatError> {
        Ok(self.history_checked(peer).await?.0)
    }

    /// [`Client::history`], and how many messages couldn't be read (a damaged store).
    pub async fn history_checked(&self, peer: Code) -> Result<(Vec<StoredMessage>, usize), ChatError> {
        self.ask(|r| Command::History(peer, r)).await
    }

    /// The newest message of `peer`'s that you've read, if any: everything
    /// they sent after it is unread.
    pub async fn read_upto(&self, peer: Code) -> Result<Option<String>, ChatError> {
        self.ask(|r| Command::ReadUpto(peer, r)).await
    }

    /// Locks your history with `new` instead, if `old` is your passphrase now.
    pub async fn change_passphrase(&self, old: &str, new: &str) -> Result<(), ChatError> {
        let (old, new) = (Zeroizing::new(old.to_string()), Zeroizing::new(new.to_string()));
        self.ask(|reply| Command::ChangePassphrase { old, new, reply }).await
    }

    /// Your seed (and so your 12 words), if `passphrase` is yours.
    pub async fn phrase(&self, passphrase: &str) -> Result<Seed, ChatError> {
        let passphrase = Zeroizing::new(passphrase.to_string());
        self.ask(|r| Command::Phrase(passphrase, r)).await
    }

    /// Closes every connection and stops. The store is released when this returns.
    pub async fn shutdown(self) {
        let _ = self.commands.send(Command::Shutdown);
        let _ = self.task.await;
    }

    async fn ask<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, ChatError> {
        let (reply, answer) = oneshot::channel();
        self.commands.send(command(reply)).map_err(|_| ChatError::Stopped)?;
        answer.await.map_err(|_| ChatError::Stopped)?
    }
}

struct Link {
    id: u64,
    out: mpsc::Sender<Frame>,
    conn: Connection,
    /// Who opened this connection.
    initiator: Code,
    /// Our messages sent on this connection and not acked yet (at most [`WINDOW`]).
    in_flight: HashSet<String>,
    /// The name a guest's app sent, kept in case they become a request.
    heard_name: Option<String>,
    /// Their app's run, from its hello (0 from apps before sessions).
    session: u64,
    /// When the link came up, and when they last sent anything.
    opened: Instant,
    heard: Instant,
}

impl Link {
    /// Queues `frame` to be written. A full queue means they've stopped
    /// reading, so the connection is cut. `true` if it was queued.
    fn queue(&self, frame: Frame) -> bool {
        match self.out.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.conn.close(close::FLOOD.into(), b"not reading");
                false
            }
            Err(TrySendError::Closed(_)) => false,
        }
    }
}

struct Retry {
    at: Instant,
    /// Attempts that failed since the last link that lasted.
    failures: u32,
}

struct Core {
    store: Store,
    endpoint: Endpoint,
    config: Config,
    me: Code,
    name: String,
    events: mpsc::UnboundedSender<Event>,
    internal: mpsc::Sender<Internal>,
    links: HashMap<Code, Link>,
    /// Ids of connections from people who aren't contacts, from accepting them
    /// until they close: at most [`MAX_GUESTS`].
    guests: HashSet<u64>,
    /// Peers whose connection was dropped as a duplicate while the other one
    /// was still starting: not reported offline unless reaching them fails.
    quiet: HashSet<Code>,
    dialing: HashSet<Code>,
    retry: HashMap<Code, Retry>,
    next_link: u64,
    /// Whether the endpoint was connected to a relay, as last reported.
    online: Option<bool>,
    /// Name changes taken from each request, until it's answered. Kept per
    /// person, not per connection, so reconnecting doesn't earn more.
    renames: HashMap<Code, usize>,
    /// This run of the app, sent in every hello.
    session: u64,
    /// Contacts on another protocol version, and theirs, as last reported:
    /// each is said once, not on every attempt.
    mismatched: HashMap<Code, u32>,
}

impl Core {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>, mut internal: mpsc::Receiver<Internal>) {
        let accept = tokio::spawn(accept_loop(self.endpoint.clone(), self.internal.clone()));
        let watcher = self.endpoint.home_relay_status();
        let network = tokio::spawn(watch_network(watcher, self.internal.clone(), self.config.connecting));
        match self.store.contacts() {
            Ok(contacts) => {
                for contact in contacts {
                    self.retry_in(contact.code, Duration::ZERO);
                }
            }
            Err(e) => self.problem(e),
        }
        let mut sweep = tokio::time::interval((self.config.guest_idle / 4).max(Duration::from_millis(50)));
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let due = self.next_dial();
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Shutdown) | None => break,
                    Some(command) => self.command(command),
                },
                Some(message) = internal.recv() => self.internal(message),
                _ = sleep_until(due) => self.dial_due(),
                _ = sweep.tick() => self.sweep_guests(),
            }
        }
        accept.abort();
        network.abort();
        for link in self.links.values() {
            link.conn.close(close::BYE.into(), b"bye");
        }
        self.endpoint.close().await;
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::AddContact(code, reply) => {
                let _ = reply.send(self.add_contact(code));
            }
            Command::Accept(code, reply) => {
                let _ = reply.send(self.accept(code));
            }
            Command::Block(code, reply) => {
                let _ = reply.send(self.block(code));
            }
            Command::Unblock(code, reply) => {
                let _ = reply.send(self.unblock(code));
            }
            Command::Remove { who, history, reply } => {
                let _ = reply.send(self.remove(who, history));
            }
            Command::Send(to, text, reply) => {
                let _ = reply.send(self.send(to, text));
            }
            Command::MarkRead(peer, reply) => {
                let _ = reply.send(self.mark_read(peer));
            }
            Command::SetName(name, reply) => {
                let _ = reply.send(self.set_name(&name));
            }
            Command::People(reply) => {
                let _ = reply.send(self.store.people_checked().map_err(ChatError::from));
            }
            Command::Contacts(reply) => {
                let _ = reply.send(self.store.contacts().map_err(ChatError::from));
            }
            Command::History(peer, reply) => {
                let _ = reply.send(self.store.history_checked(&peer).map_err(ChatError::from));
            }
            Command::ReadUpto(peer, reply) => {
                let _ = reply.send(self.store.read_upto(&peer).map_err(ChatError::from));
            }
            // Argon2 takes a fifth of a second to a second: done on a blocking
            // thread, so messages keep flowing meanwhile.
            Command::ChangePassphrase { old, new, reply } => {
                let (passphrase, kdf) = (self.store.passphrase(), self.config.kdf);
                tokio::task::spawn_blocking(move || {
                    let _ = reply.send(change_passphrase(&passphrase, &old, &new, kdf));
                });
            }
            Command::Phrase(typed, reply) => {
                let passphrase = self.store.passphrase();
                let seed = self.store.profile().map(|p| p.seed);
                tokio::task::spawn_blocking(move || {
                    let _ = reply.send(check_passphrase(&passphrase, &typed).and_then(|()| Ok(seed?)));
                });
            }
            Command::Shutdown => {}
        }
    }

    fn add_contact(&mut self, code: Code) -> Result<(), ChatError> {
        if code == self.me {
            return Err(ChatError::Yourself);
        }
        match self.store.standing(&code)? {
            Some(Standing::Contact) => Ok(()),
            Some(Standing::Request) => self.accept(code),
            Some(Standing::Blocked) => Err(ChatError::Blocked),
            None => {
                self.store.add_contact(&code)?;
                self.welcome(code)?;
                Ok(())
            }
        }
    }

    fn accept(&mut self, code: Code) -> Result<(), ChatError> {
        if !self.store.accept(&code)? {
            return Err(ChatError::NotARequest);
        }
        self.welcome(code)?;
        Ok(())
    }

    /// `peer` just became a contact. If they're connected (as a guest), that
    /// connection becomes a contact's; otherwise they're dialed now.
    fn welcome(&mut self, peer: Code) -> Result<(), StoreError> {
        self.renames.remove(&peer);
        let Some(link) = self.links.get(&peer) else {
            self.retry_in(peer, Duration::ZERO);
            return Ok(());
        };
        self.guests.remove(&link.id);
        self.emit(Event::Online(peer));
        self.catch_up(&peer)
    }

    fn block(&mut self, code: Code) -> Result<(), ChatError> {
        if code == self.me {
            return Err(ChatError::Yourself);
        }
        self.store.block(&code)?;
        self.drop_link(&code, close::REFUSED);
        Ok(())
    }

    fn unblock(&mut self, code: Code) -> Result<(), ChatError> {
        if self.store.unblock(&code)? { Ok(()) } else { Err(ChatError::NotBlocked) }
    }

    fn remove(&mut self, code: Code, history: bool) -> Result<(), ChatError> {
        match self.store.standing(&code)? {
            Some(Standing::Blocked) => return Err(ChatError::Blocked),
            None => return Err(ChatError::NotAContact),
            Some(Standing::Contact | Standing::Request) => {}
        }
        self.store.remove(&code, history)?;
        // If their app still has you, it reconnects and they're a guest.
        self.drop_link(&code, close::BYE);
        Ok(())
    }

    /// Closes the connection to `peer`, if any, and stops dialing them.
    fn drop_link(&mut self, peer: &Code, code: u32) {
        self.renames.remove(peer);
        if let Some(link) = self.links.remove(peer) {
            link.conn.close(code.into(), b"");
            self.guests.remove(&link.id);
        }
        self.retry.remove(peer);
        self.quiet.remove(peer);
    }

    fn send(&mut self, to: Code, text: String) -> Result<String, ChatError> {
        if text.trim().is_empty() {
            return Err(ChatError::Empty);
        }
        if !self.store.is_contact(&to)? {
            return Err(ChatError::NotAContact);
        }
        let id = ulid::Ulid::generate().to_string();
        let frame = Frame::Msg { id: id.clone(), sent_at: now_ms(), text };
        // Escaping can make the JSON much longer than the text, so check the frame itself.
        let Frame::Msg { sent_at, text, .. } = &frame else { unreachable!() };
        if text.len() > MAX_TEXT || encode(&frame).len() > MAX_FRAME {
            return Err(ChatError::TooLong);
        }
        self.store.add_outgoing(&to, &id, *sent_at, text)?;
        self.pump(&to)?;
        Ok(id)
    }

    fn mark_read(&mut self, peer: Code) -> Result<(), ChatError> {
        let Some(standing @ (Standing::Contact | Standing::Request)) = self.store.standing(&peer)? else {
            return Ok(());
        };
        let Some(up_to) = self.store.last_received(&peer)? else { return Ok(()) };
        self.store.note_read(&peer, &up_to)?;
        // Remembered either way, but requests never get read receipts.
        if standing == Standing::Contact {
            self.send_frame(&peer, Frame::Read { up_to });
        }
        Ok(())
    }

    fn set_name(&mut self, name: &str) -> Result<String, ChatError> {
        let name = self.store.set_name(name)?;
        self.name = name.clone();
        for link in self.links.values().filter(|l| !self.guests.contains(&l.id)) {
            link.queue(Frame::Name { name: name.clone() });
        }
        Ok(name)
    }

    /// Queues `frame` for `to` if connected. `true` if it was queued.
    fn send_frame(&self, to: &Code, frame: Frame) -> bool {
        self.links.get(to).is_some_and(|link| link.queue(frame))
    }

    /// Sends `to` the oldest of your waiting messages, keeping at most
    /// [`WINDOW`] unacked. Called when they connect, when you write, and when
    /// they ack, so a long outbox goes out without flooding the connection.
    fn pump(&mut self, to: &Code) -> Result<(), StoreError> {
        if !self.links.contains_key(to) {
            return Ok(());
        }
        // The in-flight messages are the oldest waiting ones, so this covers them.
        let waiting = self.store.oldest_waiting(to, WINDOW)?;
        let Some(link) = self.links.get_mut(to) else { return Ok(()) };
        let still_waiting: HashSet<&str> = waiting.iter().map(|m| m.id.as_str()).collect();
        link.in_flight.retain(|id| still_waiting.contains(id.as_str()));
        for m in waiting {
            if link.in_flight.len() >= WINDOW {
                break;
            }
            if link.in_flight.contains(&m.id) {
                continue;
            }
            if !link.queue(Frame::Msg { id: m.id.clone(), sent_at: m.sent_at, text: m.text }) {
                break;
            }
            link.in_flight.insert(m.id);
        }
        Ok(())
    }

    fn internal(&mut self, message: Internal) {
        match message {
            Internal::Incoming(conn) => self.incoming(conn),
            Internal::Ready { peer, id, conn, role, hello, out, verdict } => {
                let now = Instant::now();
                let link = Link {
                    id,
                    out,
                    conn,
                    initiator: peer,
                    in_flight: HashSet::new(),
                    heard_name: None,
                    session: 0,
                    opened: now,
                    heard: now,
                };
                self.ready(peer, role, hello, link, verdict);
            }
            Internal::Frame { peer, frame } => self.frame(peer, frame),
            Internal::Closed { peer, id, duplicate } => self.closed(peer, id, duplicate),
            Internal::Connected { peer, still } => {
                let contact = self.store.is_contact(&peer).unwrap_or_else(|e| {
                    self.problem(e);
                    false
                });
                let _ = still.send(contact);
            }
            Internal::Failed { peer, id, role } => {
                self.guests.remove(&id);
                if role == Role::Dialer {
                    self.dialing.remove(&peer);
                    self.back_off(peer, None);
                }
            }
            Internal::VersionRefused { peer, id, theirs } => {
                self.guests.remove(&id);
                self.dialing.remove(&peer);
                self.mismatch(peer, theirs);
                self.back_off(peer, Some(self.config.retry_max));
            }
            Internal::Network(online) => {
                if self.online.replace(online) != Some(online) {
                    self.emit(Event::Network { online });
                }
            }
        }
    }

    fn incoming(&mut self, conn: Connection) {
        let peer = Code::from_key(conn.remote_id());
        let standing = match self.store.standing(&peer) {
            Ok(s) => s,
            Err(e) => {
                conn.close(close::ERROR.into(), b"error");
                return self.problem(e);
            }
        };
        match standing {
            Some(Standing::Contact) => {
                self.spawn_link(conn, Role::Acceptor, peer);
            }
            // Before any hello, so to them you look offline.
            Some(Standing::Blocked) => conn.close(close::REFUSED.into(), b""),
            Some(Standing::Request) | None => match self.room_for_guest(standing.is_none()) {
                Ok(true) => {
                    let id = self.spawn_link(conn, Role::Acceptor, peer);
                    self.guests.insert(id);
                }
                Ok(false) => conn.close(close::REFUSED.into(), b""),
                Err(e) => {
                    conn.close(close::ERROR.into(), b"error");
                    self.problem(e);
                }
            },
        }
    }

    /// Whether another guest may connect. A `stranger` (not even a request)
    /// also needs room for one more request.
    fn room_for_guest(&self, stranger: bool) -> Result<bool, StoreError> {
        Ok(self.guests.len() < MAX_GUESTS && !(stranger && self.requests()? >= MAX_REQUESTS))
    }

    fn requests(&self) -> Result<usize, StoreError> {
        self.store.request_count()
    }

    fn spawn_link(&mut self, conn: Connection, role: Role, peer: Code) -> u64 {
        self.next_link += 1;
        let start = link::Start {
            conn,
            role,
            peer,
            id: self.next_link,
            hello: self.hello(),
            handshake_timeout: self.config.handshake_timeout,
            internal: self.internal.clone(),
        };
        tokio::spawn(link::run(start));
        self.next_link
    }

    fn hello(&self) -> Frame {
        Frame::Hello { proto: PROTO_VERSION, app_version: VERSION.into(), session: self.session }
    }

    /// A link has exchanged hellos. Decides whether to keep it; a link turned
    /// down here is closed without a single frame being read from it.
    fn ready(&mut self, peer: Code, role: Role, hello: Frame, mut link: Link, verdict: oneshot::Sender<Verdict>) {
        if role == Role::Dialer {
            self.dialing.remove(&peer);
        }
        let refuse = |core: &mut Core, link: Link, code: u32| {
            link.conn.close(code.into(), b"");
            core.guests.remove(&link.id);
        };
        let standing = match self.store.standing(&peer) {
            Ok(s) => s,
            Err(e) => {
                refuse(self, link, close::ERROR);
                return self.problem(e);
            }
        };
        let contact = standing == Some(Standing::Contact);
        // Blocked meanwhile, or dialed as a contact and removed meanwhile.
        if standing == Some(Standing::Blocked) || (role == Role::Dialer && !contact) {
            return refuse(self, link, close::REFUSED);
        }
        let Frame::Hello { proto, session, .. } = hello else {
            refuse(self, link, close::BAD_HELLO);
            return self.back_off(peer, None);
        };
        if proto != PROTO_VERSION {
            // Either way they learn our version, so their app can say who
            // needs to update: as a hello if they dialed, else as the close reason.
            self.guests.remove(&link.id);
            if role == Role::Acceptor {
                let _ = verdict.send(Verdict::TellVersion);
            } else {
                link.conn.close(close::VERSION.into(), PROTO_VERSION.to_string().as_bytes());
            }
            if contact {
                self.mismatch(peer, proto);
                self.back_off(peer, Some(self.config.retry_max));
            }
            return;
        }
        self.mismatched.remove(&peer);
        // A contact removed while saying hello is a guest now, and needs room like any.
        if !contact && !self.guests.contains(&link.id) {
            match self.room_for_guest(standing.is_none()) {
                Ok(true) => {}
                Ok(false) => return refuse(self, link, close::REFUSED),
                Err(e) => {
                    refuse(self, link, close::ERROR);
                    return self.problem(e);
                }
            }
        }
        link.session = session;
        link.initiator = if role == Role::Dialer { self.me } else { peer };
        let was_online = if let Some(existing) = self.links.get(&peer) {
            // A link from another run of their app: the old one is dead (they
            // restarted without closing it), so the new one wins.
            let restarted = existing.session != link.session;
            // Both sides dialed at once. Both keep the connection opened by the
            // smaller key, so they agree; a newer connection from the same
            // side replaces a stale one.
            let smaller = if self.me.key().as_bytes() < peer.key().as_bytes() { self.me } else { peer };
            if !restarted && existing.initiator != link.initiator && existing.initiator == smaller {
                return refuse(self, link, close::DUPLICATE);
            }
            existing.conn.close(close::DUPLICATE.into(), b"duplicate");
            self.guests.remove(&existing.id);
            true
        } else {
            self.quiet.remove(&peer)
        };
        let _ = verdict.send(Verdict::Accept);
        if !contact {
            self.guests.insert(link.id);
            self.links.insert(peer, link);
            return;
        }
        self.guests.remove(&link.id);
        self.links.insert(peer, link);
        if !was_online {
            self.emit(Event::Online(peer));
        }
        if let Err(e) = self.catch_up(&peer) {
            self.problem(e);
        }
    }

    /// Sends a contact what may have waited or been lost while not connected:
    /// your current name, messages not yet acked, and the latest read receipt.
    /// All of them are safe to receive twice. The name goes first, so if they
    /// don't have you as a contact, their request arrives with your name.
    fn catch_up(&mut self, peer: &Code) -> Result<(), StoreError> {
        self.send_frame(peer, Frame::Name { name: self.name.clone() });
        self.pump(peer)?;
        if let Some(up_to) = self.store.read_upto(peer)? {
            self.send_frame(peer, Frame::Read { up_to });
        }
        Ok(())
    }

    fn frame(&mut self, peer: Code, frame: Frame) {
        // From a connection dropped since (blocked or removed): ignore. Frames
        // from a connection being replaced still count, since every frame is
        // safe to receive twice and dropping one could strand a message.
        let Some(link) = self.links.get_mut(&peer) else { return };
        link.heard = Instant::now();
        let result = self.store.standing(&peer).and_then(|standing| match frame {
            Frame::Msg { id, sent_at, text } => self.message(peer, standing, id, sent_at, text),
            Frame::Ack { id } => {
                if let Some(link) = self.links.get_mut(&peer) {
                    link.in_flight.remove(&id);
                }
                if self.store.mark_delivered(&peer, &id)? {
                    self.emit(Event::Delivered { to: peer, id });
                }
                if standing == Some(Standing::Contact) { self.pump(&peer) } else { Ok(()) }
            }
            Frame::Read { up_to } => {
                if self.store.mark_read(&peer, &up_to)? {
                    self.emit(Event::Read { by: peer, up_to });
                }
                Ok(())
            }
            Frame::Name { name } => match clean_name(&name) {
                Ok(name) => self.their_name(peer, standing, name),
                Err(_) => Ok(()),
            },
            Frame::Hello { .. } => Ok(()), // links close on a second hello and never pass it on
        });
        if let Err(e) = result {
            self.problem(e);
        }
    }

    /// Saves a message and acks it, or, from a guest or request that's used up
    /// its room, leaves it unacked so it waits on their side.
    fn message(
        &mut self,
        peer: Code,
        standing: Option<Standing>,
        id: String,
        sent_at: i64,
        text: String,
    ) -> Result<(), StoreError> {
        if standing == Some(Standing::Blocked) {
            return Ok(());
        }
        // A repeat (our ack was lost, maybe before you removed them): ack it
        // again, without writing anything or opening a request. Checking first
        // matters: even an insert that changes nothing costs a disk sync.
        if self.store.has_message(&peer, &id)? {
            self.send_frame(&peer, Frame::Ack { id });
            return Ok(());
        }
        match standing {
            Some(Standing::Contact | Standing::Blocked) => {}
            Some(Standing::Request) => {
                if self.store.request_messages(&peer)? >= MAX_REQUEST_MESSAGES {
                    return Ok(());
                }
            }
            None => {
                if self.requests()? >= MAX_REQUESTS {
                    return Ok(());
                }
                let name = self.links.get(&peer).and_then(|l| l.heard_name.clone()).unwrap_or_default();
                self.store.add_request(&peer, &name)?;
                self.emit(Event::Request { from: peer, name });
            }
        }
        if self.store.add_incoming(&peer, &id, sent_at, &text)? {
            self.emit(Event::Received { from: peer, id: id.clone(), sent_at, text });
        }
        self.send_frame(&peer, Frame::Ack { id });
        Ok(())
    }

    fn their_name(&mut self, peer: Code, standing: Option<Standing>, name: String) -> Result<(), StoreError> {
        match standing {
            None => {
                if let Some(link) = self.links.get_mut(&peer) {
                    link.heard_name = Some(name);
                }
            }
            Some(Standing::Blocked) => {}
            Some(standing @ (Standing::Contact | Standing::Request)) => {
                let Some(before) = self.store.person(&peer)?.map(|p| p.name) else { return Ok(()) };
                if before != name {
                    // A request's first name isn't a change: apps before
                    // 0.1.0-rc.1 sent it after their first message.
                    if standing == Standing::Request && !before.is_empty() {
                        let renames = self.renames.entry(peer).or_default();
                        if *renames >= MAX_REQUEST_RENAMES {
                            return Ok(());
                        }
                        *renames += 1;
                    }
                    self.store.set_contact_name(&peer, &name)?;
                    self.emit(Event::Name { of: peer, name });
                }
            }
        }
        Ok(())
    }

    fn closed(&mut self, peer: Code, id: u64, duplicate: bool) {
        self.guests.remove(&id);
        if !self.links.get(&peer).is_some_and(|l| l.id == id) {
            return; // a connection we had already replaced or dropped
        }
        let Some(link) = self.links.remove(&peer) else { return };
        let lasted = link.opened.elapsed() >= self.config.stable_link;
        match self.store.is_contact(&peer) {
            Ok(true) => {}
            Ok(false) => return, // guests and requests are never dialed back
            Err(e) => return self.problem(e),
        }
        if duplicate {
            // Their side kept the other connection, which is still starting here.
            self.quiet.insert(peer);
        } else {
            self.went_offline(peer);
        }
        if lasted {
            self.retry.insert(peer, Retry { at: Instant::now() + self.config.retry_first, failures: 0 });
        } else {
            // Up and straight down again: as good as a failed attempt.
            self.after_failure(peer, None);
        }
    }

    /// Closes guest links that have said nothing for [`Config::guest_idle`],
    /// so strangers who stay silent can't hold every guest slot.
    fn sweep_guests(&mut self) {
        let idle = self.config.guest_idle;
        for link in self.links.values().filter(|l| self.guests.contains(&l.id) && l.heard.elapsed() >= idle) {
            link.conn.close(close::IDLE.into(), b"idle");
        }
    }

    /// A contact's app speaks protocol `theirs`: said once, and again only if it changes.
    fn mismatch(&mut self, peer: Code, theirs: u32) {
        if self.mismatched.insert(peer, theirs) != Some(theirs) {
            self.emit(Event::VersionMismatch { peer, theirs });
        }
    }

    fn went_offline(&mut self, peer: Code) {
        self.emit(Event::Offline(peer));
        if let Err(e) = self.store.set_last_seen(&peer, now_ms()) {
            self.problem(e);
        }
    }

    fn retry_in(&mut self, peer: Code, delay: Duration) {
        let failures = self.retry.get(&peer).map_or(0, |r| r.failures);
        self.retry.insert(peer, Retry { at: Instant::now() + delay, failures });
    }

    /// Tries again after `wait`, or else after the first wait doubled once
    /// for every failure since the last link that lasted.
    fn after_failure(&mut self, peer: Code, wait: Option<Duration>) {
        let failures = self.retry.get(&peer).map_or(0, |r| r.failures);
        let doubled = self.config.retry_first.saturating_mul(1 << failures.min(20));
        let delay = wait.unwrap_or(doubled).min(self.config.retry_max);
        self.retry.insert(peer, Retry { at: Instant::now() + delay, failures: failures.saturating_add(1) });
    }

    /// Reaching a contact didn't work out: try again after `wait`, or else
    /// after a wait that doubles with each failure (see `after_failure`).
    /// Nothing to do while another connection to them is up, or if they
    /// aren't a contact (any more).
    fn back_off(&mut self, peer: Code, wait: Option<Duration>) {
        if self.links.contains_key(&peer) {
            return;
        }
        match self.store.is_contact(&peer) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => return self.problem(e),
        }
        if self.quiet.remove(&peer) {
            self.went_offline(peer);
        }
        self.after_failure(peer, wait);
    }

    fn next_dial(&self) -> Option<Instant> {
        next_due(&self.retry, |peer| self.dialing.contains(peer) || self.links.contains_key(peer))
    }

    fn dial_due(&mut self) {
        let now = Instant::now();
        let due: Vec<Code> = self
            .retry
            .iter()
            .filter(|(peer, r)| r.at <= now && !self.dialing.contains(peer) && !self.links.contains_key(peer))
            .map(|(peer, _)| *peer)
            .collect();
        for peer in due {
            self.dialing.insert(peer);
            let endpoint = self.endpoint.clone();
            let internal = self.internal.clone();
            let dial_timeout = self.config.dial_timeout;
            self.next_link += 1;
            let (id, hello, handshake_timeout) = (self.next_link, self.hello(), self.config.handshake_timeout);
            tokio::spawn(async move {
                let failed = Internal::Failed { peer, id, role: Role::Dialer };
                let Ok(Ok(conn)) = tokio::time::timeout(dial_timeout, endpoint.connect(peer.key(), ALPN)).await else {
                    let _ = internal.send(failed).await;
                    return;
                };
                let (still, answer) = oneshot::channel();
                if internal.send(Internal::Connected { peer, still }).await.is_err() || answer.await != Ok(true) {
                    conn.close(close::BYE.into(), b"bye");
                    let _ = internal.send(failed).await;
                    return;
                }
                let role = Role::Dialer;
                link::run(link::Start { conn, role, peer, id, hello, handshake_timeout, internal }).await;
            });
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn problem(&self, error: impl std::fmt::Display) {
        self.emit(Event::Problem(error.to_string()));
    }
}

/// Hands every incoming connection to the core. Each is awaited in its own
/// task, so one slow peer can't hold up the others.
async fn accept_loop(endpoint: Endpoint, internal: mpsc::Sender<Internal>) {
    while let Some(incoming) = endpoint.accept().await {
        let internal = internal.clone();
        tokio::spawn(async move {
            if let Ok(conn) = incoming.await {
                let _ = internal.send(Internal::Incoming(conn)).await;
            }
        });
    }
}

/// Tells the core whether the endpoint is connected to a relay: once it is,
/// or `connecting` has passed at start, and then on every change. Without
/// one, only peers on the same network can be reached. Ends with the core.
async fn watch_network(
    mut watcher: impl Watcher<Value = Vec<RelayStatus>>,
    internal: mpsc::Sender<Internal>,
    connecting: Duration,
) {
    let online = |relays: &[RelayStatus]| relays.iter().any(RelayStatus::is_connected);
    let mut relays = watcher.get();
    // At start the relay hasn't had a chance to answer: "offline" would be wrong.
    let deadline = Instant::now() + connecting;
    while !online(&relays) {
        tokio::select! {
            changed = tokio::time::timeout_at(deadline, watcher.updated()) => match changed {
                Ok(Ok(now)) => relays = now,
                Ok(Err(_)) => return,
                Err(_) => break,
            },
            _ = internal.closed() => return,
        }
    }
    loop {
        if internal.send(Internal::Network(online(&relays))).await.is_err() {
            return;
        }
        tokio::select! {
            changed = watcher.updated() => match changed {
                Ok(now) => relays = now,
                Err(_) => return,
            },
            _ = internal.closed() => return,
        }
    }
}

/// When the next dial is due: the earliest retry among peers that aren't
/// connected or being dialed. Busy peers never count, so a leftover entry
/// can't wake the loop over and over.
fn next_due(retry: &HashMap<Code, Retry>, busy: impl Fn(&Code) -> bool) -> Option<Instant> {
    retry.iter().filter(|(peer, _)| !busy(peer)).map(|(_, r)| r.at).min()
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn check_passphrase(passphrase: &Passphrase, typed: &str) -> Result<(), ChatError> {
    if passphrase.check(typed)? { Ok(()) } else { Err(ChatError::WrongPassphrase) }
}

fn change_passphrase(passphrase: &Passphrase, old: &str, new: &str, kdf: KdfParams) -> Result<(), ChatError> {
    if new.is_empty() {
        return Err(StoreError::EmptyPassphrase.into());
    }
    check_passphrase(passphrase, old)?;
    Ok(passphrase.change(new, kdf)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Seed;

    #[tokio::test(flavor = "multi_thread")]
    async fn the_network_watcher_ends_when_the_core_stops() {
        let lookup = iroh::address_lookup::MemoryLookup::new();
        let endpoint = crate::net::local_endpoint(Seed::generate().secret_key(), lookup).await.unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let watcher = tokio::spawn(watch_network(endpoint.home_relay_status(), tx, Duration::from_millis(10)));
        assert!(matches!(rx.recv().await, Some(Internal::Network(false))));
        drop(rx);
        tokio::time::timeout(Duration::from_secs(1), watcher).await.expect("it ended").unwrap();
    }

    #[test]
    fn connected_or_dialing_peers_are_never_due() {
        let now = Instant::now();
        let (linked, dialing, waiting) = (Seed::generate().code(), Seed::generate().code(), Seed::generate().code());
        let mut retry = HashMap::new();
        retry.insert(linked, Retry { at: now - Duration::from_secs(9), failures: 0 });
        retry.insert(dialing, Retry { at: now - Duration::from_secs(8), failures: 0 });
        retry.insert(waiting, Retry { at: now + Duration::from_secs(3), failures: 0 });
        let busy = |peer: &Code| *peer == linked || *peer == dialing;
        assert_eq!(next_due(&retry, busy), Some(now + Duration::from_secs(3)));
        assert_eq!(next_due(&retry, |_| true), None);
    }
}
