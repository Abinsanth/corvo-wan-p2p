//! The chat engine: one tokio task that owns the store and the iroh endpoint.
//!
//! The UI talks to it through a [`Client`]: requests go in as commands, and
//! everything that happens comes back as [`Event`]s. While running it keeps
//! trying to reach every contact (backing off while they're offline), sends
//! waiting messages as soon as a connection opens, and confirms what arrives.

mod link;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh::Endpoint;
use iroh::endpoint::Connection;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::VERSION;
use crate::identity::{Code, NameError, clean_name};
use crate::net::ALPN;
use crate::proto::{Frame, MAX_FRAME, MAX_TEXT, PROTO_VERSION, encode};
use crate::store::{Contact, Store, StoreError, StoredMessage};
use link::Role;

/// The close code for the connection dropped when both sides dialed at once.
const DUPLICATE: u32 = 6;

/// Timings. [`Config::DEFAULT`] is what the app uses; tests use shorter ones.
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
}

impl Config {
    pub const DEFAULT: Config = Config {
        dial_timeout: Duration::from_secs(30),
        handshake_timeout: Duration::from_secs(10),
        retry_first: Duration::from_secs(5),
        retry_max: Duration::from_secs(300),
    };
}

/// Something that happened, for the UI to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A connection to them is open.
    Online(Code),
    /// The connection to them ended.
    Offline(Code),
    /// A new message from them.
    Received { from: Code, id: String, sent_at: i64, text: String },
    /// Their app saved your message `id` (`✓`).
    Delivered { to: Code, id: String },
    /// They read your messages up to `up_to` (`✓✓`).
    Read { by: Code, up_to: String },
    /// Their display name, as their app told us.
    Name { of: Code, name: String },
    /// Their app speaks a different protocol version, so nothing can be exchanged
    /// until one of you updates. Messages keep waiting.
    VersionMismatch { peer: Code, theirs: u32 },
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
    #[error("you can't add yourself")]
    Yourself,
    #[error("a message can't be empty")]
    Empty,
    #[error("that message is too long; the most is 16 KB")]
    TooLong,
    #[error("the chat engine has stopped")]
    Stopped,
}

type Reply<T> = oneshot::Sender<Result<T, ChatError>>;

enum Command {
    AddContact(Code, Reply<()>),
    Send(Code, String, Reply<String>),
    MarkRead(Code, Reply<()>),
    SetName(String, Reply<String>),
    Contacts(Reply<Vec<Contact>>),
    History(Code, Reply<Vec<StoredMessage>>),
    Shutdown,
}

