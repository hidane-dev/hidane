//! Running hidane where firebase-tools expects the official emulator (ADR 0006).
//!
//! firebase-tools starts the Firestore emulator as `java <-D options> -jar <jar> <flags>`, after
//! checking that `java -version` reports 21 or newer. `hidane exec -- <command>` runs a command
//! with a directory first on its `PATH` in which `java` is this binary, and with
//! `FIRESTORE_EMULATOR_BINARY_PATH` naming a placeholder there, so firebase-tools downloads no
//! jar. Invoked as `java`, hidane then:
//!
//! - serves the emulator itself when the jar is that placeholder (or the official jar), with
//!   the flags after the jar;
//! - answers `-version` with the real Java's answer when one of 21 or newer is installed, and
//!   with a Java 21 version line otherwise, so the check passes without a JDK;
//! - hands anything else (the other emulators' jars) to the next `java` on `PATH`.
//!
//! The user's own `PATH` is not touched, and the directory is removed when the command ends.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::ExitCode,
};

/// The placeholder `FIRESTORE_EMULATOR_BINARY_PATH` names.
const PLACEHOLDER: &str = "hidane-firestore.jar";
/// The directory `hidane exec` puts first on `PATH`, so the shim can skip itself.
const SHIM_DIR_ENV: &str = "HIDANE_SHIM_DIR";
const MIN_JAVA: u32 = 21;

/// What a run of this binary is.
pub enum Mode {
    /// `hidane exec [--] <command…>`.
    Exec(Vec<OsString>),
    /// Invoked as `java`, with its arguments.
    Java(Vec<OsString>),
    /// The emulator, with its own arguments.
    Emulator(Vec<OsString>),
}

pub fn mode(args: Vec<OsString>) -> Mode {
    let invoked_as = args
        .first()
        .and_then(|a| Path::new(a).file_stem())
        .map(OsStr::to_os_string);
    if invoked_as.as_deref() == Some(OsStr::new("java")) {
        return Mode::Java(args.into_iter().skip(1).collect());
    }
    if args.get(1).map(OsString::as_os_str) == Some(OsStr::new("exec")) {
        let mut command: Vec<OsString> = args.into_iter().skip(2).collect();
        if command.first().map(OsString::as_os_str) == Some(OsStr::new("--")) {
            command.remove(0);
        }
        return Mode::Exec(command);
    }
    Mode::Emulator(args)
}

/// What to do when invoked as `java`.
#[derive(Debug, PartialEq, Eq)]
pub enum Java {
    /// Serve the emulator with these flags.
    Emulator(Vec<OsString>),
    /// Answer the version probe.
    Version,
    /// Run the real `java`.
    Forward,
}

pub fn classify(args: &[OsString]) -> Java {
    if let Some(at) = args.iter().position(|a| a == "-jar") {
        let Some(jar) = args.get(at + 1) else {
            return Java::Forward;
        };
        let name = Path::new(jar)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == PLACEHOLDER || name.starts_with("cloud-firestore-emulator") {
            return Java::Emulator(args[at + 2..].to_vec());
        }
        return Java::Forward;
    }
    if args.iter().any(|a| a == "-version" || a == "--version") {
        return Java::Version;
    }
    Java::Forward
}

/// The first `java` on `PATH` that is not this shim.
fn real_java() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?.canonicalize().ok()?;
    let shim_dir = std::env::var_os(SHIM_DIR_ENV).map(PathBuf::from);
    let name = if cfg!(windows) { "java.exe" } else { "java" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| shim_dir.as_deref() != Some(dir.as_path()))
        .map(|dir| dir.join(name))
        .find(|candidate| {
            candidate.is_file() && candidate.canonicalize().is_ok_and(|real| real != me)
        })
}

