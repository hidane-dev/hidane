//! What `hidane report` reads from the machine, into an [`Input`]: the environment, the paths
//! and names it must redact, the OS version, what `java -version` and `node --version` say, the
//! firebase-tools on `PATH` (from its `package.json`, without running it: its update check would
//! go to the network), and the file names in firebase-tools' emulator cache. Nothing is sent
//! anywhere, and no file in the cache is opened.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use super::Input;

pub fn read(root: &Path) -> Input {
    let env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| {
            Some((
                name.into_string().ok()?,
                value.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    let var = |name: &str| {
        env.iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    let home = var("HOME")
        .or_else(|| var("USERPROFILE"))
        .map(PathBuf::from);
    let mut users: Vec<String> = ["USER", "USERNAME", "LOGNAME"]
        .into_iter()
        .filter_map(var)
        .collect();
    users.extend(
        home.as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned()),
    );
    let mut host_names: Vec<String> = ["HOSTNAME", "COMPUTERNAME"]
        .into_iter()
        .filter_map(var)
        .collect();
    if host_names.is_empty() {
        host_names.extend(
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .or_else(|| probe("hostname", &[]).map(|(_, text)| text))
                .as_deref()
                .and_then(clean),
        );
    }
    let cache_dir = var("FIREBASE_EMULATORS_PATH")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".cache/firebase/emulators")));
    let cache_entries = cache_dir.as_deref().and_then(|dir| {
        let entries = std::fs::read_dir(dir).ok()?;
        Some(
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
        )
    });
    let (firebase_on_path, firebase_tools) = firebase_tools();
    Input {
        roots: forms(Some(root)),
        homes: forms(home.as_deref()),
        temps: forms(Some(&std::env::temp_dir())),
        users,
        host_names,
        os_version: os_version(),
        container: container(&env),
        java: probe("java", &["-version"]).map(|(_, text)| text),
        node: probe("node", &["--version"])
            .filter(|(ok, _)| *ok)
            .and_then(|(_, text)| clean(&text)),
        firebase_on_path,
        firebase_tools,
        cache_dir: cache_dir.map(|d| d.display().to_string()),
        cache_entries,
        env,
    }
}

/// A path as given, made absolute, and with links resolved (`/var` is `/private/var` on
/// macOS), so that the redactor knows every form it may be printed in.
fn forms(path: Option<&Path>) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    let mut forms = vec![path.display().to_string()];
    if let Ok(absolute) = std::path::absolute(path) {
        forms.push(absolute.display().to_string());
    }
    if let Ok(real) = path.canonicalize() {
        let real = real.display().to_string();
        // Windows' verbatim prefix.
        forms.push(real.strip_prefix(r"\\?\").unwrap_or(&real).to_owned());
    }
    forms.dedup();
    forms
}

/// Runs a probe with nothing on stdin and returns whether it succeeded and what it printed
/// (stdout, then stderr). Version managers' shims are asked not to install or look anything
/// up over the network. `None` when it could not be run at all.
fn probe(program: &str, args: &[&str]) -> Option<(bool, String)> {
    let output = Command::new(program)
        .args(args)
        .env("MISE_OFFLINE", "1")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some((output.status.success(), text))
}

/// The first line, when it is short and printable.
pub fn clean(value: &str) -> Option<String> {
    let line = value.lines().next()?.trim();
    (!line.is_empty() && line.len() <= 200 && !line.chars().any(char::is_control))
        .then(|| line.to_owned())
}

fn os_version() -> Option<String> {
    if cfg!(target_os = "macos") {
        return probe("sw_vers", &["-productVersion"])
            .and_then(|(_, text)| clean(&text))
            .map(|v| format!("macOS {v}"));
    }
    if cfg!(windows) {
        // "Microsoft Windows [Version 10.0.26100.4652]"
        let (_, text) = probe("cmd", &["/C", "ver"])?;
        let version = text.split("[Version ").nth(1)?.split(']').next()?;
        return clean(version).map(|v| format!("Windows {v}"));
    }
    let release = std::fs::read_to_string("/etc/os-release").ok()?;
    let name = release.lines().find_map(|l| {
        let value = l.strip_prefix("PRETTY_NAME=")?;
        clean(value.trim_matches('"'))
    })?;
    let wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|r| r.to_ascii_lowercase().contains("microsoft"));
    Some(if wsl { format!("{name} (WSL)") } else { name })
}

fn container(env: &[(String, String)]) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    if Path::new("/.dockerenv").exists() {
        Some("docker".to_owned())
    } else if Path::new("/run/.containerenv").exists() {
        Some("podman".to_owned())
    } else if env.iter().any(|(n, _)| n == "KUBERNETES_SERVICE_HOST") {
        Some("kubernetes".to_owned())
    } else {
        None
    }
}

/// Whether `firebase` is on `PATH`, and its version from the `package.json` of the
/// firebase-tools it belongs to: above it once links are resolved (npm on macOS and Linux), or
/// in `node_modules` next to it (npm on Windows).
fn firebase_tools() -> (bool, Option<String>) {
    let Some(paths) = std::env::var_os("PATH") else {
        return (false, None);
    };
    let names: &[&str] = if cfg!(windows) {
        &["firebase.cmd", "firebase.exe", "firebase"]
    } else {
        &["firebase"]
    };
    let Some(firebase) = std::env::split_paths(&paths)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
    else {
        return (false, None);
    };
    let real = firebase.canonicalize().unwrap_or(firebase);
    let mut dir = real.parent();
    while let Some(d) = dir {
        for candidate in [
            d.join("package.json"),
            d.join("node_modules/firebase-tools/package.json"),
        ] {
            if let Ok(text) = std::fs::read_to_string(&candidate)
                && let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text)
                && pkg["name"] == "firebase-tools"
            {
                return (true, pkg["version"].as_str().and_then(clean));
            }
        }
        dir = d.parent();
    }
    (true, None)
}
