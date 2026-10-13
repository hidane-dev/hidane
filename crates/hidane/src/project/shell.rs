//! Shell command lines as workflows, npm scripts and shell scripts write them, split into
//! simple commands and words with their positions, and classified: a firebase-tools emulator
//! command, one already under `hidane exec`, a package script, a shell script, a use of Java.
//!
//! Only what can be read with confidence is classified. Commands inside quotes (`bash -c "…"`),
//! command substitutions and the like are left alone; [`Parsed::unread`] lists where the text
//! names the emulators outside a command that was read, so that the scan can say so.

/// A word of a command: its text with quotes and escapes removed, and its byte offsets in the
/// script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub start: usize,
    pub end: usize,
    /// It had quotes or escapes.
    pub quoted: bool,
    /// It holds an expansion (`$X`, `$(…)`, a workflow expression) whose value is not known.
    pub dynamic: bool,
}

/// A simple command: its words, from the first one that is not a reserved word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub words: Vec<Word>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub commands: Vec<Command>,
    /// Byte ranges of comments and heredoc bodies.
    pub skipped: Vec<(usize, usize)>,
}

/// Splits `script` into simple commands.
pub fn parse(script: &str) -> Parsed {
    let masked = mask_expressions(script);
    let bytes = masked.as_bytes();
    let mut parsed = Parsed::default();
    let mut words: Vec<Word> = Vec::new();
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    let end_command = |words: &mut Vec<Word>, parsed: &mut Parsed| {
        let mut list = std::mem::take(words);
        while list
            .first()
            .is_some_and(|w| !w.quoted && RESERVED.contains(&w.text.as_str()))
        {
            list.remove(0);
        }
        if !list.is_empty() {
            parsed.commands.push(Command { words: list });
        }
    };
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\r' => i += 1,
            b'\\' if bytes.get(i + 1) == Some(&b'\n') => i += 2,
            b'\\' if bytes.get(i + 1) == Some(&b'\r') && bytes.get(i + 2) == Some(&b'\n') => i += 3,
            b'#' => {
                let end = masked[i..].find('\n').map_or(bytes.len(), |e| i + e);
                parsed.skipped.push((i, end));
                i = end;
            }
            b'\n' => {
                end_command(&mut words, &mut parsed);
                i += 1;
                // Heredoc bodies start on the next line.
                for (delimiter, strip_tabs) in std::mem::take(&mut heredocs) {
                    let start = i;
                    loop {
                        if i >= bytes.len() {
                            break;
                        }
                        let line_end = masked[i..].find('\n').map_or(bytes.len(), |e| i + e);
                        let line = masked[i..line_end].trim_end_matches('\r');
                        let line = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            line
                        };
                        i = (line_end + 1).min(bytes.len());
                        if line == delimiter {
                            break;
                        }
                    }
                    parsed.skipped.push((start, i));
                }
            }
            b';' | b'&' | b'|' | b'(' | b')' => {
                end_command(&mut words, &mut parsed);
                i += 1;
            }
            b'<' | b'>' => {
                // A redirection: the operator and its target are not words of the command.
                if masked[i..].starts_with("<<") && !masked[i..].starts_with("<<<") {
                    let mut j = i + 2;
                    let strip_tabs = bytes.get(j) == Some(&b'-');
                    if strip_tabs {
                        j += 1;
                    }
                    while bytes.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
                        j += 1;
                    }
                    let (word, next) = read_word(&masked, j);
                    heredocs.push((word.text, strip_tabs));
                    i = next;
                    continue;
                }
                let mut j = i;
                while bytes
                    .get(j)
                    .is_some_and(|b| matches!(b, b'<' | b'>' | b'&'))
                {
                    j += 1;
                }
                if bytes.get(j).is_some_and(u8::is_ascii_digit) && bytes[j - 1] == b'&' {
                    while bytes.get(j).is_some_and(u8::is_ascii_digit) {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                while bytes.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
                    j += 1;
                }
                let (_, next) = read_word(&masked, j);
                i = next.max(j);
            }
            _ => {
                // `2>` and `2>&1`: a file descriptor before a redirection.
                if bytes[i].is_ascii_digit() {
                    let mut j = i;
                    while bytes.get(j).is_some_and(u8::is_ascii_digit) {
                        j += 1;
                    }
                    if bytes.get(j).is_some_and(|b| matches!(b, b'<' | b'>')) {
                        i = j;
                        continue;
                    }
                }
                let (mut word, next) = read_word(&masked, i);
                if masked[word.start..word.end].contains("${{") {
                    word.dynamic = true;
                }
                // The word as written in the script, expressions unmasked.
                if word.dynamic && !word.quoted {
                    word.text = script[word.start..word.end].to_owned();
                }
                words.push(word);
                i = next.max(i + 1);
            }
        }
    }
    end_command(&mut words, &mut parsed);
    parsed
}

