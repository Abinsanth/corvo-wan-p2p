//! The iroh endpoint: how this app reaches others by their key.

use std::net::{Ipv4Addr, SocketAddr};

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{BindError, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};

/// The protocol name both apps must agree on, or the connection is refused.
pub const ALPN: &[u8] = b"corvo-wan-p2p/1";

/// The real thing: n0's discovery and relays, so anyone on the internet can be reached by key.
pub async fn internet_endpoint(secret: SecretKey) -> Result<Endpoint, BindError> {
    Endpoint::builder(presets::N0).secret_key(secret).alpns(vec![ALPN.to_vec()]).bind().await
}

/// For tests: no relays and no internet lookups. Peers find each other through
/// `lookup`, where each one registers itself with [`local_addr`].
pub async fn local_endpoint(secret: SecretKey, lookup: MemoryLookup) -> Result<Endpoint, BindError> {
    Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup)
        .bind()
        .await
}

/// Where a local endpoint can be reached on this machine (its ports on 127.0.0.1).
pub fn local_addr(endpoint: &Endpoint) -> EndpointAddr {
    let addrs = endpoint
        .bound_sockets()
        .into_iter()
        .filter(SocketAddr::is_ipv4)
        .map(|s| TransportAddr::Ip(SocketAddr::from((Ipv4Addr::LOCALHOST, s.port()))));
    EndpointAddr::from_parts(endpoint.id(), addrs)
}
