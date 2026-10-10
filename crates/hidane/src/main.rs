use std::{ffi::OsString, io::ErrorKind, process::ExitCode};

use clap::Parser;
use hidane::cli::{self, Cli};

mod launch;

fn main() -> ExitCode {
    match launch::mode(std::env::args_os().collect()) {
        launch::Mode::Exec(command) => launch::exec(&command),
        // firebase-tools running "java" in a `hidane exec` command (ADR 0006).
        launch::Mode::Java(args) => match launch::java(&args) {
            Ok(flags) => emulator(
                std::iter::once(OsString::from("hidane"))
                    .chain(flags)
                    .collect(),
            ),
            Err(code) => code,
        },
        launch::Mode::Emulator(args) => emulator(args),
    }
}

fn emulator(args: Vec<OsString>) -> ExitCode {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run(args)),
        Err(err) => {
            eprintln!("ERROR: could not start the runtime: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Vec<OsString>) -> ExitCode {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(2));
        }
    };
    if cli.help {
        print!("{}", cli::usage());
        return ExitCode::SUCCESS;
    }
    if cli.version {
        println!("hidane {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if cli.licenses {
        print!("{}", cli::licenses());
        return ExitCode::SUCCESS;
    }
    match cli.validate() {
        Ok(warnings) => {
            for warning in warnings {
                eprintln!("WARNING: {warning}");
            }
        }
        Err(err) => {
            eprintln!("ERROR: {err}");
            return ExitCode::from(2);
        }
    }
    if std::env::var("EXPERIMENTAL_MODE").is_ok_and(|v| v.eq_ignore_ascii_case("true")) {
        eprintln!("Emulator has been started in experimental mode!");
    }

    let listeners = match hidane::bind(&cli.host, cli.port).await {
        Ok(listeners) => listeners,
        Err(err) => {
            // firebase-tools looks for exactly this lowercase phrase on stderr to explain a
            // port conflict (downloadableEmulators.ts); Rust's own message is capitalised.
            let reason = if err.kind() == ErrorKind::AddrInUse {
                "address already in use".to_owned()
            } else {
                err.to_string()
            };
            eprintln!("ERROR: could not bind {}:{}: {reason}", cli.host, cli.port);
            return ExitCode::FAILURE;
        }
    };
    let port = listeners
        .first()
        .and_then(|l| l.local_addr().ok())
        .map_or(cli.port, |a| a.port());

    // Install the handlers before announcing readiness: a SIGINT that arrives right after the
    // banner must start a clean shutdown, not kill the process with the default action.
    let signals = match Signals::install() {
        Ok(signals) => signals,
        Err(err) => {
            eprintln!("ERROR: could not install signal handlers: {err}");
            return ExitCode::FAILURE;
        }
    };
    print_banner(&cli, port);

    let admin = hidane::Admin::default()
        .with_enterprise_edition(cli.database_edition == cli::DatabaseEdition::Enterprise);
    let (code_tx, code_rx) = tokio::sync::oneshot::channel();
    let shutdown = {
        let admin = admin.clone();
        async move {
            let code = tokio::select! {
                code = signals.wait() => code,
                // `POST /shutdown`: the official emulator exits 0.
                () = admin.shutdown_requested() => 0,
            };
            eprintln!("Shutting down...");
            let _ = code_tx.send(code);
        }
    };
    let grpc = hidane::grpc_routes(&admin);
    if let Err(err) = hidane::serve(listeners, grpc, hidane::http_routes(admin), shutdown).await {
        eprintln!("ERROR: server failed: {err}");
        return ExitCode::FAILURE;
    }
    // 130 = 128 + SIGINT, the code the JVM (and firebase-tools' check) uses.
    ExitCode::from(code_rx.await.unwrap_or(0))
}

/// The official emulator's startup banner. `Dev App Server is now running.` is kept verbatim:
/// CI scripts wait for that line.
fn print_banner(cli: &Cli, port: u16) {
    let host = if cli.host.contains(':') && !cli.host.starts_with('[') {
        format!("[{}]", cli.host)
    } else {
        cli.host.clone()
    };
    println!();
    println!("API endpoint: http://{host}:{port}");
    println!(
        "Database Edition: {}",
        cli::value_name(&cli.database_edition)
    );
    println!("Database Mode: {}", cli::value_name(&cli.database_mode));
    println!();
    println!(
        "If you are using a library that supports the FIRESTORE_EMULATOR_HOST environment variable, run:"
    );
    println!();
    println!("   export FIRESTORE_EMULATOR_HOST={host}:{port}");
    println!();
    println!("Dev App Server is now running.");
}

/// Shutdown signals, registered eagerly. Resolves to the exit code the JVM would use:
/// 130 for SIGINT, 143 for SIGTERM.
#[cfg(unix)]
struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn wait(mut self) -> u8 {
        tokio::select! {
            _ = self.interrupt.recv() => 130,
            _ = self.terminate.recv() => 143,
        }
    }
}

#[cfg(not(unix))]
struct Signals(tokio::signal::windows::CtrlC);

#[cfg(not(unix))]
impl Signals {
    fn install() -> std::io::Result<Self> {
        Ok(Self(tokio::signal::windows::ctrl_c()?))
    }

    async fn wait(mut self) -> u8 {
        let _ = self.0.recv().await;
        130
    }
}