/// The major version in `java -version` output (`version "21.0.1"`, `version "1.8.0"`).
pub fn java_major(output: &str) -> Option<u32> {
    let rest = &output[output.find("version \"")? + 9..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    match digits.parse().ok()? {
        1 => rest[2..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok(),
        major => Some(major),
    }
}

/// Runs as `java`. `Some(flags)` means: serve the emulator with them.
pub fn java(args: &[OsString]) -> Result<Vec<OsString>, ExitCode> {
    match classify(args) {
        Java::Emulator(flags) => Ok(flags),
        Java::Version => {
            if let Some(java) = real_java()
                && let Ok(output) = std::process::Command::new(&java).args(args).output()
            {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                if java_major(&text).is_some_and(|major| major >= MIN_JAVA) {
                    use std::io::Write as _;
                    let _ = std::io::stdout().write_all(&output.stdout);
                    let _ = std::io::stderr().write_all(&output.stderr);
                    return Err(ExitCode::from(
                        u8::try_from(output.status.code().unwrap_or(1)).unwrap_or(1),
                    ));
                }
            }
            // Java prints its version on stderr.
            eprintln!("openjdk version \"{MIN_JAVA}\" (hidane: no JDK needed for Firestore)");
            Err(ExitCode::SUCCESS)
        }
        Java::Forward => Err(forward(args)),
    }
}

fn forward(args: &[OsString]) -> ExitCode {
    let Some(java) = real_java() else {
        eprintln!(
            "hidane: this command needs Java, and no java other than hidane's is on PATH \
             (hidane serves only the Firestore emulator)"
        );
        return ExitCode::from(127);
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let err = std::process::Command::new(&java).args(args).exec();
        eprintln!("hidane: could not run {}: {err}", java.display());
        ExitCode::from(126)
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&java).args(args).status() {
            Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
            Err(err) => {
                eprintln!("hidane: could not run {}: {err}", java.display());
                ExitCode::from(126)
            }
        }
    }
}

/// `hidane exec [--] <command…>`: runs the command with hidane standing in for the official
/// emulator, and exits with its code.
pub fn exec(command: &[OsString]) -> ExitCode {
    let Some((program, args)) = command.split_first() else {
        eprintln!("usage: hidane exec [--] <command> [args…]");
        eprintln!("       hidane exec -- firebase emulators:start --only firestore");
        return ExitCode::from(2);
    };
    let dir = std::env::temp_dir().join(format!("hidane-shim-{}", std::process::id()));
    let prepared = prepare(&dir);
    let status = match prepared {
        Ok(()) => run(&dir, program, args),
        Err(err) => {
            eprintln!("hidane: could not prepare {}: {err}", dir.display());
            Err(())
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    status.unwrap_or(ExitCode::FAILURE)
}

fn prepare(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let me = std::env::current_exe()?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&me, dir.join("java"))?;
    #[cfg(not(unix))]
    std::fs::copy(&me, dir.join("java.exe")).map(|_| ())?;
    // firebase-tools checks the jar exists (and makes it executable) before running it.
    std::fs::write(dir.join(PLACEHOLDER), b"")
}

fn run(dir: &Path, program: &OsString, args: &[OsString]) -> Result<ExitCode, ()> {
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).map_err(|err| {
        eprintln!("hidane: could not build PATH: {err}");
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| eprintln!("hidane: {err}"))?;
    runtime.block_on(async {
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .env("PATH", path)
            .env("FIRESTORE_EMULATOR_BINARY_PATH", dir.join(PLACEHOLDER))
            .env(SHIM_DIR_ENV, dir)
            .spawn()
            .map_err(|err| {
                eprintln!("hidane: could not run {}: {err}", program.to_string_lossy())
            })?;
        // Ctrl-C reaches the whole process group; the command shuts its emulators down and
        // hidane waits for it, then removes the directory.
        let mut interrupts = Interrupts::install();
        let status = loop {
            tokio::select! {
                status = child.wait() => break status,
                () = interrupts.next() => {}
            }
        }
        .map_err(|err| eprintln!("hidane: {err}"))?;
        Ok(exit_code(status))
    })
}

fn exit_code(status: std::process::ExitStatus) -> ExitCode {
    if let Some(code) = status.code() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return ExitCode::from(u8::try_from(128 + signal).unwrap_or(1));
        }
    }
    ExitCode::FAILURE
}

/// Interrupts received while the command runs, which hidane outlives.
struct Interrupts {
    #[cfg(unix)]
    signals: Option<(tokio::signal::unix::Signal, tokio::signal::unix::Signal)>,
}

impl Interrupts {
    fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                signals: signal(SignalKind::interrupt())
                    .and_then(|i| Ok((i, signal(SignalKind::terminate())?)))
                    .ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    async fn next(&mut self) {
        #[cfg(unix)]
        if let Some((interrupt, terminate)) = &mut self.signals {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        #[cfg(unix)]
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_firestore_jar_runs_hidane_with_the_flags_after_it() {
        // What firebase-tools 15.33.0 runs.
        let firebase = args(&[
            "-Dgoogle.cloud_firestore.debug_log_level=FINE",
            "-Duser.language=en",
            "-jar",
            "/tmp/hidane-shim-1/hidane-firestore.jar",
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
        ]);
        assert_eq!(
            classify(&firebase),
            Java::Emulator(args(&["--host", "127.0.0.1", "--port", "8080"]))
        );
        let official = args(&[
            "-jar",
            "/cache/cloud-firestore-emulator-v1.22.0.jar",
            "--port",
            "1",
        ]);
        assert_eq!(classify(&official), Java::Emulator(args(&["--port", "1"])));
    }

    #[test]
    fn other_jars_and_the_version_probe() {
        assert_eq!(
            classify(&args(&[
                "-Duser.language=en",
                "-jar",
                "/cache/firebase-database-emulator-v4.11.2.jar"
            ])),
            Java::Forward
        );
        assert_eq!(
            classify(&args(&[
                "-Duser.language=en",
                "-Dfile.encoding=UTF-8",
                "-version"
            ])),
            Java::Version
        );
        assert_eq!(classify(&args(&["-cp", "x", "Main"])), Java::Forward);
    }

    #[test]
    fn java_major_versions() {
        assert_eq!(
            java_major("openjdk version \"24.0.2\" 2025-07-15"),
            Some(24)
        );
        assert_eq!(java_major("java version \"1.8.0_381\""), Some(8));
        assert_eq!(java_major("openjdk version \"21\" (hidane)"), Some(21));
        assert_eq!(java_major("nothing"), None);
    }

    #[test]
    fn modes() {
        assert!(
            matches!(mode(args(&["/x/java", "-version"])), Mode::Java(a) if a == args(&["-version"]))
        );
        assert!(
            matches!(mode(args(&["hidane", "exec", "--", "firebase"])), Mode::Exec(a) if a == args(&["firebase"]))
        );
        assert!(matches!(
            mode(args(&["hidane", "--port", "1"])),
            Mode::Emulator(_)
        ));
    }
}