/// What the link tasks and the accept loop tell the core.
enum Internal {
    Incoming(Connection),
    Ready { peer: Code, id: u64, conn: Connection, role: Role, hello: Frame, out: mpsc::UnboundedSender<Frame> },
    Frame { peer: Code, frame: Frame },
    Closed { peer: Code, id: u64, duplicate: bool },
    DialFailed { peer: Code },
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
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::unbounded_channel();
        let (internal, internal_rx) = mpsc::unbounded_channel();
        let core = Core {
            store,
            endpoint,
            config,
            me,
            name: profile.name,
            events: events_tx,
            internal,
            links: HashMap::new(),
            quiet: HashSet::new(),
            dialing: HashSet::new(),
            retry: HashMap::new(),
            next_link: 0,
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

    pub async fn add_contact(&self, code: Code) -> Result<(), ChatError> {
        self.ask(|r| Command::AddContact(code, r)).await
    }

    /// Saves a message and sends it as soon as `to` is reachable. Returns its id.
    pub async fn send(&self, to: Code, text: &str) -> Result<String, ChatError> {
        self.ask(|r| Command::Send(to, text.to_string(), r)).await
    }

    /// You've seen everything `peer` sent so far: tells them (now, or once you're connected).
    pub async fn mark_read(&self, peer: Code) -> Result<(), ChatError> {
        self.ask(|r| Command::MarkRead(peer, r)).await
    }

    /// Changes your display name and tells everyone connected. Returns it as saved.
    pub async fn set_name(&self, name: &str) -> Result<String, ChatError> {
        self.ask(|r| Command::SetName(name.to_string(), r)).await
    }

    pub async fn contacts(&self) -> Result<Vec<Contact>, ChatError> {
        self.ask(Command::Contacts).await
    }

    pub async fn history(&self, peer: Code) -> Result<Vec<StoredMessage>, ChatError> {
        self.ask(|r| Command::History(peer, r)).await
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
    out: mpsc::UnboundedSender<Frame>,
    conn: Connection,
    /// Who opened this connection.
    initiator: Code,
}

struct Retry {
    at: Instant,
    delay: Duration,
}

struct Core {
    store: Store,
    endpoint: Endpoint,
    config: Config,
    me: Code,
    name: String,
    events: mpsc::UnboundedSender<Event>,
    internal: mpsc::UnboundedSender<Internal>,
    links: HashMap<Code, Link>,
    /// Peers whose connection was dropped as a duplicate while the other one
    /// was still starting: not reported offline unless reaching them fails.
    quiet: HashSet<Code>,
    dialing: HashSet<Code>,
    retry: HashMap<Code, Retry>,
    next_link: u64,
}

impl Core {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut internal: mpsc::UnboundedReceiver<Internal>,
    ) {
        let accept = tokio::spawn(accept_loop(self.endpoint.clone(), self.internal.clone()));
        match self.store.contacts() {
            Ok(contacts) => {
                for contact in contacts {
                    self.retry_in(contact.code, Duration::ZERO);
                }
            }
            Err(e) => self.problem(e),
        }
        loop {
            let due = self.next_dial();
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Shutdown) | None => break,
                    Some(command) => self.command(command),
                },
                Some(message) = internal.recv() => self.internal(message),
                _ = sleep_until(due) => self.dial_due(),
            }
        }
        accept.abort();
        for link in self.links.values() {
            link.conn.close(0u32.into(), b"bye");
        }
        self.endpoint.close().await;
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::AddContact(code, reply) => {
                let _ = reply.send(self.add_contact(code));
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
            Command::Contacts(reply) => {
                let _ = reply.send(self.store.contacts().map_err(ChatError::from));
            }
            Command::History(peer, reply) => {
                let _ = reply.send(self.store.history(&peer).map_err(ChatError::from));
            }
            Command::Shutdown => {}
        }
    }

    fn add_contact(&mut self, code: Code) -> Result<(), ChatError> {
        if code == self.me {
            return Err(ChatError::Yourself);
        }
        if self.store.add_contact(&code)? {
            self.retry_in(code, Duration::ZERO);
        }
        Ok(())
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
        self.send_frame(&to, frame);
        Ok(id)
    }

    fn mark_read(&mut self, peer: Code) -> Result<(), ChatError> {
        let Some(up_to) = self.store.last_received(&peer)? else { return Ok(()) };
        self.store.note_read(&peer, &up_to)?;
        self.send_frame(&peer, Frame::Read { up_to });
        Ok(())
    }

    fn set_name(&mut self, name: &str) -> Result<String, ChatError> {
        let name = self.store.set_name(name)?;
        self.name = name.clone();
        for link in self.links.values() {
            let _ = link.out.send(Frame::Name { name: name.clone() });
        }
        Ok(name)
    }

    /// Queues `frame` for `to` if connected. `true` if it was queued.
    fn send_frame(&self, to: &Code, frame: Frame) -> bool {
        self.links.get(to).is_some_and(|link| link.out.send(frame).is_ok())
    }

    fn internal(&mut self, message: Internal) {
        match message {
            Internal::Incoming(conn) => self.incoming(conn),
            Internal::Ready { peer, id, conn, role, hello, out } => self.ready(peer, id, conn, role, hello, out),
            // From any connection to them, even one being replaced: every frame
            // is safe to receive twice, and dropping one could strand a message.
            Internal::Frame { peer, frame } => self.frame(peer, frame),
            Internal::Closed { peer, id, duplicate } => self.closed(peer, id, duplicate),
            Internal::DialFailed { peer } => {
                self.dialing.remove(&peer);
                if self.quiet.remove(&peer) {
                    self.went_offline(peer);
                }
                if !self.links.contains_key(&peer) {
                    let delay = self.retry.get(&peer).map_or(self.config.retry_first, |r| r.delay * 2);
                    self.retry_in(peer, delay.min(self.config.retry_max));
                }
            }
        }
    }

    fn incoming(&mut self, conn: Connection) {
        let peer = Code::from_key(conn.remote_id());
        match self.store.is_contact(&peer) {
            Ok(true) => self.spawn_link(conn, Role::Acceptor, peer),
            Ok(false) => conn.close(2u32.into(), b"not a contact"),
            Err(e) => {
                conn.close(3u32.into(), b"error");
                self.problem(e);
            }
        }
    }

    fn spawn_link(&mut self, conn: Connection, role: Role, peer: Code) {
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
    }

    fn hello(&self) -> Frame {
        Frame::Hello { proto: PROTO_VERSION, app_version: VERSION.into() }
    }

    fn ready(
        &mut self,
        peer: Code,
        id: u64,
        conn: Connection,
        role: Role,
        hello: Frame,
        out: mpsc::UnboundedSender<Frame>,
    ) {
        if role == Role::Dialer {
            self.dialing.remove(&peer);
        }
        let Frame::Hello { proto, .. } = hello else {
            conn.close(4u32.into(), b"expected hello");
            return;
        };
        if proto != PROTO_VERSION {
            conn.close(5u32.into(), b"protocol version");
            self.emit(Event::VersionMismatch { peer, theirs: proto });
            self.retry_in(peer, self.config.retry_max);
            return;
        }
        let initiator = if role == Role::Dialer { self.me } else { peer };
        let was_online = if let Some(existing) = self.links.get(&peer) {
            // Both sides dialed at once. Both keep the connection opened by the
            // smaller key, so they agree; a newer connection from the same
            // side replaces a stale one.
            let smaller = if self.me.key().as_bytes() < peer.key().as_bytes() { self.me } else { peer };
            if existing.initiator != initiator && existing.initiator == smaller {
                conn.close(DUPLICATE.into(), b"duplicate");
                return;
            }
            existing.conn.close(DUPLICATE.into(), b"duplicate");
            true
        } else {
            self.quiet.remove(&peer)
        };
        self.links.insert(peer, Link { id, out, conn, initiator });
        self.retry.remove(&peer);
        if !was_online {
            self.emit(Event::Online(peer));
        }
        if let Err(e) = self.catch_up(&peer) {
            self.problem(e);
        }
    }

    /// Sends what may have waited or been lost while not connected: messages
    /// not yet acked, the latest read receipt, and your current name. All of
    /// them are safe to receive twice.
    fn catch_up(&mut self, peer: &Code) -> Result<(), StoreError> {
        for m in self.store.waiting(peer)? {
            self.send_frame(peer, Frame::Msg { id: m.id, sent_at: m.sent_at, text: m.text });
        }
        if let Some(up_to) = self.store.read_upto(peer)? {
            self.send_frame(peer, Frame::Read { up_to });
        }
        self.send_frame(peer, Frame::Name { name: self.name.clone() });
        Ok(())
    }

    fn their_name(&mut self, peer: Code, name: String) {
        match self.store.contact(&peer) {
            Ok(Some(contact)) if contact.name == name => {}
            Ok(_) => match self.store.set_contact_name(&peer, &name) {
                Ok(()) => self.emit(Event::Name { of: peer, name }),
                Err(e) => self.problem(e),
            },
            Err(e) => self.problem(e),
        }
    }

    fn frame(&mut self, peer: Code, frame: Frame) {
        let result = match frame {
            Frame::Msg { id, sent_at, text } => self.store.add_incoming(&peer, &id, sent_at, &text).map(|new| {
                if new {
                    self.emit(Event::Received { from: peer, id: id.clone(), sent_at, text });
                }
                // Ack even a repeat: our earlier ack may have been lost.
                self.send_frame(&peer, Frame::Ack { id });
            }),
            Frame::Ack { id } => self.store.mark_delivered(&peer, &id).map(|changed| {
                if changed {
                    self.emit(Event::Delivered { to: peer, id });
                }
            }),
            Frame::Read { up_to } => self.store.mark_read(&peer, &up_to).map(|changed| {
                if changed {
                    self.emit(Event::Read { by: peer, up_to });
                }
            }),
            Frame::Name { name } => {
                if let Ok(name) = clean_name(&name) {
                    self.their_name(peer, name);
                }
                Ok(())
            }
            Frame::Hello { .. } => {
                // A second hello breaks the protocol.
                if let Some(link) = self.links.get(&peer) {
                    link.conn.close(4u32.into(), b"unexpected hello");
                }
                Ok(())
            }
        };
        if let Err(e) = result {
            self.problem(e);
        }
    }

    fn closed(&mut self, peer: Code, id: u64, duplicate: bool) {
        if !self.links.get(&peer).is_some_and(|l| l.id == id) {
            return; // a connection we had already replaced
        }
        self.links.remove(&peer);
        if duplicate {
            // Their side kept the other connection, which is still starting here.
            self.quiet.insert(peer);
        } else {
            self.went_offline(peer);
        }
        self.retry_in(peer, self.config.retry_first);
    }

    fn went_offline(&mut self, peer: Code) {
        self.emit(Event::Offline(peer));
        if let Err(e) = self.store.set_last_seen(&peer, now_ms()) {
            self.problem(e);
        }
    }

    fn retry_in(&mut self, peer: Code, delay: Duration) {
        self.retry.insert(peer, Retry { at: Instant::now() + delay, delay: delay.max(self.config.retry_first) });
    }

    fn next_dial(&self) -> Option<Instant> {
        self.retry.iter().filter(|(peer, _)| !self.dialing.contains(peer)).map(|(_, r)| r.at).min()
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
                match tokio::time::timeout(dial_timeout, endpoint.connect(peer.key(), ALPN)).await {
                    Ok(Ok(conn)) => {
                        let role = Role::Dialer;
                        link::run(link::Start { conn, role, peer, id, hello, handshake_timeout, internal }).await;
                    }
                    _ => {
                        let _ = internal.send(Internal::DialFailed { peer });
                    }
                }
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
async fn accept_loop(endpoint: Endpoint, internal: mpsc::UnboundedSender<Internal>) {
    while let Some(incoming) = endpoint.accept().await {
        let internal = internal.clone();
        tokio::spawn(async move {
            if let Ok(conn) = incoming.await {
                let _ = internal.send(Internal::Incoming(conn));
            }
        });
    }
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
