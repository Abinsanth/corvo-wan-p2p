//! One connection to one peer: the hello exchange, then a reader loop here and
//! a writer task, so a slow read never blocks a write (and reads are never
//! cancelled half-way).
//!
//! Nothing is read past their hello, and an accepted connection gets no hello
//! of ours, until the core has looked at it and said yes. So a connection the
//! core turns down never delivers a frame and, unless it was only turned down
//! for speaking another protocol version, never sees us answer.

use std::time::Duration;

use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use super::{Internal, OUT_QUEUE, close};
use crate::identity::Code;
use crate::proto::{Frame, FrameError, PROTO_VERSION, read_frame, write_frame};

/// The core's answer to a hello. Dropping the sender instead turns the link down.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Accept,
    /// They speak another protocol version: send our hello anyway, so their
    /// app can say who needs to update, then close.
    TellVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// We dialed: we open the stream and say hello first.
    Dialer,
    /// They dialed: we accept their stream and answer their hello.
    Acceptor,
}

/// Everything a link needs to start.
pub(super) struct Start {
    pub conn: Connection,
    pub role: Role,
    pub peer: Code,
    pub id: u64,
    pub hello: Frame,
    pub handshake_timeout: Duration,
    pub internal: mpsc::Sender<Internal>,
}

/// Runs the link until the connection ends, reporting to the core through `internal`.
pub(super) async fn run(start: Start) {
    let Start { conn, role, peer, id, hello, handshake_timeout, internal } = start;
    let handshake = tokio::time::timeout(handshake_timeout, shake_hands(&conn, role, &hello)).await;
    let (send, mut recv, theirs) = match handshake {
        Ok(Ok(parts)) => parts,
        _ => {
            // An app on another protocol version may have turned us down
            // without a hello; it names its version as the close reason.
            let refused = if role == Role::Dialer { refused_version(&conn) } else { None };
            debug!(peer = %peer.short(), ?role, "the hello exchange failed");
            conn.close(close::HANDSHAKE.into(), b"handshake failed");
            let failed = match refused {
                Some(theirs) => Internal::VersionRefused { peer, id, theirs },
                None => Internal::Failed { peer, id, role },
            };
            let _ = internal.send(failed).await;
            return;
        }
    };
    let (out, out_rx) = mpsc::channel(OUT_QUEUE);
    if role == Role::Acceptor {
        // Our hello is the first thing written, and only once the core says
        // yes: someone turned away (blocked, say) never gets one.
        let _ = out.try_send(hello.clone());
    }
    let (verdict, accepted) = oneshot::channel();
    let ready = Internal::Ready { peer, id, conn: conn.clone(), role, hello: theirs, out, verdict };
    if internal.send(ready).await.is_err() {
        return;
    }
    match accepted.await {
        Ok(Verdict::Accept) => {}
        Ok(Verdict::TellVersion) => return tell_version(&conn, send, &hello).await,
        Err(_) => return, // the core turned it down and has closed it
    }
    tokio::spawn(write_loop(send, out_rx));
    loop {
        match read_frame(&mut recv).await {
            Ok(Frame::Hello { .. }) => {
                warn!(peer = %peer.short(), "a second hello; closing the link");
                conn.close(close::BAD_HELLO.into(), b"unexpected hello");
                break;
            }
            Ok(frame) => {
                // Waits while the core is busy, so a peer sending faster than
                // we can save just stops being read (and QUIC slows them down).
                if internal.send(Internal::Frame { peer, frame }).await.is_err() {
                    break;
                }
            }
            Err(e @ (FrameError::TooBig(_) | FrameError::Invalid(_))) => {
                warn!(peer = %peer.short(), error = %e, "a bad frame; closing the link");
                conn.close(close::BAD_FRAME.into(), b"bad frame");
                break;
            }
            Err(FrameError::Connection(e)) => {
                debug!(peer = %peer.short(), error = %e, "the link ended");
                break;
            }
        }
    }
    // Closed by them as the losing half of a double dial, not because they left.
    let duplicate = matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(c)) if c.error_code == close::DUPLICATE.into()
    );
    conn.close(close::BYE.into(), b"bye");
    let _ = internal.send(Internal::Closed { peer, id, duplicate }).await;
}

/// Sends a peer on another protocol version our hello, gives their app a
/// moment to read it (it closes first), then closes, naming our version.
async fn tell_version(conn: &Connection, mut send: SendStream, hello: &Frame) {
    if write_frame(&mut send, hello).await.is_ok() {
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(1), conn.closed()).await;
    }
    conn.close(close::VERSION.into(), PROTO_VERSION.to_string().as_bytes());
}

/// The version an app named when it closed our connection for speaking
/// another one, if that's why it closed.
fn refused_version(conn: &Connection) -> Option<u32> {
    match conn.close_reason()? {
        ConnectionError::ApplicationClosed(c) if c.error_code == close::VERSION.into() => {
            std::str::from_utf8(&c.reason).ok()?.parse().ok()
        }
        _ => None,
    }
}

async fn shake_hands(
    conn: &Connection,
    role: Role,
    hello: &Frame,
) -> Result<(SendStream, RecvStream, Frame), FrameError> {
    let connection = |e: &dyn std::fmt::Display| FrameError::Connection(e.to_string());
    match role {
        Role::Dialer => {
            let (mut send, mut recv) = conn.open_bi().await.map_err(|e| connection(&e))?;
            write_frame(&mut send, hello).await?;
            let theirs = read_frame(&mut recv).await?;
            Ok((send, recv, theirs))
        }
        Role::Acceptor => {
            // Our hello waits for the core's verdict (see `run`).
            let (send, mut recv) = conn.accept_bi().await.map_err(|e| connection(&e))?;
            let theirs = read_frame(&mut recv).await?;
            Ok((send, recv, theirs))
        }
    }
}

async fn write_loop(mut send: SendStream, mut frames: mpsc::Receiver<Frame>) {
    while let Some(frame) = frames.recv().await {
        if write_frame(&mut send, &frame).await.is_err() {
            break;
        }
    }
    let _ = send.finish();
}
