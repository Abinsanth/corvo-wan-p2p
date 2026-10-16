//! Running real peers in one test process: each has its own store in a temp
//! folder and its own iroh endpoint on 127.0.0.1, with no relays. They find
//! each other through a shared in-memory address book.

#![allow(dead_code)] // each test file uses a different part of this

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use corvo_wan_p2p_core::chat::{Client, Config, Event};
use corvo_wan_p2p_core::identity::{Code, Seed};
use corvo_wan_p2p_core::net::{ALPN, local_addr, local_endpoint};
use corvo_wan_p2p_core::proto::{Frame, read_frame, write_frame};
use corvo_wan_p2p_core::store::{KdfParams, Store};
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{QuicTransportConfig, RecvStream, SendStream, VarInt, presets};
use iroh::{Endpoint, RelayMode};
use tempfile::TempDir;

/// Short timings, so a test doesn't wait minutes for a retry.
pub const FAST: Config = Config {
    dial_timeout: Duration::from_secs(5),
    handshake_timeout: Duration::from_secs(5),
    retry_first: Duration::from_millis(200),
    retry_max: Duration::from_secs(1),
    stable_link: Duration::from_secs(1),
    guest_idle: Duration::from_secs(10),
    connecting: Duration::from_millis(300),
    kdf: KdfParams::FAST_INSECURE,
};

/// How long a test waits for something before failing.
const PATIENCE: Duration = Duration::from_secs(15);

pub struct Peer {
    pub client: Client,
    pub code: Code,
    dir: TempDir,
    seed: Seed,
    lookup: MemoryLookup,
}

impl Peer {
    /// A brand-new account called `name`.
    pub async fn start(lookup: &MemoryLookup, name: &str) -> Peer {
        Peer::start_with(lookup, Seed::generate(), name).await
    }

    /// A new account with a chosen seed, so others can add its code before it runs.
    pub async fn start_with(lookup: &MemoryLookup, seed: Seed, name: &str) -> Peer {
        let dir = tempfile::tempdir().unwrap();
        Store::create(dir.path(), "pass", KdfParams::FAST_INSECURE, &seed, name).unwrap();
        Peer::open(lookup, dir, seed).await
    }

    /// A new account locked with the real Argon2 settings, which take about a
    /// second, for tests about slow passphrase work.
    pub async fn start_with_real_kdf(lookup: &MemoryLookup, name: &str) -> Peer {
        let (dir, seed) = (tempfile::tempdir().unwrap(), Seed::generate());
        Store::create(dir.path(), "pass", KdfParams::DEFAULT, &seed, name).unwrap();
        Peer::open_with(lookup, dir, seed, Config { kdf: KdfParams::DEFAULT, ..FAST }).await
    }

    /// A new account whose engine runs with `config`.
    pub async fn start_with_config(lookup: &MemoryLookup, name: &str, config: Config) -> Peer {
        let (dir, seed) = (tempfile::tempdir().unwrap(), Seed::generate());
        Store::create(dir.path(), "pass", KdfParams::FAST_INSECURE, &seed, name).unwrap();
        Peer::open_with(lookup, dir, seed, config).await
    }

    async fn open(lookup: &MemoryLookup, dir: TempDir, seed: Seed) -> Peer {
        Peer::open_with(lookup, dir, seed, FAST).await
    }

    async fn open_with(lookup: &MemoryLookup, dir: TempDir, seed: Seed, config: Config) -> Peer {
        let store = Store::open(dir.path(), "pass").unwrap();
        let endpoint = local_endpoint(seed.secret_key(), lookup.clone()).await.unwrap();
        lookup.set_endpoint_info(local_addr(&endpoint));
        let client = Client::start(store, endpoint, config).unwrap();
        Peer { code: client.me(), client, dir, seed, lookup: lookup.clone() }
    }

    /// Quits the app and starts it again with the same data (on a new port).
    pub async fn restart(self) -> Peer {
        let Peer { client, dir, seed, lookup, .. } = self;
        client.shutdown().await;
        Peer::open(&lookup, dir, seed).await
    }

    /// Quits the app, keeping its data so it can start again later.
    pub async fn stop(self) -> (TempDir, Seed) {
        self.client.shutdown().await;
        (self.dir, self.seed)
    }

    pub async fn resume(lookup: &MemoryLookup, stopped: (TempDir, Seed)) -> Peer {
        Peer::open(lookup, stopped.0, stopped.1).await
    }

