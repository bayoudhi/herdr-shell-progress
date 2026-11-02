//! The Program Status Protocol (OSC 7501), as spoken to the Herdr desktop app.
//!
//! The desktop app runs its terminals in herdr-core, which has no socket method
//! for reporting a pane's state. A program reports by printing an escape
//! sequence into its own terminal instead:
//!
//! ```text
//! ESC ] 7501 ; state=working:app=cargo:title=<base64>:msg=<base64> ESC \
//! ```
//!
//! Pure: this module only builds the bytes. The watcher and the `finish`
//! subcommand decide when they are written, and to where.

use crate::config::Finish as FinishConfig;
use crate::label::Outcome;

/// Reported when the program name is unusable as an `app` value. herdr-app
/// looks `app` up in its program table, and `shell` is the entry it already
/// uses for shells.
pub const FALLBACK_APP: &str = "shell";

/// The protocol's cap on a decoded `title`.
const MAX_TITLE_BYTES: usize = 192;

/// Well under the protocol's 2048-byte cap on a decoded `msg`. A finish label is
/// a dozen characters; this only guards against a pathological template.
const MAX_MSG_BYTES: usize = 512;

/// The protocol's cap on `app`.
const MAX_APP_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    Done,
    Error,
    Idle,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Done => "done",
            State::Error => "error",
            State::Idle => "idle",
        }
    }
}

/// What a finished command leaves behind in the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Report(State),
    /// Remove the record outright.
    Clear,
}

/// How each outcome maps onto the protocol's states.
///
/// * success is `done`, which herdr-app shows as finished-while-you-were-away
///   and turns to `idle` once you look at the terminal;
/// * a failure is `error`, which it ranks with the things that need you;
/// * a signal is `idle`: the protocol asks for `idle` when the user cancels, and
///   a Ctrl+C is not news;
/// * an unreadable exit code is `idle` too — it must never pass for a success.
///
/// The protocol has no expiry, so `success_sticky_ms` cannot be honored as a
/// duration. Zero still means what it says: clear the moment the command ends.
/// Anything else keeps the record until the next command clears it.
pub fn finish_state(outcome: Outcome, cfg: &FinishConfig) -> Finish {
    match outcome {
        Outcome::Failure(_) if cfg.failure_sticky => Finish::Report(State::Error),
        Outcome::Failure(_) => auto_clearing(State::Idle, cfg),
        Outcome::Success => auto_clearing(State::Done, cfg),
        Outcome::Signal(_) | Outcome::Unknown => auto_clearing(State::Idle, cfg),
    }
}

fn auto_clearing(state: State, cfg: &FinishConfig) -> Finish {
    if cfg.success_sticky_ms == 0 {
        Finish::Clear
    } else {
        Finish::Report(state)
    }
}

/// Words that open a compound command. `for i in …; do make; done` is the shell
/// at work, not a program called `for`.
const SHELL_KEYWORDS: &[&str] = &[
    "for", "while", "until", "if", "case", "select", "function", "repeat", "coproc",
];

/// The program name as an `app` value, or the fallback when it is a shell
/// keyword, has a byte outside `[A-Za-z0-9_.+-]`, or is too long. Never
/// truncated: a cut name could match the wrong entry in herdr-app's program
/// table.
pub fn app_name(agent: &str) -> &str {
    let valid = !agent.is_empty()
        && !SHELL_KEYWORDS.contains(&agent)
        && agent.len() <= MAX_APP_BYTES
        && agent
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'));
    if valid {
        agent
    } else {
        FALLBACK_APP
    }
}

/// A full report. Empty `title` or `msg` are left out rather than sent empty.
pub fn report(state: State, app: &str, title: &str, msg: &str) -> String {
    let mut out = format!("\x1b]7501;state={}:app={}", state.as_str(), app_name(app));
    let title = clean(title, MAX_TITLE_BYTES);
    if !title.is_empty() {
        out.push_str(":title=");
        out.push_str(&base64(title.as_bytes()));
    }
    let msg = clean(msg, MAX_MSG_BYTES);
    if !msg.is_empty() {
        out.push_str(":msg=");
        out.push_str(&base64(msg.as_bytes()));
    }
    out.push_str("\x1b\\");
    out
}

/// Removes every record on the terminal.
///
/// Not scoped with an `id`: herdr-app shows the root record, so that is the one
/// we report into, and a root clear is how a root record goes away.
pub const CLEAR: &str = "\x1b]7501;state=clear\x1b\\";

