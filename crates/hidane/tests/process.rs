//! The process contract firebase-tools relies on (#12), checked against the real binary:
//! it polls the port until it accepts, sends SIGINT to stop the emulator and waits 4 s,
//! treats exit codes other than 0 / 130 as fatal, and looks for "address already in use" on
//! stderr to explain port conflicts.
#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_hidane");

fn spawn(args: &[&str]) -> Child {
    Command::new(BIN)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hidane")
}

/// Reads stdout until the official completion line and returns every line seen.
fn wait_for_banner(child: &mut Child) -> Vec<String> {
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut lines = Vec::new();
    while let Ok(line) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        let done = line == "Dev App Server is now running.";
        lines.push(line);
        if done {
            return lines;
        }
    }
    panic!("no completion line; stdout so far: {lines:#?}");
}

fn port_from(lines: &[String]) -> u16 {
    let endpoint = lines
        .iter()
        .find_map(|l| l.strip_prefix("API endpoint: http://"))
        .expect("API endpoint line");
    endpoint.rsplit(':').next().unwrap().parse().unwrap()
}

fn signal(child: &Child, sig: &str) {
    let status = Command::new("kill")
        .args([sig, &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("process did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn stderr_of(child: &mut Child) -> String {
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    stderr
}

#[test]
fn starts_with_the_firebase_tools_arguments_and_stops_on_sigint_with_130() {
    let rules = std::env::temp_dir().join(format!("hidane-test-{}.rules", std::process::id()));
    std::fs::write(&rules, "rules_version = '2';").unwrap();
    let mut child = spawn(&[
        "--host",
        "127.0.0.1",
        "--port",
        "0",
        "--websocket_port",
        "0",
        "--database-edition",
        "standard",
        "--project_id",
        "demo-hidane",
        "--rules",
        rules.to_str().unwrap(),
        "--single_project_mode",
        "true",
        "--functions_emulator",
        "127.0.0.1:5001",
    ]);
    let lines = wait_for_banner(&mut child);
    assert!(lines.iter().any(|l| l == "Database Edition: STANDARD"));
    assert!(lines.iter().any(|l| l == "Database Mode: CLOUD_FIRESTORE"));
    let port = port_from(&lines);
    assert!(
        lines
            .iter()
            .any(|l| l == &format!("   export FIRESTORE_EMULATOR_HOST=127.0.0.1:{port}"))
    );
    // What firebase-tools does to detect startup: a plain TCP connect.
    TcpStream::connect(("127.0.0.1", port)).expect("port accepts after the banner");

    signal(&child, "-INT");
    let status = wait_with_timeout(&mut child, Duration::from_secs(4));
    assert_eq!(status.code(), Some(130));
    let stderr = stderr_of(&mut child);
    assert!(
        stderr.contains("WARNING: Security Rules are not evaluated yet"),
        "{stderr}"
    );
    let _ = std::fs::remove_file(rules);
}

#[test]
fn sigterm_exits_with_143() {
    let mut child = spawn(&["--host", "127.0.0.1", "--port", "0"]);
    wait_for_banner(&mut child);
    signal(&child, "-TERM");
    let status = wait_with_timeout(&mut child, Duration::from_secs(4));
    assert_eq!(status.code(), Some(143));
}

#[test]
fn port_conflicts_use_the_wording_firebase_tools_looks_for() {
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let mut child = spawn(&["--host", "127.0.0.1", "--port", &port]);
    let status = wait_with_timeout(&mut child, Duration::from_secs(4));
    assert!(!status.success());
    let stderr = stderr_of(&mut child);
    assert!(stderr.contains("address already in use"), "{stderr}");
}

#[test]
fn localhost_is_reachable_over_ipv4() {
    let mut child = spawn(&["--host", "localhost", "--port", "0"]);
    let lines = wait_for_banner(&mut child);
    let port = port_from(&lines);
    TcpStream::connect(("127.0.0.1", port)).expect("127.0.0.1 accepts for --host localhost");
    signal(&child, "-INT");
    wait_with_timeout(&mut child, Duration::from_secs(4));
}

#[test]
fn unknown_flags_exit_with_an_error() {
    let mut child = spawn(&["--no-such-flag"]);
    let status = wait_with_timeout(&mut child, Duration::from_secs(4));
    assert_eq!(status.code(), Some(2));
}

#[test]
fn help_and_version_exit_zero() {
    let help = Command::new(BIN).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("Usage: hidane [options] <ignored>"));
    let version = Command::new(BIN).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("hidane "));
}