    /// Waits for the first event that `wanted` accepts, skipping others.
    pub async fn expect(&mut self, wanted: impl Fn(&Event) -> bool) -> Event {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            match tokio::time::timeout_at(deadline, self.client.next_event()).await {
                Ok(Some(event)) if wanted(&event) => return event,
                Ok(Some(_)) => continue,
                Ok(None) => panic!("the chat engine stopped"),
                Err(_) => panic!("waited {PATIENCE:?} for an event that never came"),
            }
        }
    }

    /// Skips every event that has already happened. After a command returns,
    /// that's everything from before it.
    pub async fn skip_pending(&mut self) {
        while let Ok(Some(_)) = tokio::time::timeout(Duration::ZERO, self.client.next_event()).await {}
    }

    /// Fails if `unwanted` happens within `within`.
    pub async fn expect_none(&mut self, within: Duration, unwanted: impl Fn(&Event) -> bool) {
        let deadline = tokio::time::Instant::now() + within;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.client.next_event()).await {
            assert!(!unwanted(&event), "unexpected event: {event:?}");
        }
    }
}

/// Two friends who have added each other, both online.
pub async fn friends(lookup: &MemoryLookup) -> (Peer, Peer) {
    let mut a = Peer::start(lookup, "abin").await;
    let mut b = Peer::start(lookup, "rahul").await;
    a.client.add_contact(b.code).await.unwrap();
    b.client.add_contact(a.code).await.unwrap();
    let (bc, ac) = (b.code, a.code);
    a.expect(|e| *e == Event::Online(bc)).await;
    b.expect(|e| *e == Event::Online(ac)).await;
    (a, b)
}

/// A hand-driven peer that speaks the wire protocol directly, for testing how
/// the app copes with odd or hostile behaviour.
pub struct Fake {
    pub endpoint: Endpoint,
    pub code: Code,
    /// What this run of the fake says in its hello, like an app's start.
    pub session: u64,
}

impl Fake {
    pub async fn new(lookup: &MemoryLookup) -> Fake {
        Fake::with_seed(lookup, Seed::generate()).await
    }

    /// A fake with a chosen key. Another one with the same seed is the same
    /// person's app after a restart.
    pub async fn with_seed(lookup: &MemoryLookup, seed: Seed) -> Fake {
        let endpoint = local_endpoint(seed.secret_key(), lookup.clone()).await.unwrap();
        lookup.set_endpoint_info(local_addr(&endpoint));
        Fake { code: seed.code(), endpoint, session: rand::random() }
    }

    /// The hello this fake's app would send.
    pub fn hello_frame(&self, proto: u32) -> Frame {
        Frame::Hello { proto, app_version: "test".into(), session: self.session }
    }

    /// Dials `to`, says hello with protocol `proto`, and returns the streams
    /// and the hello that came back. `None` if the app refused the connection,
    /// which it may do at any point of the exchange.
    pub async fn hello(&self, to: Code, proto: u32) -> Option<(SendStream, RecvStream, Frame)> {
        self.hello_with(to, proto, &[]).await
    }

    /// Only connects, saying nothing yet (see [`Fake::hello_on`]).
    pub async fn connect(&self, to: Code) -> Option<iroh::endpoint::Connection> {
        tokio::time::timeout(PATIENCE, self.endpoint.connect(to.key(), ALPN)).await.ok()?.ok()
    }

    /// Says hello on a connection from [`Fake::connect`].
    pub async fn hello_on(
        &self,
        conn: &iroh::endpoint::Connection,
        proto: u32,
    ) -> Option<(SendStream, RecvStream, Frame)> {
        let exchange = async {
            let (mut send, mut recv) = conn.open_bi().await.ok()?;
            write_frame(&mut send, &self.hello_frame(proto)).await.ok()?;
            let theirs = read_frame(&mut recv).await.ok()?;
            Some((send, recv, theirs))
        };
        tokio::time::timeout(PATIENCE, exchange).await.expect("the exchange took too long")
    }

    /// Like [`Fake::hello`], but sends `then` straight after the hello,
    /// without waiting for the app's answer.
    pub async fn hello_with(&self, to: Code, proto: u32, then: &[Frame]) -> Option<(SendStream, RecvStream, Frame)> {
        let exchange = async {
            let conn = self.endpoint.connect(to.key(), ALPN).await.ok()?;
            let (mut send, mut recv) = conn.open_bi().await.ok()?;
            write_frame(&mut send, &self.hello_frame(proto)).await.ok()?;
            for frame in then {
                write_frame(&mut send, frame).await.ok()?;
            }
            let theirs = read_frame(&mut recv).await.ok()?;
            Some((send, recv, theirs))
        };
        tokio::time::timeout(PATIENCE, exchange).await.expect("the exchange took too long")
    }
}

