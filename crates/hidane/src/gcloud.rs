//! hidane's container image where Testcontainers expects the Cloud SDK image
//! (docs/testcontainers.md).
//!
//! Testcontainers' Firestore modules for Java, Node and Go start the Cloud SDK image with the
//! command `/bin/sh -c "gcloud beta emulators firestore start --host-port 0.0.0.0:8080 …"`. In
//! hidane's image, whose entrypoint is hidane, that command arrives as hidane's arguments. hidane
//! reads the gcloud command line in the script itself, without a shell (the image has none):
//! plain words, quotes and backslashes only. It serves the emulator with its own flags for
//! gcloud's, and refuses anything else, such as another command, a pipe or a variable.
//!
//! Nothing printed here may contain the word "running": the modules wait for a log line that
//! contains it, and hidane's banner ends with `Dev App Server is now running.`.

/// Why a script is not served.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not a gcloud command line hidane can read (exit 127, as a shell would for a command it
    /// cannot find).
    NotGcloud(String),
    /// A gcloud command line with a flag or command hidane does not serve (exit 2, a usage
    /// error).
    Usage(String),
}

impl Refusal {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::NotGcloud(_) => 127,
            Self::Usage(_) => 2,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::NotGcloud(message) | Self::Usage(message) => message,
        }
    }
}

const DOCS: &str = "https://github.com/hidane-dev/hidane/blob/main/docs/testcontainers.md";

/// hidane's emulator flags for the script of `sh -c <script>`.
pub fn from_script(script: &str) -> Result<Vec<String>, Refusal> {
    let words = words(script).map_err(|why| {
        Refusal::NotGcloud(format!(
            "hidane's image has no shell: it reads only a `gcloud … emulators firestore start` \
             command, and {why} ({DOCS})"
        ))
    })?;
    match words.split_first() {
        Some((gcloud, args)) if gcloud.rsplit('/').next() == Some("gcloud") => {
            flags(args).map_err(Refusal::Usage)
        }
        _ => Err(Refusal::NotGcloud(format!(
            "hidane's image has no shell: it reads only a `gcloud … emulators firestore start` \
             command, not `{}` ({DOCS})",
            script.trim()
        ))),
    }
}

/// Splits a script into words as a shell would split a simple command with no expansions:
/// whitespace between words, single and double quotes, backslash escapes. Operators,
/// expansions, redirections and globs are refused, with the reason.
fn words(script: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // A word has started, even if empty so far (`''`).
    let mut started = false;
    let mut chars = script.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("a single quote is not closed".into()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('\n') => {}
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("a double quote is not closed".into()),
                        },
                        Some(c @ ('$' | '`')) => {
                            return Err(format!("`{c}` would need a shell to expand"));
                        }
                        Some(c) => word.push(c),
                        None => return Err("a double quote is not closed".into()),
                    }
                }
            }
            '\\' => match chars.next() {
                // A line continuation.
                Some('\n') => {}
                Some(c) => {
                    started = true;
                    word.push(c);
                }
                None => return Err("the script ends with a backslash".into()),
            },
            c if c.is_ascii_alphanumeric() || "-_=.:/,@%+[]".contains(c) => {
                started = true;
                word.push(c);
            }
            '\n' => return Err("it holds more than one line".into()),
            c => return Err(format!("`{c}` would need a shell")),
        }
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// `gcloud emulators firestore start` flags that hidane serves, with the flag hidane takes for
/// each (`None`: accepted and not needed).
///
/// From the command's reference, https://cloud.google.com/sdk/gcloud/reference/emulators/firestore/start
const VALUE_FLAGS: [(&str, Option<&str>); 8] = [
    ("database-mode", Some("--database-mode")),
    ("edition", Some("--database-edition")),
    ("export-on-exit", Some("--export-on-exit")),
    ("import-data", Some("--import-data")),
    ("index-file", Some("--index_file")),
    ("rules", Some("--rules")),
    // gcloud-wide flags. hidane serves every project, as the official emulator does under
    // gcloud; Testcontainers' Go module passes `--project`.
    ("project", None),
    ("verbosity", None),
];
const SWITCHES: [(&str, Option<&str>); 3] = [
    ("licenses", Some("--licenses")),
    ("require-indexes", Some("--require_indexes")),
    ("quiet", None),
];

