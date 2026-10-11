//! Comparing Security Rules denials with the official emulator's.

/// A denial's reasons: `\nfalse for 'get' @ L5, <error> for 'get' @ L6`, each ending in a line
/// number (an error's own text may hold `, `).
pub fn reasons(message: &str) -> Vec<&str> {
    let mut rest = message.strip_prefix('\n').unwrap_or(message);
    let mut reasons = Vec::new();
    let mut scanned = 0;
    while let Some(at) = rest[scanned..].find(" @ L").map(|i| scanned + i + 4) {
        let end = at + rest[at..].bytes().take_while(u8::is_ascii_digit).count();
        match rest[end..].strip_prefix(", ") {
            Some(next) if end > at => {
                reasons.push(&rest[..end]);
                rest = next;
                scanned = 0;
            }
            _ => scanned = end,
        }
    }
    reasons.push(rest);
    reasons
}

/// Whether hidane's denial says what the official emulator's does. The official emulator
/// evaluates conditions in two passes and lists the reasons of both (firebase-tools #6252,
/// `docs/parity-exceptions.md`), hidane evaluates them once: its reasons must be among the
/// official emulator's. Other messages must be equal.
pub fn same_denial(hidane: &str, official: &str) -> bool {
    if official.starts_with('\n') && hidane.starts_with('\n') {
        let official = reasons(official);
        reasons(hidane).iter().all(|r| official.contains(r))
    } else {
        hidane == official
    }
}
