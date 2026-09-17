//! Whether the tracked command is a locked `keylock` session, and how that
//! shows in the row. Pure: the watcher does the talking to keylock.

/// keylock's own program name, as `command::agent_name` reports it.
#[allow(dead_code)]
const KEYLOCK: &str = "keylock";

#[allow(dead_code)]
pub fn is_keylock(agent: &str) -> bool {
    agent == KEYLOCK
}

/// Reads `keylock status`'s reply. `None` means "no answer we understand" —
/// empty output, an error message, or the usage text a keylock older than
/// 0.2.0 prints for the unknown `--pane` flag.
///
/// A pane can hold several sessions (a nested `keylock run`); one locked
/// session is enough to call the pane locked.
#[allow(dead_code)]
pub fn parse_status(stdout: &str) -> Option<bool> {
    let mut seen_unlocked = false;
    for line in stdout.lines() {
        if line.starts_with("locked ") {
            return Some(true);
        }
        if line.starts_with("unlocked ") {
            seen_unlocked = true;
        }
    }
    seen_unlocked.then_some(false)
}

/// The row name with the lock in front. An empty prefix is the off switch.
pub fn decorate(text: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return text.to_string();
    }
    format!("{prefix}{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_keylock_program_is_probed() {
        assert!(is_keylock("keylock"));
        // `agent_name` already reduces a path to its basename.
        assert!(!is_keylock("keylockd"));
        assert!(!is_keylock("lock"));
        assert!(!is_keylock(""));
        assert!(!is_keylock("shell"));
    }

    #[test]
    fn status_lines_decide_the_lock() {
        assert_eq!(
            parse_status("locked pid=4821 cmd=./migrate.sh\n"),
            Some(true)
        );
        assert_eq!(
            parse_status("unlocked pid=4821 cmd=./migrate.sh\n"),
            Some(false)
        );
        // Several sessions in one pane: any locked session locks the row.
        assert_eq!(
            parse_status("unlocked pid=1 cmd=a\nlocked pid=2 cmd=b\n"),
            Some(true)
        );
        assert_eq!(
            parse_status("unlocked pid=1 cmd=a\nunlocked pid=2 cmd=b\n"),
            Some(false)
        );
    }

    #[test]
    fn anything_else_is_unknown() {
        assert_eq!(parse_status(""), None);
        assert_eq!(parse_status("\n\n"), None);
        // keylock older than 0.2.0 has no --pane and prints usage.
        assert_eq!(parse_status("usage:\n  keylock run [--locked] ...\n"), None);
        assert_eq!(parse_status("keylock: no session in pane w1:p1\n"), None);
        // A command line that merely starts with the word is not a state.
        assert_eq!(parse_status("lockedish pid=1 cmd=x\n"), None);
        assert_eq!(parse_status("locked\n"), None);
    }

    #[test]
    fn decorate_prefixes_only_when_asked() {
        assert_eq!(
            decorate("keylock run -- ./m.sh", "🔒 "),
            "🔒 keylock run -- ./m.sh"
        );
        assert_eq!(
            decorate("keylock run -- ./m.sh", ""),
            "keylock run -- ./m.sh"
        );
        // The prefix lands outside an already-truncated name.
        assert_eq!(
            decorate("keylock run -- ./mi…", "🔒 "),
            "🔒 keylock run -- ./mi…"
        );
    }
}
