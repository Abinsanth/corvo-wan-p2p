//! One connection to one peer: the hello exchange, then a reader loop here and
//! a writer task, so a slow read never blocks a write (and reads are never
//! cancelled half-way).

use std::time::Duration;

use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use tokio::sync::mpsc;

use super::{DUPLICATE, Internal};
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
    pub internal: mpsc::UnboundedSender<Internal>,
}

/// Runs the link until the connection ends, reporting to the core through `internal`.
pub(super) async fn run(start: Start) {
    let Start { conn, role, peer, id, hello, handshake_timeout, internal } = start;
    let handshake = tokio::time::timeout(handshake_timeout, shake_hands(&conn, role, &hello)).await;
    let (send, mut recv, theirs) = match handshake {
        Ok(Ok(parts)) => parts,
        _ => {
            conn.close(1u32.into(), b"handshake failed");
            if role == Role::Dialer {
                let _ = internal.send(Internal::DialFailed { peer });
            }
            return;
        }
    };
    let (out, out_rx) = mpsc::unbounded_channel();
    tokio::spawn(write_loop(send, out_rx));
    let ready = Internal::Ready { peer, id, conn: conn.clone(), role, hello: theirs, out };
    if internal.send(ready).is_err() {
        return;
    }
    while let Ok(frame) = read_frame(&mut recv).await {
        if internal.send(Internal::Frame { peer, frame }).is_err() {
            break;
        }
    }
    // Closed by them as the losing half of a double dial, not because they left.
    let duplicate = matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(close)) if close.error_code == DUPLICATE.into()
    );
    conn.close(0u32.into(), b"bye");
    let _ = internal.send(Internal::Closed { peer, id, duplicate });
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
            let (mut send, mut recv) = conn.accept_bi().await.map_err(|e| connection(&e))?;
            let theirs = read_frame(&mut recv).await?;
            write_frame(&mut send, hello).await?;
            Ok((send, recv, theirs))
        }
    }
}

async fn write_loop(mut send: SendStream, mut frames: mpsc::UnboundedReceiver<Frame>) {
    while let Some(frame) = frames.recv().await {
        if write_frame(&mut send, &frame).await.is_err() {
            break;
        }
    }
    let _ = send.finish();
}
