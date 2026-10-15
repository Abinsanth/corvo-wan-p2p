//! Throwaway M0 check: can two machines reach each other with iroh, by key alone?
//!
//!   iroh-spike listen          prints this machine's key, then waits for a line
//!   iroh-spike dial <key>      connects to that key and sends a line
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::{Endpoint, EndpointId, endpoint::presets};

const ALPN: &[u8] = b"corvo-wan-p2p/spike/0";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Check the arguments before going online, so a typo fails fast.
    let target = match args.as_slice() {
        [cmd] if cmd == "listen" => None,
        [cmd, key] if cmd == "dial" => Some(
            key.parse::<EndpointId>()
                .context("that key isn't valid: copy the whole key that `iroh-spike listen` printed")?,
        ),
        _ => bail!("usage: iroh-spike listen | iroh-spike dial <key>"),
    };
    let endpoint = Endpoint::builder(presets::N0).alpns(vec![ALPN.to_vec()]).bind().await?;
    endpoint.online().await;
    match target {
        None => listen(endpoint).await,
        Some(key) => dial(endpoint, key).await,
    }
}

async fn listen(endpoint: Endpoint) -> Result<()> {
    println!("key: {}", endpoint.id());
    println!("on the other machine run:  iroh-spike dial {}", endpoint.id());
    while let Some(incoming) = endpoint.accept().await {
        // One failed attempt shouldn't stop the listener.
        if let Err(err) = answer(incoming).await {
            eprintln!("a connection failed: {err:#}");
        }
    }
    Ok(())
}

async fn answer(incoming: iroh::endpoint::Incoming) -> Result<()> {
    let conn = incoming.await?;
    let (mut send, mut recv) = conn.accept_bi().await?;
    let line = recv.read_to_end(1024).await?;
    println!("from {}: {}", conn.remote_id().fmt_short(), String::from_utf8_lossy(&line));
    send.write_all(b"pong").await?;
    send.finish()?;
    report_path(&conn).await;
    conn.closed().await;
    Ok(())
}

async fn dial(endpoint: Endpoint, key: EndpointId) -> Result<()> {
    let conn = tokio::time::timeout(Duration::from_secs(30), endpoint.connect(key, ALPN))
        .await
        .context("no answer within 30 s: is `iroh-spike listen` running on the other machine?")??;
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(b"ping from the spike").await?;
    send.finish()?;
    let reply = recv.read_to_end(1024).await?;
    println!("reply: {}", String::from_utf8_lossy(&reply));
    report_path(&conn).await;
    conn.close(0u32.into(), b"bye");
    endpoint.close().await;
    Ok(())
}

/// Hole punching can finish after the first bytes, so look again after a few seconds.
async fn report_path(conn: &iroh::endpoint::Connection) {
    tokio::time::sleep(Duration::from_secs(5)).await;
    let paths = conn.paths();
    let direct = paths.iter().any(|p| p.is_ip());
    let relay = paths.iter().any(|p| p.is_relay());
    println!("path: direct={direct} relay={relay}");
}
