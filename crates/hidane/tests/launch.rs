//! `hidane exec -- <command>`: hidane standing in for the official emulator under firebase-tools
//! (ADR 0006). The command sees a `java` that is hidane and a placeholder jar, as firebase-tools
//! would run them: `java -version`, then `java <-D…> -jar <jar> <flags>`.
#![cfg(unix)]

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_hidane");

/// `hidane exec -- /bin/sh -c <script>` with `PATH` set to `path`; its exit code and output.
fn exec(path: &str, script: &str) -> (i32, String) {
    let output = Command::new(BIN)
        .args(["exec", "--", "/bin/sh", "-c", script])
        .env("PATH", path)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

#[test]
fn java_is_hidane_and_the_jar_is_a_placeholder() {
    let (code, out) = exec(
        "/bin",
        r#"command -v java; echo "${FIRESTORE_EMULATOR_BINARY_PATH##*/}"; test -f "$FIRESTORE_EMULATOR_BINARY_PATH" && echo exists"#,
    );
    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].ends_with("/java"), "{out}");
    assert_eq!(lines[1..], ["hidane-firestore.jar", "exists"]);
}

#[test]
fn the_version_probe_passes_without_a_jdk() {
    // The probe firebase-tools runs before starting a JVM emulator; /bin has no java.
    let (code, out) = exec(
        "/bin",
        "java -Duser.language=en -Dfile.encoding=UTF-8 -version",
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(r#"version "21"#), "{out}");
}

#[test]
fn other_jars_need_a_real_java() {
    let (code, out) = exec("/bin", "java -jar /x/firebase-database-emulator.jar");
    assert_eq!(code, 127, "{out}");
    assert!(out.contains("no java other than hidane's"), "{out}");
}

#[test]
fn without_a_command_exec_explains_itself() {
    let output = Command::new(BIN).arg("exec").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("hidane exec"));
}

#[test]
fn the_jar_runs_the_emulator_and_exec_ends_with_it() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // What firebase-tools 15.33.0 runs for the Firestore emulator.
    let script = format!(
        r#"exec java -Dgoogle.cloud_firestore.debug_log_level=FINE -Duser.language=en -jar "$FIRESTORE_EMULATOR_BINARY_PATH" --host 127.0.0.1 --port {port} --websocket_port 0 --project_id demo-hidane --single_project_mode true"#
    );
    let mut child = Command::new(BIN)
        .args(["exec", "--", "/bin/sh", "-c", &script])
        .env("PATH", "/bin")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(_) if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => panic!("the emulator did not start: {err}"),
        }
    };
    stream
        .write_all(b"POST /shutdown HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.ends_with("Shutting down...\n"), "{answer}");
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "exec ends with the command's code");
}
