//! One connection to one peer: the hello exchange, then a reader loop here and
//! a writer task, so a slow read never blocks a write (and reads are never
//! cancelled half-way).
//!
//! Nothing is read past their hello, and an accepted connection gets no hello
//! of ours, until the core has looked at it and said yes. So a connection the
//! core turns down never delivers a frame, and never sees us answer.

use std::time::Duration;

use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use tokio::sync::{mpsc, oneshot};

use super::{Internal, OUT_QUEUE, close};
use crate::identity::Code;
use crate::proto::{Frame, FrameError, read_frame, write_frame};

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
            conn.close(close::HANDSHAKE.into(), b"handshake failed");
            let _ = internal.send(Internal::Failed { peer, id, role }).await;
            return;
        }
    };
    let (out, out_rx) = mpsc::channel(OUT_QUEUE);
    if role == Role::Acceptor {
        // Our hello is the first thing written, and only once the core says
        // yes: someone turned away (blocked, say) never gets one.
        let _ = out.try_send(hello);
    }
    let (verdict, accepted) = oneshot::channel();
    let ready = Internal::Ready { peer, id, conn: conn.clone(), role, hello: theirs, out, verdict };
    if internal.send(ready).await.is_err() || accepted.await != Ok(true) {
        return; // the core turned it down and has closed it
    }
    tokio::spawn(write_loop(send, out_rx));
    while let Ok(frame) = read_frame(&mut recv).await {
        if matches!(frame, Frame::Hello { .. }) {
            conn.close(close::BAD_HELLO.into(), b"unexpected hello");
            break;
        }
        // Waits while the core is busy, so a peer sending faster than we can
        // save just stops being read (and QUIC slows them down).
        if internal.send(Internal::Frame { peer, frame }).await.is_err() {
            break;
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