/// Drops control characters — a report carrying one is discarded whole by the
/// terminal — and cuts to `max` bytes on a character boundary.
fn clean(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    for c in text.chars() {
        if c.is_control() {
            continue;
        }
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. Hand-rolled: a dozen lines are cheaper than a
/// dependency in a binary spawned once per prompt.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish_cfg(success_sticky_ms: u64, failure_sticky: bool) -> FinishConfig {
        FinishConfig {
            success_sticky_ms,
            failure_sticky,
        }
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn base64_carries_utf8_through_untouched() {
        // "ok · 4s": the middle dot is two bytes.
        assert_eq!(base64("ok · 4s".as_bytes()), "b2sgwrcgNHM=");
    }

    #[test]
    fn a_working_report_is_framed_exactly_as_the_spec_says() {
        assert_eq!(
            report(State::Working, "cargo", "cargo build", "cargo build"),
            "\x1b]7501;state=working:app=cargo:title=Y2FyZ28gYnVpbGQ=:msg=Y2FyZ28gYnVpbGQ=\x1b\\"
        );
    }

    #[test]
    fn empty_fields_are_left_out() {
        assert_eq!(
            report(State::Idle, "make", "", ""),
            "\x1b]7501;state=idle:app=make\x1b\\"
        );
    }

    #[test]
    fn every_state_has_its_protocol_word() {
        for (state, word) in [
            (State::Working, "state=working:"),
            (State::Done, "state=done:"),
            (State::Error, "state=error:"),
            (State::Idle, "state=idle:"),
        ] {
            assert!(report(state, "x", "", "").contains(word), "{state:?}");
        }
    }

    /// A report whose decoded text holds a control character is thrown away
    /// whole, so a tab in a command line would cost the entire report.
    #[test]
    fn control_characters_never_reach_the_wire() {
        assert_eq!(clean("a\tb\nc\x1b[0md\u{9b}e\u{7f}", 64), "abc[0mde");
    }

    #[test]
    fn text_is_cut_on_a_character_boundary() {
        // Each `·` is two bytes; three bytes fit one and a half of them.
        assert_eq!(clean("···", 3), "·");
        assert_eq!(clean("abc", 3), "abc");
        assert_eq!(clean("abcd", 3), "abc");
    }

    #[test]
    fn a_long_title_stays_inside_the_protocol_limit() {
        let long = "x".repeat(1000);
        let wire = report(State::Working, "x", &long, "");
        let title = wire
            .split(':')
            .find_map(|p| p.strip_prefix("title="))
            .unwrap()
            .trim_end_matches("\x1b\\");
        assert!(title.len() <= 256, "encoded title is {} bytes", title.len());
    }

    #[test]
    fn the_whole_sequence_stays_under_the_4096_byte_cap() {
        let huge = "é".repeat(5000);
        let app = "a".repeat(32);
        assert!(report(State::Error, &app, &huge, &huge).len() < 4096);
    }

    #[test]
    fn program_names_become_app_values_only_when_valid() {
        assert_eq!(app_name("cargo"), "cargo");
        assert_eq!(app_name("build.sh"), "build.sh");
        assert_eq!(app_name("g++"), "g++");
        assert_eq!(app_name("for"), FALLBACK_APP, "a loop is the shell's");
        assert_eq!(app_name("while"), FALLBACK_APP);
        assert_eq!(app_name("forge"), "forge", "only the whole word");
        assert_eq!(app_name("my_tool-2"), "my_tool-2");
        assert_eq!(app_name(""), FALLBACK_APP);
        assert_eq!(app_name("café"), FALLBACK_APP);
        assert_eq!(
            app_name("a:b"),
            FALLBACK_APP,
            "a colon would split the pair"
        );
        assert_eq!(app_name("a;b"), FALLBACK_APP, "a semicolon ends the OSC");
        assert_eq!(app_name(&"a".repeat(33)), FALLBACK_APP);
        assert_eq!(app_name(&"a".repeat(32)), "a".repeat(32));
    }

    #[test]
    fn the_clear_sequence_removes_every_record() {
        assert_eq!(CLEAR, "\x1b]7501;state=clear\x1b\\");
    }

    #[test]
    fn outcomes_map_onto_protocol_states() {
        let cfg = finish_cfg(20_000, true);
        assert_eq!(
            finish_state(Outcome::Success, &cfg),
            Finish::Report(State::Done)
        );
        assert_eq!(
            finish_state(Outcome::Failure(1), &cfg),
            Finish::Report(State::Error)
        );
        assert_eq!(
            finish_state(Outcome::Signal("SIGINT"), &cfg),
            Finish::Report(State::Idle),
            "a cancel is not news"
        );
        assert_eq!(
            finish_state(Outcome::Unknown, &cfg),
            Finish::Report(State::Idle),
            "an unreadable exit code must not pass for done"
        );
    }

    #[test]
    fn zero_success_stickiness_clears_instead_of_reporting() {
        let cfg = finish_cfg(0, true);
        assert_eq!(finish_state(Outcome::Success, &cfg), Finish::Clear);
        assert_eq!(finish_state(Outcome::Signal("SIGINT"), &cfg), Finish::Clear);
        assert_eq!(finish_state(Outcome::Unknown, &cfg), Finish::Clear);
        assert_eq!(
            finish_state(Outcome::Failure(2), &cfg),
            Finish::Report(State::Error),
            "a sticky failure stays whatever success does"
        );
    }

    #[test]
    fn a_non_sticky_failure_is_reported_without_raising_the_alarm() {
        assert_eq!(
            finish_state(Outcome::Failure(1), &finish_cfg(20_000, false)),
            Finish::Report(State::Idle)
        );
        assert_eq!(
            finish_state(Outcome::Failure(1), &finish_cfg(0, false)),
            Finish::Clear
        );
    }
}
