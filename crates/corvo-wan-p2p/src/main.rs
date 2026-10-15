use clap::Parser;

/// Peer-to-peer chat in your terminal, over the internet.
#[derive(Parser)]
#[command(version)]
struct Args {}

fn main() {
    let _args = Args::parse();
    println!("corvo-wan-p2p {} isn't usable yet: chat arrives in a later milestone.", corvo_wan_p2p_core::VERSION);
}
