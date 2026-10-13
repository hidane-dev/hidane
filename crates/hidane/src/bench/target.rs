//! The two targets and their processes. Both are started directly, the same way: a fresh
//! process on a free localhost port with `--host 127.0.0.1 --port <port>`, output discarded
//! (or kept with `--keep-logs`), stopped with `POST /shutdown`.

use std::{
    ffi::OsString,
    io::Read as _,
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{config, system};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Official,
    Hidane,
}

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Hidane => "hidane",
        }
    }
}

/// How to start one target.
#[derive(Debug, Clone)]
pub struct Target {
    pub kind: Kind,
    /// The hidane binary, or the official emulator's `java`.
    pub program: PathBuf,
    prefix: Vec<OsString>,
    /// The official emulator's jar.
    pub jar: Option<PathBuf>,
    /// `targets.<kind>` in the result.
    pub info: Value,
}

impl Target {
    pub fn hidane(
        path: &Path,
        commit: Option<&str>,
        dirty: bool,
        profile: Option<&str>,
    ) -> Result<Self, String> {
        let version = Command::new(path)
            .arg("--version")
            .output()
            .map_err(|e| format!("could not run hidane ({e})"))?;
        let version = String::from_utf8_lossy(&version.stdout);
        let version = version
            .trim()
            .strip_prefix("hidane ")
            .ok_or("hidane --version did not answer `hidane <version>`")?
            .to_owned();
        Ok(Self {
            kind: Kind::Hidane,
            program: path.to_owned(),
            prefix: Vec::new(),
            jar: None,
            info: json!({
                "version": version,
                "git_commit": commit,
                "git_dirty": commit.map(|_| dirty),
                "build_profile": profile,
                "binary_sha256": sha256_file(path)?,
                "command": "hidane --host 127.0.0.1 --port <port>",
            }),
        })
    }

    pub fn official(java: &Java, jar: &Path, verified: bool, sha256: &str) -> Result<Self, String> {
        let mut prefix: Vec<OsString> = config::JAVA_OPTIONS.iter().map(OsString::from).collect();
        prefix.push("-jar".into());
        prefix.push(jar.as_os_str().to_owned());
        let output = Command::new(&java.path)
            .args(&prefix)
            .arg("--version")
            .output()
            .map_err(|e| format!("could not run java ({e})"))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let version = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("cloud-firestore-emulator "))
            .ok_or("the jar's --version did not answer `cloud-firestore-emulator <version>`")?
            .to_owned();
        Ok(Self {
            kind: Kind::Official,
            program: java.path.clone(),
            prefix,
            jar: Some(jar.to_owned()),
            info: json!({
                "version": version,
                "jar_sha256": sha256,
                "jar_verified": verified,
                "java": java.version,
                "java_vendor": java.vendor,
                "java_options": config::JAVA_OPTIONS,
                "command": format!(
                    "java {} -jar cloud-firestore-emulator-v{version}.jar --host 127.0.0.1 --port <port>",
                    config::JAVA_OPTIONS.join(" ")
                ),
            }),
        })
    }

    /// Starts a fresh process on a free port; the clock starts just before the spawn.
    pub fn spawn(&self, logs: Option<&Path>, label: &str) -> Result<Running, String> {
        let port = free_port()?;
        let mut command = Command::new(&self.program);
        command
            .args(&self.prefix)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .stdin(Stdio::null());
        match logs {
            Some(dir) => {
                let file = std::fs::File::create(dir.join(format!("{label}.log")))
                    .map_err(|e| format!("could not create a log file ({e})"))?;
                let err = file
                    .try_clone()
                    .map_err(|e| format!("could not create a log file ({e})"))?;
                command.stdout(file).stderr(err);
            }
            None => {
                command.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        let started = Instant::now();
        let child = command
            .spawn()
            .map_err(|e| format!("could not start the {} target ({e})", self.kind.id()))?;
        Ok(Running {
            child: Some(child),
            port,
            started,
        })
    }
}

/// A free port on 127.0.0.1, as the OS assigns it.
pub fn free_port() -> Result<u16, String> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("no free port ({e})"))
}

/// A started target. Dropping it kills the process, so nothing outlives the benchmark.
pub struct Running {
    child: Option<Child>,
    pub port: u16,
    pub started: Instant,
}

const READY_TIMEOUT: Duration = Duration::from_secs(120);
const EXIT_TIMEOUT: Duration = Duration::from_secs(20);

