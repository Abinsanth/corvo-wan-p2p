use std::io::stdout;

use anyhow::Context;
use clap::Parser;
use corvo_wan_p2p_core::chat::Config;
use corvo_wan_p2p_core::files::{data_dir, home_dir};
use corvo_wan_p2p_core::store::Store;

use self::ui::Ending;
use self::ui::onboarding::Onboarding;
use self::ui::runner::{Net, Runner};
use self::ui::state::State;

mod logging;
mod selfupdate;
mod ui;

/// Peer-to-peer chat in your terminal, over the internet.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Forgot your passphrase? Deletes the account on this computer (after you
    /// type yes) and brings it back from your 12 words: the same code, with no history.
    #[arg(long)]
    recover: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let runtime = tokio::runtime::Runtime::new()?;
    ui::install_panic_hook();
    let dir = data_dir().context("couldn't tell where your home folder is; set HOME (APPDATA on Windows)")?;
    logging::start(&dir);
    let mut state = State::onboarding(Onboarding::start(Store::exists(&dir), args.recover));
    state.data_dir = Some(ui::window::shown_path(&dir, home_dir().as_deref()));
    let runner = Runner::new(dir, home_dir(), Config::DEFAULT, Net::Internet, stdout());
    match runtime.block_on(ui::run(state, runner))? {
        Ending::Quit => println!("{}", ui::window::goodbye().join("\n")),
        Ending::Leave(notes) => {
            println!("{}\n", ui::window::goodbye().join("\n"));
            for note in notes {
                println!("{note}");
            }
        }
        Ending::Exit(why) => {
            eprintln!("{why}");
            std::process::exit(1);
        }
    }
    Ok(())
}
