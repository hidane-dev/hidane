use std::process::ExitCode;

use clap::Parser;
use tokio::net::TcpListener;

/// hidane (火種, the seed of fire) — a Firestore emulator without Java.
#[derive(Debug, Parser)]
#[command(name = "hidane", version)]
struct Cli {
    /// The address to bind to on the local machine.
    #[arg(long, default_value = "localhost")]
    host: String,
    /// The port number to listen to on the local machine.
    #[arg(long, default_value_t = 8080)]
    port: u16,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let listener = match TcpListener::bind((cli.host.as_str(), cli.port)).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("failed to bind {}:{}: {err}", cli.host, cli.port);
            return ExitCode::FAILURE;
        }
    };
    match listener.local_addr() {
        Ok(addr) => println!("API endpoint: http://{addr}"),
        Err(err) => eprintln!("could not read the bound address: {err}"),
    }
    match hidane::serve(listener, hidane::http_routes()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("server error: {err}");
            ExitCode::FAILURE
        }
    }
}
