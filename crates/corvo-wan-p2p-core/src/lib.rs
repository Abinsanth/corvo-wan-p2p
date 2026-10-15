//! Everything corvo-wan-p2p does except drawing the screen: identity, the
//! encrypted store, the wire protocol, networking and the chat logic. It never
//! depends on terminal crates, so all of it can be tested without a screen.

pub mod chat;
pub mod files;
pub mod identity;
pub mod net;
pub mod proto;
pub mod recovery;
pub mod store;

/// The app version, shared by every crate in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
