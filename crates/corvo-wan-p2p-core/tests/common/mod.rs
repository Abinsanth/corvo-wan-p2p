//! Running real peers in one test process: each has its own store in a temp
//! folder and its own iroh endpoint on 127.0.0.1, with no relays. They find
//! each other through a shared in-memory address book.

#![allow(dead_code)] // each test file uses a different part of this

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use corvo_wan_p2p_core::chat::{Client, Config, Event};
use corvo_wan_p2p_core::identity::{Code, Seed};
use corvo_wan_p2p_core::net::{ALPN, local_addr, local_endpoint};
use corvo_wan_p2p_core::proto::{Frame, read_frame, write_frame};
use corvo_wan_p2p_core::store::{KdfParams, Store};
use iroh::Endpoint;
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{RecvStream, SendStream};
use tempfile::TempDir;

/// Short timings, so a test doesn't wait minutes for a retry.
pub const FAST: Config = Config {
    dial_timeout: Duration::from_secs(5),
    handshake_timeout: Duration::from_secs(5),
    retry_first: Duration::from_millis(200),
    retry_max: Duration::from_secs(1),
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

    async fn open(lookup: &MemoryLookup, dir: TempDir, seed: Seed) -> Peer {
        let store = Store::open(dir.path(), "pass").unwrap();
        let endpoint = local_endpoint(seed.secret_key(), lookup.clone()).await.unwrap();
        lookup.set_endpoint_info(local_addr(&endpoint));
        let client = Client::start(store, endpoint, FAST).unwrap();
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
}

impl Fake {
    pub async fn new(lookup: &MemoryLookup) -> Fake {
        let seed = Seed::generate();
        let endpoint = local_endpoint(seed.secret_key(), lookup.clone()).await.unwrap();
        lookup.set_endpoint_info(local_addr(&endpoint));
        Fake { code: seed.code(), endpoint }
    }

    /// Dials `to`, says hello with protocol `proto`, and returns the streams
    /// and the hello that came back. `None` if the app refused the connection,
    /// which it may do at any point of the exchange.
    pub async fn hello(&self, to: Code, proto: u32) -> Option<(SendStream, RecvStream, Frame)> {
        let exchange = async {
            let conn = self.endpoint.connect(to.key(), ALPN).await.ok()?;
            let (mut send, mut recv) = conn.open_bi().await.ok()?;
            write_frame(&mut send, &Frame::Hello { proto, app_version: "test".into() }).await.ok()?;
            let theirs = read_frame(&mut recv).await.ok()?;
            Some((send, recv, theirs))
        };
        tokio::time::timeout(PATIENCE, exchange).await.expect("the exchange took too long")
    }
}

impl Fake {
    /// Answers every connection with `reply` where a hello should be, keeps it
    /// open, and counts the connections.
    pub fn answer_with(&self, reply: Frame) -> Arc<AtomicUsize> {
        let count = Arc::new(AtomicUsize::new(0));
        let (endpoint, counter) = (self.endpoint.clone(), count.clone());
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let reply = reply.clone();
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let Ok((mut send, mut recv)) = conn.accept_bi().await else { return };
                    let _ = read_frame(&mut recv).await;
                    let _ = write_frame(&mut send, &reply).await;
                    conn.closed().await;
                });
            }
        });
        count
    }
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