/// hidane's emulator flags for gcloud's arguments (the words after `gcloud`).
pub fn flags(args: &[String]) -> Result<Vec<String>, String> {
    // hidane flag → value, in order; a later gcloud flag replaces an earlier one, as in gcloud.
    let mut out: Vec<(&str, Option<String>)> = Vec::new();
    let mut set = |flag: &'static str, value: Option<String>| {
        out.retain(|(f, _)| *f != flag);
        out.push((flag, value));
    };
    let mut command = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "-q" {
            continue;
        }
        let Some(flag) = arg.strip_prefix("--") else {
            command.push(arg.as_str());
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        if let Some((_, to)) = SWITCHES.iter().find(|(n, _)| *n == name) {
            if inline.is_some() {
                return Err(format!("`--{name}` takes no value"));
            }
            if let Some(to) = *to {
                set(to, None);
            }
            continue;
        }
        let known = name == "host-port" || VALUE_FLAGS.iter().any(|(n, _)| *n == name);
        if !known {
            return Err(format!(
                "`--{name}` is not a flag of `gcloud emulators firestore start` that hidane \
                 serves ({DOCS})"
            ));
        }
        let value = match inline {
            Some(value) => value,
            None => match args.next() {
                Some(value) if !value.starts_with("--") => value.clone(),
                _ => return Err(format!("`--{name}` needs a value")),
            },
        };
        if name == "host-port" {
            let (host, port) = host_port(&value)?;
            if let Some(host) = host {
                set("--host", Some(host));
            }
            if let Some(port) = port {
                set("--port", Some(port.to_string()));
            }
        } else if let Some(&(_, Some(to))) = VALUE_FLAGS.iter().find(|(n, _)| *n == name) {
            set(to, Some(value));
        }
    }
    let command = match command.as_slice() {
        ["alpha" | "beta", rest @ ..] => rest,
        rest => rest,
    };
    if command != ["emulators", "firestore", "start"] {
        return Err(format!(
            "hidane serves `gcloud emulators firestore start`, not `gcloud {}` ({DOCS})",
            command.join(" ")
        ));
    }
    Ok(out
        .into_iter()
        .flat_map(|(flag, value)| std::iter::once(flag.to_owned()).chain(value))
        .collect())
}

