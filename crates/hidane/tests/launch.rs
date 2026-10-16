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

/// A directory for `PATH`, empty or holding a stand-in `java` that prints `java_says` and its
/// arguments, like a real Java would print its version on stderr.
fn path_dir(name: &str, java_says: Option<&str>) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hidane-launch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(says) = java_says {
        use std::os::unix::fs::PermissionsExt as _;
        let java = dir.join("java");
        std::fs::write(&java, format!("#!/bin/sh\necho '{says}' \"$@\" >&2\n")).unwrap();
        std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// `hidane exec -- /bin/sh -c <script>` with `PATH` set to `dir`; its exit code and output.
fn exec(dir: &std::path::Path, script: &str) -> (i32, String) {
    let output = Command::new(BIN)
        .args(["exec", "--", "/bin/sh", "-c", script])
        .env("PATH", dir)
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
        &path_dir("layout", None),
        r#"command -v java; echo "${FIRESTORE_EMULATOR_BINARY_PATH##*/}"; test -f "$FIRESTORE_EMULATOR_BINARY_PATH" && echo exists"#,
    );
    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].ends_with("/java"), "{out}");
    assert_eq!(lines[1..], ["hidane-firestore.jar", "exists"]);
}

/// The probe firebase-tools runs before starting a JVM emulator.
const PROBE: &str = "java -Duser.language=en -Dfile.encoding=UTF-8 -version";

#[test]
fn the_version_probe_passes_without_a_jdk() {
    let (code, out) = exec(&path_dir("no-java", None), PROBE);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(r#"version "21"#), "{out}");
}

#[test]
fn a_recent_java_answers_the_probe_itself_and_an_old_one_does_not() {
    let (code, out) = exec(
        &path_dir("java25", Some(r#"openjdk version "25.0.1""#)),
        PROBE,
    );
    assert_eq!(
        (code, out.trim()),
        (
            0,
            r#"openjdk version "25.0.1" -Duser.language=en -Dfile.encoding=UTF-8 -version"#
        )
    );
    let (code, out) = exec(
        &path_dir("java17", Some(r#"openjdk version "17.0.9""#)),
        PROBE,
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(r#"version "21""#) && !out.contains("17.0.9"),
        "{out}"
    );
}

/// Like a version manager's shim (mise, asdf) with no Java behind it: it runs the first `java`
/// on `PATH`, which is hidane's again. It gives up after a few rounds, so a regression fails
/// instead of spawning processes forever.
fn version_manager_dir(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = path_dir(name, None);
    let java = dir.join("java");
    std::fs::write(
        &java,
        "#!/bin/sh\nROUNDS=$((${ROUNDS:-0} + 1)); export ROUNDS\n[ \"$ROUNDS\" -gt 5 ] && { echo looped >&2; exit 9; }\nexec java \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

#[test]
fn a_version_manager_shim_does_not_send_the_probe_round_in_circles() {
    let (code, out) = exec(&version_manager_dir("manager-probe"), PROBE);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(r#"version "21""#) && !out.contains("looped"),
        "{out}"
    );
    let (code, out) = exec(
        &version_manager_dir("manager-jar"),
        "java -jar /x/firebase-database-emulator.jar",
    );
    assert_eq!(code, 127, "{out}");
    assert!(out.contains("leads back to hidane"), "{out}");
}

#[test]
fn other_jars_go_to_the_real_java() {
    let (code, out) = exec(
        &path_dir("forward", Some("real java")),
        "java -Duser.language=en -jar /x/firebase-database-emulator.jar --port 9000",
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        out.trim(),
        "real java -Duser.language=en -jar /x/firebase-database-emulator.jar --port 9000"
    );
}

#[test]
fn other_jars_need_a_real_java() {
    let (code, out) = exec(
        &path_dir("missing", None),
        "java -jar /x/firebase-database-emulator.jar",
    );
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
        .env("PATH", path_dir("serve", None))
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

/// What Testcontainers' gcloud modules start hidane's container image with, after its
/// entrypoint: `/bin/sh -c "gcloud beta emulators firestore start --host-port …"`
/// (docs/testcontainers.md). hidane reads the gcloud command line itself.
#[test]
fn a_gcloud_command_line_after_sh_c_runs_the_emulator() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let script = format!(
        "gcloud beta emulators firestore start --host-port 127.0.0.1:{port} --project=demo-hidane"
    );
    let mut child = Command::new(BIN)
        .args(["/bin/sh", "-c", &script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The line the modules wait for, from the banner.
    let mut stdout = child.stdout.take().unwrap();
    let mut seen = String::new();
    let mut buf = [0; 256];
    while !seen.contains("Dev App Server is now running.") {
        let n = stdout.read(&mut buf).unwrap();
        assert!(n > 0, "no banner: {seen}");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert!(
        seen.contains(&format!("API endpoint: http://127.0.0.1:{port}")),
        "{seen}"
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(b"POST /shutdown HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(
        stderr.contains(&format!(
            "in place of `gcloud emulators firestore start`, with: hidane --host 127.0.0.1 --port {port}"
        )),
        "{stderr}"
    );
}

/// hidane's image has no shell: a script that is not a gcloud command line it serves stops
/// with the reason, and a flag hidane rejects stops it as it would stop hidane.
#[test]
fn other_scripts_and_unserved_flags_are_refused() {
    for (script, code, says) in [
        ("echo hello", 127, "hidane's image has no shell"),
        (
            "gcloud beta emulators firestore start | tee log",
            127,
            "`|` would need a shell",
        ),
        (
            "gcloud beta emulators firestore start --store-on-disk",
            2,
            "`--store-on-disk` is not a flag",
        ),
        (
            "gcloud beta emulators firestore start --database-mode=datastore-mode --require-indexes",
            2,
            "hidane does not enforce indexes in Datastore mode yet",
        ),
    ] {
        let output = Command::new(BIN)
            .args(["/bin/sh", "-c", script])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{script}: {stderr}");
        assert!(stderr.contains(says), "{script}: {stderr}");
        assert!(!stderr.contains("running"), "{script}: {stderr}");
    }
}
