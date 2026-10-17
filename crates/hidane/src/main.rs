use std::{
    ffi::{OsStr, OsString},
    io::ErrorKind,
    process::ExitCode,
};

use clap::Parser;
use hidane::cli::{self, Cli};

mod bench;
mod gcloud;
mod launch;
mod project_cli;

fn main() -> ExitCode {
    match launch::mode(std::env::args_os().collect()) {
        launch::Mode::Exec(command) => launch::exec(&command),
        launch::Mode::Benchmark(args) => bench::main(args),
        launch::Mode::Project(args) => project_cli::run(args),
        // firebase-tools running "java" in a `hidane exec` command (ADR 0006).
        launch::Mode::Java(args) => match launch::java(&args) {
            Ok(flags) => emulator(
                std::iter::once(OsString::from("hidane"))
                    .chain(flags)
                    .collect(),
            ),
            Err(code) => code,
        },
        launch::Mode::Shell(script) => shell(&script),
        launch::Mode::Emulator(args) => emulator(args),
    }
}

/// `sh -c <script>`: the emulator, when the script is a `gcloud … emulators firestore start`
/// command line that hidane serves (docs/testcontainers.md).
fn shell(script: &OsStr) -> ExitCode {
    let flags = script
        .to_str()
        .ok_or_else(|| gcloud::Refusal::NotGcloud("the script is not UTF-8".into()))
        .and_then(gcloud::from_script);
    match flags {
        Ok(flags) => {
            eprintln!(
                "hidane: in place of `gcloud emulators firestore start`, with: hidane {}",
                flags.join(" ")
            );
            emulator(
                std::iter::once("hidane".to_owned())
                    .chain(flags)
                    .map(OsString::from)
                    .collect(),
            )
        }
        Err(refusal) => {
            eprintln!("ERROR: {}", refusal.message());
            ExitCode::from(refusal.exit_code())
        }
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
        emit(&cli::usage());
        return ExitCode::SUCCESS;
    }
    if cli.version {
        emit(&format!("hidane {}\n", env!("CARGO_PKG_VERSION")));
        return ExitCode::SUCCESS;
    }
    if cli.licenses {
        emit(&cli::licenses());
        return ExitCode::SUCCESS;
    }
    match cli.validate() {
        Ok(warnings) => {
            for warning in warnings {
                eprintln!("WARNING: {warning}");
            }
        }
        Err(err) => {
            // The official emulator's checks, which it exits from with code 1.
            eprintln!("ERROR: {err}");
            return ExitCode::from(1);
        }
    }
    if std::env::var("EXPERIMENTAL_MODE").is_ok_and(|v| v.eq_ignore_ascii_case("true")) {
        eprintln!("Emulator has been started in experimental mode!");
    }
    // Read before the port opens, as the official emulator does: a bad export stops the start.
    // In Datastore mode the export holds entities in that mode's format (#81).
    let datastore_mode = cli.database_mode() == cli::DatabaseMode::CloudFirestoreInDatastoreMode;
    let (seed, datastore_seed) = match cli.seed() {
        None => (None, None),
        Some(path) if datastore_mode => match hidane::export::read_datastore(path) {
            Ok(entities) => (None, Some(entities)),
            Err(message) => {
                eprintln!("ERROR: {message}");
                return ExitCode::FAILURE;
            }
        },
        Some(path) => match hidane::export::read(path) {
            Ok(documents) => (Some(documents), None),
            Err(message) => {
                eprintln!("ERROR: {message}");
                return ExitCode::FAILURE;
            }
        },
    };
    // `--rules`: the official emulator says nothing about the file; hidane prints what the
    // compiler found. A file that does not compile is not loaded and does not stop the start.
    let rules = cli.rules.as_deref().and_then(|path| {
        let (lines, ruleset) = hidane::rules::load(path);
        for line in lines {
            eprintln!("{line}");
        }
        ruleset
    });

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
    // `--websocket_port`: the feed of the Emulator UI's request monitor. The official emulator
    // starts even when it cannot bind it.
    let websocket = match cli.websocket_port {
        Some(websocket_port) => match hidane::bind(&cli.host, websocket_port).await {
            Ok(listeners) => Some(listeners),
            Err(err) => {
                eprintln!(
                    "WARNING: could not bind {}:{websocket_port} for the Emulator UI's request \
                     monitor: {err}",
                    cli.host
                );
                None
            }
        },
        None => None,
    };

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

    let mut admin = hidane::Admin::default()
        .with_enterprise_edition(cli.database_edition() == cli::DatabaseEdition::Enterprise)
        .with_firestore_index_enforcement(cli.require_indexes && !datastore_mode)
        .with_datastore_mode(datastore_mode);
    if cli.require_indexes {
        admin = admin.with_datastore_indexes(cli.index_file.clone());
    }
    if let Some(entities) = datastore_seed {
        admin = admin.with_datastore_seed(entities);
    }
    if let Some(documents) = seed {
        admin = admin.with_seed(documents);
    }
    // `Cli::validate` has made sure a project is named.
    if let (true, Some(project)) = (cli.single_project_mode, cli.project_id.as_deref()) {
        admin = admin.with_single_project(project, cli.single_project_mode_error);
    }
    if let (Some(path), Some((content, ruleset))) = (cli.rules.as_deref(), rules) {
        admin.rules().set_default(path, content, ruleset);
    }
    admin
        .triggers()
        .set_functions_emulator(cli.functions_emulator.clone());
    let feed = websocket.map(|listeners| {
        let monitor = hidane::rules::monitor::Monitor::new();
        admin
            .rules()
            .monitor_requests(std::sync::Arc::clone(&monitor));
        tokio::spawn(hidane::serve_websocket(listeners, monitor))
    });
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
    let served = hidane::serve(listeners, grpc, hidane::http_routes(admin), shutdown).await;
    if let Some(feed) = feed {
        feed.abort();
    }
    if let Err(err) = served {
        eprintln!("ERROR: server failed: {err}");
        return ExitCode::FAILURE;
    }
    // 130 = 128 + SIGINT, the code the JVM (and firebase-tools' check) uses.
    ExitCode::from(code_rx.await.unwrap_or(0))
}

/// Writes `text` to stdout and ends quietly when the reader has gone, as with
/// `hidane --licenses | head` (`print!` panics on a closed pipe).
fn emit(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
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
        cli::value_name(&cli.database_edition())
    );
    println!("Database Mode: {}", cli::value_name(&cli.database_mode()));
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
