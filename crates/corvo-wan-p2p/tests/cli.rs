use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_corvo-wan-p2p")).args(args).output().expect("the binary runs")
}

#[test]
fn version_flag_prints_name_and_version() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), format!("corvo-wan-p2p {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_offers_recover_for_a_forgotten_passphrase() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--recover"));
    assert!(help.contains("12 words"));
}