impl Fake {
    /// A fake that takes in at most `bytes` at a time from a connection, so
    /// whatever it doesn't read backs up on the sender's side.
    pub async fn with_small_window(lookup: &MemoryLookup, bytes: u32) -> Fake {
        let seed = Seed::generate();
        let transport = QuicTransportConfig::builder()
            .stream_receive_window(VarInt::from_u32(bytes))
            .receive_window(VarInt::from_u32(bytes))
            .build();
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(seed.secret_key())
            .alpns(vec![ALPN.to_vec()])
            .relay_mode(RelayMode::Disabled)
            .address_lookup(lookup.clone())
            .transport_config(transport)
            .bind()
            .await
            .unwrap();
        lookup.set_endpoint_info(local_addr(&endpoint));
        Fake { code: seed.code(), endpoint, session: rand::random() }
    }

    /// Answers every connection with `reply` where a hello should be, keeps it
    /// open, and counts the connections.
    pub fn answer_with(&self, reply: Frame) -> Arc<AtomicUsize> {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        self.answer(
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
            Answer::Reply(reply),
        );
        count
    }

    /// Like [`Fake::answer_with`], but notes when each connection came.
    pub fn answer_with_times(&self, reply: Frame) -> Arc<Mutex<Vec<tokio::time::Instant>>> {
        let times = Arc::new(Mutex::new(Vec::new()));
        let noted = times.clone();
        self.answer(move |at| noted.lock().unwrap().push(at), Answer::Reply(reply));
        times
    }

    /// Answers every connection with `reply`, then hangs up a moment later (code 0).
    /// Notes when each connection came.
    pub fn answer_and_hang_up(&self, reply: Frame) -> Arc<Mutex<Vec<tokio::time::Instant>>> {
        let times = Arc::new(Mutex::new(Vec::new()));
        let noted = times.clone();
        self.answer(move |at| noted.lock().unwrap().push(at), Answer::ReplyThenClose(reply));
        times
    }

    /// Reads each hello and closes the connection with `code` and `reason`,
    /// saying nothing.
    pub fn refuse_with(&self, code: u32, reason: &'static [u8]) {
        self.answer(|_| {}, Answer::Close(code, reason));
    }

    fn answer(&self, mut noted: impl FnMut(tokio::time::Instant) + Send + 'static, answer: Answer) {
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                noted(tokio::time::Instant::now());
                let answer = answer.clone();
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let Ok((mut send, mut recv)) = conn.accept_bi().await else { return };
                    let _ = read_frame(&mut recv).await;
                    match answer {
                        Answer::Reply(reply) => {
                            let _ = write_frame(&mut send, &reply).await;
                            conn.closed().await;
                        }
                        Answer::ReplyThenClose(reply) => {
                            let _ = write_frame(&mut send, &reply).await;
                            // Long enough for the link to come up first.
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            conn.close(VarInt::from_u32(0), b"bye");
                        }
                        Answer::Close(code, reason) => conn.close(VarInt::from_u32(code), reason),
                    }
                });
            }
        });
    }
}

#[derive(Clone)]
enum Answer {
    Reply(Frame),
    ReplyThenClose(Frame),
    Close(u32, &'static [u8]),
}

/// Reads frames until one that `wanted` accepts, skipping others (names, read receipts).
pub async fn next_frame(recv: &mut RecvStream, wanted: impl Fn(&Frame) -> bool) -> Frame {
    loop {
        let frame = tokio::time::timeout(PATIENCE, read_frame(recv)).await.unwrap().unwrap();
        if wanted(&frame) {
            return frame;
        }
    }
}

/// Sends a message frame.
pub async fn say(send: &mut SendStream, id: &str, text: &str) {
    write_frame(send, &Frame::Msg { id: id.into(), sent_at: 1, text: text.into() }).await.unwrap();
}

/// The next `n` frames, whatever they are.
pub async fn next_frames(recv: &mut RecvStream, n: usize) -> Vec<Frame> {
    let mut frames = Vec::new();
    for _ in 0..n {
        frames.push(next_frame(recv, |_| true).await);
    }
    frames
}

/// Every frame that arrives until none has for `quiet`. Only for checking
/// that nothing more comes: on a slow machine even the first frame can take
/// longer than `quiet`, so wait for expected frames with `next_frame`.
pub async fn frames_until_quiet(recv: &mut RecvStream, quiet: Duration) -> Vec<Frame> {
    let mut frames = Vec::new();
    while let Ok(Ok(frame)) = tokio::time::timeout(quiet, read_frame(recv)).await {
        frames.push(frame);
    }
    frames
}

/// Whether the other side closes the connection (reads until it fails).
pub async fn is_cut_off(recv: &mut RecvStream) -> bool {
    let reads = async { while read_frame(recv).await.is_ok() {} };
    tokio::time::timeout(PATIENCE, reads).await.is_ok()
}

/// Waits until `check` is true, trying every 50 ms.
pub async fn eventually(mut check: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "waited {PATIENCE:?} and it never became true");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