/// `--host-port`: `[ADDRESS][:PORT]`, an IPv6 address in square brackets.
fn host_port(value: &str) -> Result<(Option<String>, Option<u16>), String> {
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| format!("`--host-port {value}`: `[` without `]`"))?;
        match after {
            "" => (host, None),
            _ => match after.strip_prefix(':') {
                Some(port) => (host, Some(port)),
                None => return Err(format!("`--host-port {value}`: expected `:` after `]`")),
            },
        }
    } else {
        match value.split_once(':') {
            Some((_, port)) if port.contains(':') => {
                return Err(format!(
                    "`--host-port {value}`: enclose an IPv6 address in square brackets"
                ));
            }
            Some((host, port)) => (host, Some(port)),
            None => (value, None),
        }
    };
    let port = port
        .map(|p| {
            p.parse::<u16>()
                .map_err(|_| format!("`--host-port {value}`: `{p}` is not a port"))
        })
        .transpose()?;
    let host = (!host.is_empty()).then(|| host.to_owned());
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn served(script: &str) -> Result<Vec<String>, Refusal> {
        from_script(script)
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn what_the_testcontainers_modules_run() {
        // Java (org.testcontainers.gcloud.FirestoreEmulatorContainer, 2.0.5), with `withFlags`.
        assert_eq!(
            served("gcloud beta emulators firestore start --host-port 0.0.0.0:8080"),
            Ok(strings(&["--host", "0.0.0.0", "--port", "8080"]))
        );
        assert_eq!(
            served(
                "gcloud beta emulators firestore start --host-port 0.0.0.0:8080 --rules /r/firestore.rules"
            ),
            Ok(strings(&[
                "--host",
                "0.0.0.0",
                "--port",
                "8080",
                "--rules",
                "/r/firestore.rules"
            ]))
        );
        // Node (@testcontainers/gcloud 12.2.0): `--name=value`, after two spaces.
        assert_eq!(
            served("gcloud beta emulators firestore start  --host-port=0.0.0.0:8080"),
            Ok(strings(&["--host", "0.0.0.0", "--port", "8080"]))
        );
        // Go (modules/gcloud/firestore, v0.44.0).
        assert_eq!(
            served(
                "gcloud beta emulators firestore start --host-port 0.0.0.0:8080 --project=test-project"
            ),
            Ok(strings(&["--host", "0.0.0.0", "--port", "8080"]))
        );
    }

    #[test]
    fn every_flag_of_the_command() {
        assert_eq!(
            served(
                "gcloud emulators firestore start --database-mode=firestore-native --edition enterprise \
                 --export-on-exit=/out --import-data /in/x.overall_export_metadata --index-file=/i.yaml \
                 --licenses --require-indexes --rules=/r --project p --verbosity=debug --quiet -q"
            ),
            Ok(strings(&[
                "--database-mode",
                "firestore-native",
                "--database-edition",
                "enterprise",
                "--export-on-exit",
                "/out",
                "--import-data",
                "/in/x.overall_export_metadata",
                "--index_file",
                "/i.yaml",
                "--licenses",
                "--require_indexes",
                "--rules",
                "/r",
            ]))
        );
    }

    #[test]
    fn the_command_and_its_tracks() {
        assert_eq!(served("gcloud emulators firestore start"), Ok(vec![]));
        assert_eq!(
            served("/usr/bin/gcloud alpha emulators firestore start"),
            Ok(vec![])
        );
        // gcloud-wide flags may come first.
        assert_eq!(
            served("gcloud --project=p beta emulators firestore start --host-port=:9000"),
            Ok(strings(&["--port", "9000"]))
        );
        for other in [
            "gcloud beta emulators datastore start",
            "gcloud emulators firestore env-init",
            "gcloud beta emulators firestore start extra",
        ] {
            assert!(
                matches!(served(other), Err(Refusal::Usage(m)) if m.contains("hidane serves `gcloud emulators firestore start`")),
                "{other}"
            );
        }
    }

    #[test]
    fn a_later_flag_replaces_an_earlier_one() {
        assert_eq!(
            served(
                "gcloud beta emulators firestore start --host-port 0.0.0.0:8080 --host-port=127.0.0.1:9090"
            ),
            Ok(strings(&["--host", "127.0.0.1", "--port", "9090"]))
        );
    }

    #[test]
    fn host_port_forms() {
        assert_eq!(
            host_port("0.0.0.0:8080"),
            Ok((Some("0.0.0.0".into()), Some(8080)))
        );
        assert_eq!(host_port("localhost"), Ok((Some("localhost".into()), None)));
        assert_eq!(host_port(":8081"), Ok((None, Some(8081))));
        assert_eq!(host_port("[::]:8080"), Ok((Some("::".into()), Some(8080))));
        assert_eq!(
            host_port("[2001:db8::1]"),
            Ok((Some("2001:db8::1".into()), None))
        );
        assert!(
            host_port("::1:8080")
                .unwrap_err()
                .contains("square brackets")
        );
        assert!(
            host_port("0.0.0.0:http")
                .unwrap_err()
                .contains("not a port")
        );
        assert!(
            host_port("0.0.0.0:70000")
                .unwrap_err()
                .contains("not a port")
        );
        assert!(host_port("[::1").is_err());
        assert!(host_port("[::1]8080").is_err());
    }

    #[test]
    fn flags_hidane_does_not_serve_are_refused() {
        for (script, says) in [
            (
                "gcloud beta emulators firestore start --store-on-disk",
                "`--store-on-disk` is not a flag",
            ),
            (
                "gcloud beta emulators firestore start --rules",
                "`--rules` needs a value",
            ),
            (
                "gcloud beta emulators firestore start --rules --licenses",
                "`--rules` needs a value",
            ),
            (
                "gcloud beta emulators firestore start --licenses=true",
                "`--licenses` takes no value",
            ),
        ] {
            assert!(
                matches!(served(script), Err(Refusal::Usage(ref m)) if m.contains(says)),
                "{script}: {:?}",
                served(script)
            );
        }
    }

    #[test]
    fn quotes_and_escapes() {
        assert_eq!(
            words(r#"gcloud --rules '/a b/r' --x="c \"d\" \$" e\ f '' "#),
            Ok(strings(&[
                "gcloud",
                "--rules",
                "/a b/r",
                r#"--x=c "d" $"#,
                "e f",
                ""
            ]))
        );
        assert_eq!(
            words("gcloud \\\n  start\n"),
            Ok(strings(&["gcloud", "start"]))
        );
    }

    #[test]
    fn anything_that_needs_a_shell_is_refused() {
        for script in [
            "gcloud beta emulators firestore start; echo done",
            "gcloud beta emulators firestore start && true",
            "gcloud beta emulators firestore start | tee log",
            "gcloud beta emulators firestore start > log",
            "gcloud beta emulators firestore start --rules=$RULES",
            "gcloud beta emulators firestore start --rules=\"$RULES\"",
            "gcloud beta emulators firestore start --rules=`pwd`",
            "gcloud beta emulators firestore start --rules=*.rules",
            "gcloud beta emulators firestore start --rules='/r",
            "gcloud beta emulators firestore start\ngcloud beta emulators firestore start",
            "FOO=1 gcloud beta emulators firestore start",
            "exec gcloud beta emulators firestore start",
            "true && (cat /proc/net/tcp* | awk '{print $2}' | grep -i :1f90)",
            "",
        ] {
            let refusal = served(script).unwrap_err();
            assert!(
                matches!(refusal, Refusal::NotGcloud(_)),
                "{script}: {refusal:?}"
            );
            assert_eq!(refusal.exit_code(), 127);
        }
    }

    #[test]
    fn no_message_says_running() {
        // The modules wait for a line with "running" and would take a refusal that said it for
        // readiness.
        for script in [
            "echo hi",
            "a; b",
            "'",
            "gcloud x",
            "gcloud beta emulators firestore start --x",
            "gcloud beta emulators firestore start --rules",
            "gcloud beta emulators firestore start --licenses=1",
            "gcloud beta emulators firestore start --host-port=h:x",
            "gcloud beta emulators firestore start --host-port=::1",
        ] {
            let refusal = served(script).unwrap_err();
            assert!(!refusal.message().contains("running"), "{refusal:?}");
        }
    }
}