impl Running {
    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// Polls the port until a TCP connection is accepted, as firebase-tools does; returns the
    /// time since the spawn.
    pub fn wait_port(&mut self) -> Result<Duration, String> {
        let addr = self.addr();
        loop {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
                return Ok(self.started.elapsed());
            }
            self.check_alive()?;
            if self.started.elapsed() > READY_TIMEOUT {
                return Err(format!(
                    "the port did not accept within {} s",
                    READY_TIMEOUT.as_secs()
                ));
            }
            thread::sleep(Duration::from_micros(500));
        }
    }

    pub fn check_alive(&mut self) -> Result<(), String> {
        let child = self.child.as_mut().ok_or("the process is gone")?;
        match child.try_wait() {
            Ok(Some(status)) => Err(format!(
                "the process exited ({status}) (rerun with --keep-logs DIR to keep its output)"
            )),
            Ok(None) => Ok(()),
            Err(e) => Err(format!("could not check the process ({e})")),
        }
    }

    /// `POST /shutdown`, then waits for the process to exit (killing it after 20 s). Returns the
    /// time from the request to the exit.
    pub fn shutdown(mut self) -> Result<Duration, String> {
        let asked = Instant::now();
        let _ = system::http(
            self.addr(),
            "POST",
            "/shutdown",
            &[],
            Duration::from_secs(10),
        );
        let Some(mut child) = self.child.take() else {
            return Err("the process is gone".into());
        };
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(asked.elapsed()),
                Ok(None) if asked.elapsed() > EXIT_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "did not exit within {} s of POST /shutdown; killed",
                        EXIT_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(1)),
                Err(e) => return Err(format!("could not wait for the process ({e})")),
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The Java that runs the official emulator.
#[derive(Debug, Clone)]
pub struct Java {
    /// `<java.home>/bin/java`: version managers' shims (mise, asdf, jenv, SDKMAN!) are resolved
    /// once here, so their own start-up is not counted in the emulator's.
    pub path: PathBuf,
    pub version: String,
    pub vendor: Option<String>,
}

pub fn java(explicit: Option<&Path>) -> Result<Java, String> {
    let program = explicit.map_or_else(|| PathBuf::from("java"), Path::to_owned);
    let output = Command::new(&program)
        .args(["-XshowSettings:properties", "-version"])
        .env("MISE_OFFLINE", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            format!("no Java found ({e}); the official emulator needs Java 21 or newer")
        })?;
    let text = String::from_utf8_lossy(&output.stderr);
    let property = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_owned())
        })
    };
    let version = property("java.version").ok_or("could not read java.version from java")?;
    let major: u32 = version
        .split(['.', '-', '+'])
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    if major < 21 {
        return Err(format!(
            "Java {version} is too old: the official emulator v{} needs Java 21 or newer",
            config::OFFICIAL_VERSION
        ));
    }
    let resolved = property("java.home")
        .map(|home| {
            Path::new(&home)
                .join("bin")
                .join(if cfg!(windows) { "java.exe" } else { "java" })
        })
        .filter(|p| p.is_file())
        .unwrap_or(program);
    Ok(Java {
        path: resolved,
        version,
        vendor: property("java.vendor"),
    })
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("could not read a file to hash ({e})"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("could not read a file to hash ({e})"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The cache directory for the downloaded jar: `$XDG_CACHE_HOME/hidane-bench`, else
/// `~/.cache/hidane-bench`.
pub fn default_cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|d| d.join("hidane-bench"))
}

/// The pinned jar in `cache`, downloaded with `curl` from the URL firebase-tools uses when it
/// is not there yet, and checked against the pinned SHA-256 before it is used.
pub fn download_official(cache: &Path) -> Result<PathBuf, String> {
    let jar = cache.join(config::OFFICIAL_JAR_NAME);
    if jar.is_file() && sha256_file(&jar)? == config::OFFICIAL_SHA256 {
        return Ok(jar);
    }
    std::fs::create_dir_all(cache)
        .map_err(|e| format!("could not create the cache directory ({e})"))?;
    let partial = cache.join(format!("{}.part", config::OFFICIAL_JAR_NAME));
    eprintln!(
        "hidane benchmark: downloading the official emulator v{} ({} MB)",
        config::OFFICIAL_VERSION,
        config::OFFICIAL_SIZE / 1_000_000
    );
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&partial)
        .arg(config::OFFICIAL_URL)
        .status()
        .map_err(|e| format!("could not run curl to download the jar ({e})"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("downloading the jar failed ({status})"));
    }
    let hash = sha256_file(&partial)?;
    if hash != config::OFFICIAL_SHA256 {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "the downloaded jar's SHA-256 is {hash}, not the pinned {}; not used",
            config::OFFICIAL_SHA256
        ));
    }
    std::fs::rename(&partial, &jar).map_err(|e| format!("could not store the jar ({e})"))?;
    Ok(jar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_files() {
        let dir = std::env::temp_dir().join(format!("hidane-bench-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abc");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
