//! Embeds the third-party notices into release builds, for `--licenses`: the release workflow
//! writes them with cargo-about (`about.toml`, `about.hbs`) and points
//! `HIDANE_THIRD_PARTY_LICENSES` at the file. Other builds say where to find them.

use std::{env, fs, path::PathBuf};

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
}
