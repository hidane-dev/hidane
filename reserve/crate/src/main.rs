use clap::Parser;

/// hidane (火種) — a Firestore emulator without Java. Pre-release placeholder.
#[derive(Parser)]
#[command(name = "hidane", version)]
struct Cli {}

fn main() {
    let _ = Cli::parse();
    println!(
        "hidane {} — pre-release placeholder. See https://hidane.dev",
        env!("CARGO_PKG_VERSION")
    );
}
