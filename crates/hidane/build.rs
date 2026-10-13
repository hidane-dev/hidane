//! Embeds the third-party notices into release builds, for `--licenses`: the release workflow
//! writes them with cargo-about (`about.toml`, `about.hbs`) and points
//! `HIDANE_THIRD_PARTY_LICENSES` at the file. Other builds say where to find them.
//!
//! Also records what `hidane report` says about the build: the target, and the commit it was
//! built from (`HIDANE_BUILD_TARGET`, `HIDANE_BUILD_COMMIT`; empty when not known).

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-env-changed=HIDANE_THIRD_PARTY_LICENSES");
    let notices = match env::var_os("HIDANE_THIRD_PARTY_LICENSES") {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
            fs::read_to_string(&path).unwrap_or_else(|err| {
                panic!(
                    "HIDANE_THIRD_PARTY_LICENSES={}: {err}",
                    path.to_string_lossy()
                )
            })
        }
        None => "The third-party notices are embedded in release builds only. Each release \
                 archive also carries them as THIRD-PARTY-LICENSES.txt; `cargo about generate \
                 about.hbs` writes them from a checkout.\n"
            .to_owned(),
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    fs::write(out.join("third-party-licenses.txt"), notices).expect("OUT_DIR is writable");

    let target = env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=HIDANE_BUILD_TARGET={target}");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let commit = commit(&manifest).unwrap_or_default();
    println!("cargo:rustc-env=HIDANE_BUILD_COMMIT={commit}");
}

/// The commit the crate is built from: in a package from crates.io, the one `cargo package`
/// recorded in `.cargo_vcs_info.json`; in a checkout of the hidane repository, `HEAD` (the
/// script runs again when it moves). `None` otherwise, such as for a source archive.
fn commit(manifest: &Path) -> Option<String> {
    if let Ok(text) = fs::read_to_string(manifest.join(".cargo_vcs_info.json")) {
        // {"git": {"sha1": "<40 hex digits>"}, "path_in_vcs": "crates/hidane"}
        let rest = &text[text.find("\"sha1\"")? + 6..];
        let start = rest.find('"')? + 1;
        let end = start + rest[start..].find('"')?;
        return hex(&rest[start..end]);
    }
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(manifest)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    // Not the commit of another repository that happens to contain the sources.
    if git(&["rev-parse", "--show-prefix"])?.trim_end_matches('/') != "crates/hidane" {
        return None;
    }
    let sha = hex(&git(&["rev-parse", "HEAD"])?)?;
    // HEAD names a branch, whose ref is a file of its own or a line of packed-refs.
    let mut watched = vec![git(&["rev-parse", "--git-path", "HEAD"])];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(git(&["rev-parse", "--git-path", &branch]));
    }
    watched.push(git(&["rev-parse", "--git-path", "packed-refs"]));
    for path in watched.into_iter().flatten() {
        let path = manifest.join(path);
        // A file that does not exist would make the script run on every build.
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    Some(sha)
}

fn hex(sha: &str) -> Option<String> {
    (sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then(|| sha.to_owned())
}
