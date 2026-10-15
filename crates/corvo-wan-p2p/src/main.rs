use clap::Parser;

mod ui;

/// Peer-to-peer chat in your terminal, over the internet.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Opens the screen with made-up people, to try it by hand.
    #[arg(long, hide = true)]
    demo_ui: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.demo_ui {
        ui::install_panic_hook();
        return tokio::runtime::Runtime::new()?.block_on(ui::demo());
    }
    println!("corvo-wan-p2p {} isn't usable yet: chat arrives in a later milestone.", corvo_wan_p2p_core::VERSION);
    Ok(())
}