/// Words that start a compound command or precede a simple one.
const RESERVED: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}", "time",
    "esac", "exec",
];

/// GitHub Actions expressions (`${{ … }}`) as one word: replaced, byte for byte, by `$` and
/// underscores, so that offsets stay the same.
fn mask_expressions(script: &str) -> String {
    let mut out = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(at) = rest.find("${{") {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        match after.find("}}") {
            Some(end) if after[..end + 2].is_ascii() => {
                out.push('$');
                out.extend(std::iter::repeat_n('_', end + 1));
                rest = &after[end + 2..];
            }
            _ => {
                out.push_str("${{");
                rest = &after[3..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The word starting at `i`, and where the text after it starts.
fn read_word(s: &str, start: usize) -> (Word, usize) {
    let bytes = s.as_bytes();
    let mut text = String::new();
    let mut quoted = false;
    let mut dynamic = false;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\n' | b'\r' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>' => break,
            b'\\' => {
                quoted = true;
                if let Some(c) = s[i + 1..].chars().next() {
                    if c != '\n' {
                        text.push(c);
                    }
                    i += 1 + c.len_utf8();
                } else {
                    i += 1;
                }
            }
            b'\'' => {
                quoted = true;
                let end = s[i + 1..].find('\'').map_or(bytes.len(), |e| i + 1 + e);
                text.push_str(&s[i + 1..end]);
                i = (end + 1).min(bytes.len());
            }
            b'"' => {
                quoted = true;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    match bytes[i] {
                        b'\\' if matches!(bytes.get(i + 1), Some(b'"' | b'\\' | b'$' | b'`')) => {
                            text.push(bytes[i + 1] as char);
                            i += 2;
                        }
                        b'$' | b'`' => {
                            dynamic = true;
                            text.push(bytes[i] as char);
                            i += 1;
                        }
                        _ => {
                            let c = s[i..].chars().next().unwrap_or(' ');
                            text.push(c);
                            i += c.len_utf8();
                        }
                    }
                }
                i = (i + 1).min(bytes.len());
            }
            b'$' => {
                dynamic = true;
                match bytes.get(i + 1) {
                    Some(b'(') => {
                        let end = closing(s, i + 1, b'(', b')');
                        text.push_str(&s[i..end]);
                        i = end;
                    }
                    Some(b'{') => {
                        let end = closing(s, i + 1, b'{', b'}');
                        text.push_str(&s[i..end]);
                        i = end;
                    }
                    _ => {
                        text.push('$');
                        i += 1;
                    }
                }
            }
            b'`' => {
                dynamic = true;
                let end = s[i + 1..].find('`').map_or(bytes.len(), |e| i + 2 + e);
                text.push_str(&s[i..end.min(bytes.len())]);
                i = end.min(bytes.len());
            }
            _ => {
                let c = s[i..].chars().next().unwrap_or(' ');
                text.push(c);
                i += c.len_utf8();
            }
        }
    }
    (
        Word {
            text,
            start,
            end: i,
            quoted,
            dynamic,
        },
        i,
    )
}

/// The end (exclusive) of the group opened at `open_at`.
fn closing(s: &str, open_at: usize, open: u8, close: u8) -> usize {
    let bytes = s.as_bytes();
    let mut depth = 0;
    let mut i = open_at;
    while i < bytes.len() {
        match bytes[i] {
            b if b == open => depth += 1,
            b if b == close => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'\'' => {
                i = s[i + 1..].find('\'').map_or(bytes.len(), |e| i + 1 + e);
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// The firebase-tools emulator commands.
pub const EMULATOR_COMMANDS: [&str; 2] = ["emulators:exec", "emulators:start"];

/// A `firebase emulators:exec` or `emulators:start` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emulators {
    /// `emulators:exec` or `emulators:start`.
    pub subcommand: String,
    /// The value of `--only`, split; `None` without the flag.
    pub only: Option<Vec<String>>,
    /// `--only` names a value that is not known (a variable, an expression).
    pub only_dynamic: bool,
    /// `--config`.
    pub config: Option<String>,
    /// The script `emulators:exec` runs.
    pub script: Option<String>,
    /// `--import`, `--export-on-exit`.
    pub import: bool,
    pub export: bool,
    /// Byte offset where the command starts (after environment assignments): where
    /// `hidane exec -- ` goes.
    pub start: usize,
    /// Byte offset of the end of the command's last word.
    pub end: usize,
    /// How firebase-tools is run: `firebase`, `npx firebase-tools@15`, ….
    pub launcher: String,
    /// The version of firebase-tools the launcher names (`npx firebase-tools@15.33.0`).
    pub version: Option<String>,
}

/// What a command is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A firebase-tools emulator command.
    Emulators(Emulators),
    /// `hidane exec [--] <command>`: the command it runs, read the same way.
    Hidane(Box<Kind>),
    /// A package script: the tool, the script names, and where to look for them.
    Script(ScriptCall),
    /// A shell script file run with its arguments.
    ShellFile(String),
    /// `bash -c '…'` and the like: a script in a string.
    Inline(String),
    /// `cd <dir>`; `None` for a directory that is not known.
    Cd(Option<String>),
    /// A command that uses Java.
    Java(String),
    /// `gcloud … emulators …`.
    Gcloud(String),
    /// `make <targets>`.
    Make,
    /// A command that installs hidane (`npm i -g hidane`, the install script, …).
    InstallsHidane,
    /// `npm install -g firebase-tools@<version>` and the like: the version, if named.
    InstallsFirebaseTools(Option<String>),
    /// Anything else.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCall {
    /// `npm`, `yarn`, `pnpm`, `bun`, `turbo`, `lerna`.
    pub tool: String,
    pub scripts: Vec<String>,
    /// `--prefix` / `-C` / `--cwd` / `--dir`: the directory, relative to the current one.
    pub dir: Option<String>,
    /// `-w` / `--workspace` / `--filter` / `yarn workspace <name>`: a workspace by name or
    /// path.
    pub workspace: Option<String>,
    /// Every workspace that has the script (`--workspaces`, `-r`, `turbo run`, `lerna run`).
    pub all: bool,
}

/// Commands that use Java.
const JAVA: &[&str] = &[
    "java",
    "javac",
    "jar",
    "jshell",
    "mvn",
    "mvnw",
    "gradle",
    "gradlew",
    "sbt",
    "lein",
    "kotlin",
    "kotlinc",
    "ant",
    "jbang",
    "sdkmanager",
    "avdmanager",
    "keytool",
];

/// The last part of a command path.
fn base(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

fn is_assignment(word: &Word) -> bool {
    let text = &word.text;
    match text.find('=') {
        Some(at) if at > 0 => {
            let name = &text[..at];
            name.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// The index of the first word after environment assignments (`A=1 B=2 cmd`) and `env`.
pub fn command_start(words: &[Word]) -> usize {
    let mut i = 0;
    loop {
        while words.get(i).is_some_and(is_assignment) {
            i += 1;
        }
        match words.get(i) {
            Some(w) if w.text == "env" && !w.quoted => i += 1,
            _ => return i,
        }
    }
}

pub fn classify(command: &Command) -> Kind {
    let words = &command.words;
    let at = command_start(words);
    classify_from(words, at)
}

fn classify_from(words: &[Word], at: usize) -> Kind {
    let Some(first) = words.get(at) else {
        return Kind::Other;
    };
    if first.dynamic {
        return Kind::Other;
    }
    let name = base(&first.text);
    let rest = &words[at + 1..];
    let arg = |k: usize| rest.get(k).map(|w| w.text.as_str());
    match name {
        "firebase" => emulators(words, at, at, name),
        "hidane" if arg(0) == Some("exec") => hidane(words, at + 2),
        "npx" | "bunx" | "pnpx" => {
            let mut k = 0;
            while let Some(w) = rest.get(k) {
                match w.text.as_str() {
                    "-y" | "--yes" | "--no" | "--no-install" | "-q" | "--quiet"
                    | "--ignore-existing" | "--bun" => k += 1,
                    "-p" | "--package" => k += 2,
                    t if t.starts_with("--package=") => k += 1,
                    "-c" | "--call" => return Kind::Inline(arg(k + 1).unwrap_or("").to_owned()),
                    _ => break,
                }
            }
            launched(words, at, at + 1 + k)
        }
        "npm" => npm(words, at),
        "yarn" => yarn(words, at),
        "pnpm" => pnpm(words, at),
        "bun" => match arg(0) {
            Some("run") => script_call("bun", rest.get(1..).unwrap_or(&[]), None, None, false),
            Some("x") => launched(words, at, at + 2),
            _ => Kind::Other,
        },
        "turbo" => {
            let tasks: Vec<String> = rest
                .iter()
                .skip(usize::from(arg(0) == Some("run")))
                .take_while(|w| !w.text.starts_with('-'))
                .map(|w| w.text.clone())
                .collect();
            Kind::Script(ScriptCall {
                tool: "turbo".into(),
                scripts: tasks,
                dir: None,
                workspace: None,
                all: true,
            })
        }
        "lerna" if arg(0) == Some("run") => Kind::Script(ScriptCall {
            tool: "lerna".into(),
            scripts: arg(1).map(str::to_owned).into_iter().collect(),
            dir: None,
            workspace: None,
            all: true,
        }),
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "source" | "." => {
            let mut k = 0;
            while let Some(w) = rest.get(k) {
                match w.text.as_str() {
                    "-c" => return Kind::Inline(arg(k + 1).unwrap_or("").to_owned()),
                    "-o" | "+o" => k += 2,
                    t if t.starts_with(['-', '+']) => k += 1,
                    _ => break,
                }
            }
            match rest.get(k) {
                Some(w) if !w.dynamic => Kind::ShellFile(w.text.clone()),
                _ => Kind::Other,
            }
        }
        "cd" | "pushd" => match rest.first() {
            Some(w) if !w.dynamic && w.text != "-" && !w.text.starts_with('~') => {
                Kind::Cd(Some(w.text.clone()))
            }
            Some(_) => Kind::Cd(None),
            None => Kind::Cd(None),
        },
        "gcloud" => {
            let line: Vec<&str> = rest.iter().map(|w| w.text.as_str()).collect();
            if line.contains(&"emulators") {
                Kind::Gcloud(line.join(" "))
            } else {
                Kind::Other
            }
        }
        "make" | "gmake" => Kind::Make,
        "flutter" if arg(0) == Some("build") && matches!(arg(1), Some("apk" | "appbundle")) => {
            Kind::Java(format!("flutter build {}", arg(1).unwrap_or("")))
        }
        "curl" | "wget" if rest.iter().any(|w| w.text.contains("hidane.dev/install")) => {
            Kind::InstallsHidane
        }
        "cargo" if matches!(arg(0), Some("install" | "binstall")) && arg(1) == Some("hidane") => {
            Kind::InstallsHidane
        }
        "brew" if arg(0) == Some("install") && rest.iter().any(|w| w.text.ends_with("hidane")) => {
            Kind::InstallsHidane
        }
        n if JAVA.contains(&n) => Kind::Java(n.to_owned()),
        n if n.ends_with(".sh") => Kind::ShellFile(first.text.clone()),
        _ => Kind::Other,
    }
}

/// `hidane exec [--] <command>`, from the word after `exec`.
fn hidane(words: &[Word], at: usize) -> Kind {
    let at = if words.get(at).is_some_and(|w| w.text == "--") {
        at + 1
    } else {
        at
    };
    let inner = command_start(&words[at.min(words.len())..]) + at;
    Kind::Hidane(Box::new(classify_from(words, inner)))
}

/// What `npx`, `yarn dlx` and the like run: firebase-tools or hidane, from word `at`.
fn launched(words: &[Word], start: usize, at: usize) -> Kind {
    let Some(word) = words.get(at) else {
        return Kind::Other;
    };
    let (package, version) = match word.text.rsplit_once('@') {
        Some((package, version)) if !package.is_empty() => (package, Some(version)),
        _ => (word.text.as_str(), None),
    };
    match package {
        "firebase" | "firebase-tools" => {
            let mut kind = emulators(
                words,
                start,
                at,
                &words[start..=at]
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            if let Kind::Emulators(e) = &mut kind {
                e.version = version.map(str::to_owned);
            }
            kind
        }
        "hidane" if words.get(at + 1).is_some_and(|w| w.text == "exec") => hidane(words, at + 2),
        _ => Kind::Other,
    }
}

/// firebase-tools at word `firebase`, run by the words from `start`: an emulator command, or
/// another command.
fn emulators(words: &[Word], start: usize, firebase: usize, launcher: &str) -> Kind {
    let mut k = firebase + 1;
    let mut config = None;
    // Options before the command.
    while let Some(w) = words.get(k) {
        match w.text.as_str() {
            "--project" | "-P" | "--token" | "--account" => k += 2,
            "--config" | "-c" => {
                config = words.get(k + 1).map(|w| w.text.clone());
                k += 2;
            }
            t if t.starts_with("--config=") => {
                config = Some(t["--config=".len()..].to_owned());
                k += 1;
            }
            t if t.starts_with('-') => k += 1,
            _ => break,
        }
    }
    let Some(sub) = words.get(k) else {
        return Kind::Other;
    };
    if !EMULATOR_COMMANDS.contains(&sub.text.as_str()) {
        return Kind::Other;
    }
    let mut call = Emulators {
        subcommand: sub.text.clone(),
        only: None,
        only_dynamic: false,
        config,
        script: None,
        import: false,
        export: false,
        start: words[start].start,
        end: words.last().map_or(sub.end, |w| w.end),
        launcher: launcher.to_owned(),
        version: None,
    };
    k += 1;
    while let Some(w) = words.get(k) {
        let text = w.text.as_str();
        let (flag, inline) = match text.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (text, None),
        };
        // A flag's value: inline, or the next word.
        let mut value = |optional: bool| -> Option<Word> {
            if let Some(v) = &inline {
                return Some(Word {
                    text: v.clone(),
                    ..w.clone()
                });
            }
            match words.get(k + 1) {
                Some(next) if !(optional && next.text.starts_with('-')) => {
                    k += 1;
                    Some(next.clone())
                }
                _ => None,
            }
        };
        match flag {
            "--only" => {
                if let Some(v) = value(false) {
                    call.only_dynamic = v.dynamic;
                    call.only = Some(
                        v.text
                            .split(',')
                            .map(|o| o.split(':').next().unwrap_or("").trim().to_owned())
                            .filter(|o| !o.is_empty())
                            .collect(),
                    );
                }
            }
            "--config" | "-c" => call.config = value(false).map(|v| v.text),
            "--import" => {
                call.import = true;
                value(false);
            }
            "--export-on-exit" => {
                call.export = true;
                value(true);
            }
            "--inspect-functions" => {
                value(true);
            }
            "--project" | "-P" | "--log-verbosity" | "--token" | "--account" => {
                value(false);
            }
            f if f.starts_with('-') => {}
            _ => {
                if call.script.is_none() && call.subcommand == "emulators:exec" {
                    call.script = Some(w.text.clone());
                }
            }
        }
        k += 1;
    }
    Kind::Emulators(call)
}

fn script_call(
    tool: &str,
    rest: &[Word],
    dir: Option<String>,
    workspace: Option<String>,
    all: bool,
) -> Kind {
    match rest.iter().find(|w| !w.text.starts_with('-')) {
        Some(script) if !script.dynamic => Kind::Script(ScriptCall {
            tool: tool.into(),
            scripts: vec![script.text.clone()],
            dir,
            workspace,
            all,
        }),
        _ => Kind::Other,
    }
}

/// Options of a package manager, before or after its command: the directory, the workspace,
/// whether every workspace is meant, and the words that are not options.
struct PackageOptions {
    dir: Option<String>,
    workspace: Option<String>,
    all: bool,
    positional: Vec<Word>,
}

fn package_options(
    words: &[Word],
    with_value: &[&str],
    dir_flags: &[&str],
    ws_flags: &[&str],
    all_flags: &[&str],
) -> PackageOptions {
    let mut options = PackageOptions {
        dir: None,
        workspace: None,
        all: false,
        positional: Vec::new(),
    };
    let mut k = 0;
    let mut after_dashes = false;
    while let Some(w) = words.get(k) {
        let text = w.text.as_str();
        if after_dashes || !text.starts_with('-') || text == "-" {
            options.positional.push(w.clone());
            k += 1;
            continue;
        }
        if text == "--" {
            after_dashes = true;
            k += 1;
            continue;
        }
        let (flag, inline) = match text.split_once('=') {
            Some((f, v)) => (f, Some(v.to_owned())),
            None => (text, None),
        };
        let takes =
            dir_flags.contains(&flag) || ws_flags.contains(&flag) || with_value.contains(&flag);
        let value = if takes && inline.is_none() {
            k += 1;
            words.get(k).map(|w| w.text.clone())
        } else {
            inline
        };
        if dir_flags.contains(&flag) {
            options.dir = value;
        } else if ws_flags.contains(&flag) {
            options.workspace = value;
        } else if all_flags.contains(&flag) {
            options.all = true;
        }
        k += 1;
    }
    options
}

fn npm(words: &[Word], at: usize) -> Kind {
    let options = package_options(
        &words[at + 1..],
        &["--loglevel", "--registry"],
        &["--prefix", "-C"],
        &["-w", "--workspace"],
        &["-ws", "--workspaces"],
    );
    let mut positional = options.positional.iter();
    let Some(command) = positional.next() else {
        return Kind::Other;
    };
    let script = match command.text.as_str() {
        "run" | "run-script" | "rum" | "urn" => match positional.next() {
            Some(w) if !w.dynamic => w.text.clone(),
            _ => return Kind::Other,
        },
        "test" | "t" | "tst" => "test".to_owned(),
        "start" | "stop" | "restart" => command.text.clone(),
        "exec" | "x" => {
            let first = positional.next();
            return match first {
                Some(w) => {
                    let index = words.iter().position(|x| x.start == w.start).unwrap_or(at);
                    launched(words, at, index)
                }
                None => Kind::Other,
            };
        }
        "install" | "i" | "add" | "ci"
            if options
                .positional
                .iter()
                .any(|w| w.text == "hidane" || w.text.starts_with("hidane@")) =>
        {
            return Kind::InstallsHidane;
        }
        "install" | "i" | "add" => {
            return match options
                .positional
                .iter()
                .find(|w| w.text == "firebase-tools" || w.text.starts_with("firebase-tools@"))
            {
                Some(w) => {
                    Kind::InstallsFirebaseTools(w.text.split_once('@').map(|(_, v)| v.to_owned()))
                }
                None => Kind::Other,
            };
        }
        _ => return Kind::Other,
    };
    Kind::Script(ScriptCall {
        tool: "npm".into(),
        scripts: vec![script],
        dir: options.dir,
        workspace: options.workspace,
        all: options.all,
    })
}

/// yarn's own commands, which are not package scripts.
const YARN_BUILTINS: &[&str] = &[
    "add",
    "audit",
    "autoclean",
    "bin",
    "cache",
    "check",
    "config",
    "constraints",
    "create",
    "dedupe",
    "explain",
    "generate-lock-entry",
    "global",
    "help",
    "import",
    "info",
    "init",
    "install",
    "licenses",
    "link",
    "list",
    "login",
    "logout",
    "node",
    "npm",
    "outdated",
    "owner",
    "pack",
    "patch",
    "plugin",
    "policies",
    "publish",
    "rebuild",
    "remove",
    "search",
    "set",
    "stage",
    "tag",
    "team",
    "unlink",
    "unplug",
    "up",
    "upgrade",
    "upgrade-interactive",
    "version",
    "versions",
    "why",
    "workspaces",
];

fn yarn(words: &[Word], at: usize) -> Kind {
    let options = package_options(&words[at + 1..], &[], &["--cwd"], &[], &[]);
    let positional = &options.positional;
    let Some(command) = positional.first() else {
        return Kind::Other;
    };
    match command.text.as_str() {
        "dlx" | "exec" => {
            let index = words
                .iter()
                .position(|x| x.start == command.start)
                .unwrap_or(at);
            launched(words, at, index + 1)
        }
        "run" => script_call("yarn", &positional[1..], options.dir, None, false),
        "workspace" => match positional.get(1) {
            Some(ws) => script_call(
                "yarn",
                positional
                    .get(2..)
                    .unwrap_or(&[])
                    .iter()
                    .skip_while(|w| w.text == "run")
                    .cloned()
                    .collect::<Vec<_>>()
                    .as_slice(),
                options.dir,
                Some(ws.text.clone()),
                false,
            ),
            None => Kind::Other,
        },
        "workspaces" if positional.get(1).is_some_and(|w| w.text == "foreach") => {
            let run = positional.iter().position(|w| w.text == "run");
            match run {
                Some(r) => script_call("yarn", &positional[r + 1..], None, None, true),
                None => Kind::Other,
            }
        }
        "global" if positional.get(1).is_some_and(|w| w.text == "add") => {
            if positional.iter().any(|w| w.text == "hidane") {
                Kind::InstallsHidane
            } else {
                Kind::Other
            }
        }
        "firebase" => {
            let index = words
                .iter()
                .position(|x| x.start == command.start)
                .unwrap_or(at);
            launched(words, at, index)
        }
        b if YARN_BUILTINS.contains(&b) => Kind::Other,
        _ => script_call("yarn", positional, options.dir, None, false),
    }
}

/// pnpm's own commands, which are not package scripts.
const PNPM_BUILTINS: &[&str] = &[
    "add",
    "audit",
    "bin",
    "config",
    "create",
    "dedupe",
    "deploy",
    "env",
    "fetch",
    "i",
    "import",
    "init",
    "install",
    "install-test",
    "it",
    "licenses",
    "link",
    "list",
    "ln",
    "ls",
    "outdated",
    "pack",
    "patch",
    "patch-commit",
    "prune",
    "publish",
    "rebuild",
    "remove",
    "rm",
    "root",
    "server",
    "setup",
    "store",
    "un",
    "uninstall",
    "unlink",
    "up",
    "update",
    "why",
];

fn pnpm(words: &[Word], at: usize) -> Kind {
    let options = package_options(
        &words[at + 1..],
        &["--reporter"],
        &["-C", "--dir"],
        &["--filter", "-F"],
        &["-r", "--recursive"],
    );
    let positional = &options.positional;
    let Some(command) = positional.first() else {
        return Kind::Other;
    };
    match command.text.as_str() {
        "exec" | "dlx" => {
            let index = words
                .iter()
                .position(|x| x.start == command.start)
                .unwrap_or(at);
            launched(words, at, index + 1)
        }
        "run" | "run-script" => script_call(
            "pnpm",
            &positional[1..],
            options.dir,
            options.workspace,
            options.all,
        ),
        "test" | "t" | "tst" | "start" => Kind::Script(ScriptCall {
            tool: "pnpm".into(),
            scripts: vec![if command.text.starts_with('t') {
                "test".into()
            } else {
                "start".into()
            }],
            dir: options.dir,
            workspace: options.workspace,
            all: options.all,
        }),
        "add" if positional.iter().any(|w| w.text == "hidane") => Kind::InstallsHidane,
        b if PNPM_BUILTINS.contains(&b) => Kind::Other,
        _ => script_call(
            "pnpm",
            positional,
            options.dir,
            options.workspace,
            options.all,
        ),
    }
}

/// Where the text names an emulator command (`emulators:exec`, `emulators:start`) outside the
/// commands that were read and outside comments: byte offsets.
pub fn unread_mentions(script: &str, parsed: &Parsed, read: &[(usize, usize)]) -> Vec<usize> {
    let mut found = Vec::new();
    for name in EMULATOR_COMMANDS {
        let mut from = 0;
        while let Some(at) = script[from..].find(name) {
            let at = from + at;
            from = at + name.len();
            let inside = |ranges: &[(usize, usize)]| ranges.iter().any(|&(s, e)| s <= at && at < e);
            if !inside(&parsed.skipped) && !inside(read) {
                found.push(at);
            }
        }
    }
    found.sort_unstable();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(script: &str) -> Vec<Kind> {
        parse(script).commands.iter().map(classify).collect()
    }

    fn only(kind: &Kind) -> &Emulators {
        match kind {
            Kind::Emulators(e) => e,
            Kind::Hidane(inner) => only(inner),
            other => panic!("not an emulator command: {other:?}"),
        }
    }

    #[test]
    fn words_and_commands() {
        let parsed = parse(
            "npm ci && FOO='a b' npx -y firebase-tools@15.33.0 emulators:exec --only=firestore,auth \"npm test\" # done\nnpm run x",
        );
        assert_eq!(parsed.commands.len(), 3);
        let words: Vec<_> = parsed.commands[1]
            .words
            .iter()
            .map(|w| w.text.as_str())
            .collect();
        assert_eq!(
            words,
            [
                "FOO=a b",
                "npx",
                "-y",
                "firebase-tools@15.33.0",
                "emulators:exec",
                "--only=firestore,auth",
                "npm test"
            ]
        );
        let kind = classify(&parsed.commands[1]);
        let e = only(&kind);
        assert_eq!(e.subcommand, "emulators:exec");
        assert_eq!(
            e.only.as_deref(),
            Some(&["firestore".to_owned(), "auth".to_owned()][..])
        );
        assert_eq!(e.script.as_deref(), Some("npm test"));
        assert_eq!(e.version.as_deref(), Some("15.33.0"));
        assert_eq!(e.launcher, "npx -y firebase-tools@15.33.0");
        // The command starts after the assignment.
        let text = "npm ci && FOO='a b' npx";
        assert_eq!(e.start, text.len() - 3);
        assert_eq!(parsed.skipped.len(), 1);
    }

    #[test]
    fn launchers() {
        for (script, expected) in [
            (
                "firebase emulators:start --only firestore &",
                Some("emulators:start"),
            ),
            (
                "./node_modules/.bin/firebase emulators:exec 'jest'",
                Some("emulators:exec"),
            ),
            (
                "pnpm exec firebase emulators:exec x",
                Some("emulators:exec"),
            ),
            ("yarn firebase emulators:start", Some("emulators:start")),
            (
                "yarn dlx firebase-tools emulators:start",
                Some("emulators:start"),
            ),
            (
                "npm exec -- firebase emulators:start",
                Some("emulators:start"),
            ),
            (
                "firebase --project demo emulators:exec x",
                Some("emulators:exec"),
            ),
            ("firebase deploy", None),
            ("echo firebase emulators:start", None),
        ] {
            let kinds = kinds(script);
            let found = kinds.iter().find_map(|k| match k {
                Kind::Emulators(e) => Some(e.subcommand.as_str()),
                _ => None,
            });
            assert_eq!(found, expected, "{script}");
        }
        let wrapped = kinds("hidane exec -- npx firebase emulators:exec --only firestore x");
        assert!(
            matches!(&wrapped[0], Kind::Hidane(inner) if matches!(**inner, Kind::Emulators(_)))
        );
        let wrapped = kinds("npx hidane exec firebase emulators:start");
        assert!(matches!(&wrapped[0], Kind::Hidane(_)));
    }

    #[test]
    fn scripts_and_others() {
        let k = kinds(
            "npm run test:emu -- --watch\nnpm --prefix functions test\nyarn workspace api test\npnpm -r test\nturbo run build test\n./scripts/ci.sh\nbash -e scripts/x.sh\nbash -c 'firebase emulators:start'\ncd functions\n./gradlew test\nmake test",
        );
        let call = |k: &Kind| match k {
            Kind::Script(c) => (
                c.tool.clone(),
                c.scripts.clone(),
                c.dir.clone(),
                c.workspace.clone(),
                c.all,
            ),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            call(&k[0]),
            ("npm".into(), vec!["test:emu".into()], None, None, false)
        );
        assert_eq!(
            call(&k[1]),
            (
                "npm".into(),
                vec!["test".into()],
                Some("functions".into()),
                None,
                false
            )
        );
        assert_eq!(
            call(&k[2]),
            (
                "yarn".into(),
                vec!["test".into()],
                None,
                Some("api".into()),
                false
            )
        );
        assert_eq!(
            call(&k[3]),
            ("pnpm".into(), vec!["test".into()], None, None, true)
        );
        assert_eq!(
            call(&k[4]),
            (
                "turbo".into(),
                vec!["build".into(), "test".into()],
                None,
                None,
                true
            )
        );
        assert_eq!(k[5], Kind::ShellFile("./scripts/ci.sh".into()));
        assert_eq!(k[6], Kind::ShellFile("scripts/x.sh".into()));
        assert_eq!(k[7], Kind::Inline("firebase emulators:start".into()));
        assert_eq!(k[8], Kind::Cd(Some("functions".into())));
        assert_eq!(k[9], Kind::Java("gradlew".into()));
        assert_eq!(k[10], Kind::Make);
    }

    #[test]
    fn unread_mentions_and_expressions() {
        let script = "concurrently \"firebase emulators:start\" \"npm test\"\n# firebase emulators:exec\nfirebase emulators:exec --only ${{ matrix.only }} x";
        let parsed = parse(script);
        let mut read = Vec::new();
        for c in &parsed.commands {
            if let Kind::Emulators(e) = classify(c) {
                assert!(e.only_dynamic);
                read.push((e.start, e.end));
            }
        }
        assert_eq!(read.len(), 1);
        let unread = unread_mentions(script, &parsed, &read);
        assert_eq!(unread, [script.find("emulators:start").unwrap()]);
    }

    #[test]
    fn heredocs_and_redirections() {
        let parsed = parse(
            "cat <<EOF > out.txt\nfirebase emulators:start\nEOF\nfirebase emulators:exec x 2>&1 | tee log",
        );
        let kinds: Vec<_> = parsed.commands.iter().map(classify).collect();
        assert_eq!(
            kinds
                .iter()
                .filter(|k| matches!(k, Kind::Emulators(_)))
                .count(),
            1
        );
        assert_eq!(parsed.skipped.len(), 1);
    }
}
