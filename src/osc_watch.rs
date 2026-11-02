//! The desktop-app backend: reports go into the terminal as OSC 7501 escape
//! sequences, not over Herdr's socket.
//!
//! Writing into a terminal that something else is writing to is the hazard.
//! A sequence landing in the middle of a program's own escape sequence garbles
//! its screen, so this backend writes as little as it can from the background:
//!
//! * the **watcher** writes exactly one `working` report, when the command
//!   crosses the threshold, and then goes quiet. No ticking label: herdr-app
//!   shows how long an agent has been in a state by itself.
//! * the **final report** is written by `finish`, which the shell's pre-prompt
//!   hook runs in the foreground. By then the command is over and the prompt
//!   not yet drawn, so nothing else is writing to the terminal at all.
//! * the **clear** before the next command is printed by the shell's
//!   pre-command hook itself, for the same reason.
//!
//! The watcher and `finish` race at the threshold: the command can end while
//! the watcher is deciding to report it. Two files and a lock settle it.
//!
//! * The watcher writes `osc-start`, *then* looks at `exit`. The hook truncated
//!   `exit` before the command started, so a non-empty one means the command
//!   is already over: the watcher takes `osc-start` back and stays silent.
//! * The hook writes `exit`, *then* looks for `osc-start`, and runs `finish`
//!   only if it is there.
//!
//! Each side writes before it reads, so at least one of them sees the other.
//! If the hook sees `osc-start`, `finish` takes the lock the watcher holds from
//! writing `osc-start` until its report is out, so `done` can never land before
//! the `working` it replaces.

use crate::args::FinishArgs;
use crate::config::Config;
use crate::osc::{self, Finish, State};
use crate::{command, label, state};
use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Written by the watcher when it reports, consumed by `finish`: the command's
/// start in epoch milliseconds on the first line, its program name on the
/// second. The shell only tests whether it is non-empty.
pub const START_FILE: &str = "osc-start";

/// Non-empty while a record of ours is up. The shell's pre-command hook clears
/// the terminal's record only then, so an ordinary command costs no output.
pub const SHOWN_FILE: &str = "osc-shown";

const LOCK_FILE: &str = "osc-lock";

/// How long `finish` waits for the watcher to let go of the lock. The watcher
/// holds it for a couple of file operations and one small write, so this only
/// matters if that write is stuck behind a terminal that stopped reading.
/// `finish` holds up the prompt, so it gives up and goes ahead.
const LOCK_WAIT_MS: u64 = 250;

/// Where reports are written.
pub trait Sink {
    fn write(&mut self, bytes: &str);
}

/// The controlling terminal.
pub struct Tty;

impl Sink for Tty {
    fn write(&mut self, bytes: &str) {
        write_tty(bytes.as_bytes());
    }
}

/// Writes `bytes` to the controlling terminal, or nothing at all.
///
/// The watcher's own stdout and stderr are `/dev/null` — the hook redirects
/// them so a stray message can never land in the session — so it opens
/// `/dev/tty`, which it still has: it is a child of the interactive shell.
///
/// The first write is non-blocking: a terminal that is not reading right now
/// (scroll lock, a stopped output) costs the report, never a stuck watcher.
/// But once part of a sequence is out the rest must follow, or the terminal is
/// left inside an unterminated OSC string, swallowing the program's output
/// until something happens to end it. So a short write finishes blocking.
fn write_tty(bytes: &[u8]) {
    // A background process writing to its terminal is stopped by SIGTTOU when
    // the user has `stty tostop`. Ignoring the signal lets the write through.
    // SAFETY: setting a signal's disposition to SIG_IGN has no preconditions.
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
    }
    let Ok(tty) = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open("/dev/tty")
    else {
        return;
    };
    let fd = tty.as_raw_fd();
    let mut sent = 0;
    while sent < bytes.len() {
        let rest = &bytes[sent..];
        // SAFETY: `fd` is open for the lifetime of `tty`, and `rest` is a live
        // slice of exactly `rest.len()` bytes.
        let n = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
        if n > 0 {
            sent += n as usize;
            continue;
        }
        let err = std::io::Error::last_os_error();
        match err.kind() {
            std::io::ErrorKind::Interrupted => continue,
            std::io::ErrorKind::WouldBlock if sent > 0 => {
                // SAFETY: as above; F_GETFL/F_SETFL on an open fd.
                unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFL);
                    if flags >= 0 {
                        libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
                    }
                }
            }
            _ => return,
        }
    }
}

/// Holds the state directory's lock until dropped (closing the file releases
/// it). `None` when it could not be had in time; callers go ahead regardless.
fn lock(state_dir: &Path, wait: Duration) -> Option<File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state_dir.join(LOCK_FILE))
        .ok()?;
    let deadline = Instant::now() + wait;
    loop {
        // SAFETY: flock on an open fd has no other preconditions.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(file);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn non_empty(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// The command a watcher is watching.
pub struct Tracked<'a> {
    pub state_dir: &'a Path,
    /// Epoch milliseconds.
    pub start_ms: u64,
    /// The program name: the report's `app`, and `{agent}` in a label.
    pub agent: &'a str,
    /// The command line, already cut to `max_title_len`.
    pub title: &'a str,
}

/// The watcher's one report, sent when the command crosses the threshold.
/// Returns false when the command turned out to be over already.
fn report_working(cmd: &Tracked, sink: &mut dyn Sink) -> bool {
    let Tracked {
        state_dir,
        start_ms,
        agent,
        title,
    } = *cmd;
    // Waiting is pointless here: nobody else takes the lock until the hook has
    // seen `osc-start`, which this is about to write.
    let _lock = lock(state_dir, Duration::from_millis(LOCK_WAIT_MS));
    let start = state_dir.join(START_FILE);
    if std::fs::write(&start, format!("{start_ms}\n{agent}\n")).is_err() {
        // Without the file `finish` never runs, and a `working` nobody
        // replaces would hang over the terminal until the next command.
        return false;
    }
    if non_empty(&state_dir.join("exit")) {
        let _ = std::fs::remove_file(&start);
        return false;
    }
    // Before the write, not after: if anything below goes wrong, the next
    // command still clears whatever made it to the terminal.
    let _ = std::fs::write(state_dir.join(SHOWN_FILE), "1\n");
    sink.write(&osc::report(State::Working, agent, title, title));
    true
}

/// The watcher, in the desktop app. Waits out the threshold, reports once, and
/// then idles until the shell signals it.
pub fn watch(
    rx: &Receiver<i32>,
    cfg: &Config,
    cmd: &Tracked,
    now_ms: &dyn Fn() -> u64,
    alive: &dyn Fn() -> bool,
    sink: &mut dyn Sink,
) -> i32 {
    loop {
        let elapsed = now_ms().saturating_sub(cmd.start_ms);
        if elapsed >= cfg.threshold_ms {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(cfg.threshold_ms - elapsed)) {
            // SIGUSR1: the command finished before it was slow. SIGTERM: the
            // shell is going away. Either way nothing was ever written.
            Ok(_) => return 0,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return 0,
        }
        if !alive() {
            return 0;
        }
    }
    report_working(cmd, sink);
    // The hook still sends SIGUSR1 when the command ends, and the default
    // disposition would kill whoever holds this PID by then. See `park`.
    crate::watch::park(rx, cfg.tick_ms, alive)
}

/// Leaves the final report, from the shell's pre-prompt hook. Silent unless the
/// watcher reported this command.
pub fn finish(args: &FinishArgs, cfg: &Config, now_ms: u64, sink: &mut dyn Sink) {
    let _lock = lock(&args.state_dir, Duration::from_millis(LOCK_WAIT_MS));
    let start_path = args.state_dir.join(START_FILE);
    let Ok(start) = std::fs::read_to_string(&start_path) else {
        return;
    };
    let _ = std::fs::remove_file(&start_path);
    let mut lines = start.lines();
    let Some(start_ms) = lines.next().and_then(|l| l.trim().parse::<u64>().ok()) else {
        return;
    };
    let agent = lines.next().unwrap_or("").trim();

    let cmd_line = std::fs::read_to_string(args.state_dir.join("cmd")).unwrap_or_default();
    let title = command::truncate(cmd_line.trim(), cfg.max_title_len);
    let outcome = label::classify(args.exit);
    let text = state::finish_label(cfg, outcome, now_ms.saturating_sub(start_ms), &title, agent);

    match osc::finish_state(outcome, &cfg.finish) {
        Finish::Clear => {
            sink.write(osc::CLEAR);
            let _ = std::fs::remove_file(args.state_dir.join(SHOWN_FILE));
        }
        Finish::Report(state) => {
            let _ = std::fs::write(args.state_dir.join(SHOWN_FILE), "1\n");
            sink.write(&osc::report(state, agent, &title, &text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[derive(Default)]
    struct Recorder(Vec<String>);

    impl Sink for Recorder {
        fn write(&mut self, bytes: &str) {
            self.0.push(bytes.to_string());
        }
    }

    /// Decodes the base64 `key` of a report, for assertions that read better
    /// as text.
    fn field(report: &str, key: &str) -> Option<String> {
        let body = report.strip_prefix("\x1b]7501;")?.strip_suffix("\x1b\\")?;
        let value = body
            .split(':')
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))?;
        if key != "title" && key != "msg" {
            return Some(value.to_string());
        }
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bits = 0u32;
        let mut nbits = 0;
        let mut out = Vec::new();
        for c in value.bytes().filter(|&c| c != b'=') {
            bits = (bits << 6) | alphabet.iter().position(|&a| a == c)? as u32;
            nbits += 6;
            if nbits >= 8 {
                nbits -= 8;
                out.push((bits >> nbits) as u8);
            }
        }
        String::from_utf8(out).ok()
    }

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn finish_args(state_dir: &Path, exit: Option<i32>) -> FinishArgs {
        FinishArgs {
            state_dir: state_dir.to_path_buf(),
            exit,
        }
    }

    // ---- the threshold report ----------------------------------------------

    #[test]
    fn crossing_the_threshold_reports_working_once() {
        let d = dir();
        let mut sink = Recorder::default();
        assert!(report_working(
            &tracked(d.path(), 1_000, "cargo", "cargo build"),
            &mut sink
        ));

        assert_eq!(sink.0.len(), 1);
        assert_eq!(field(&sink.0[0], "state").as_deref(), Some("working"));
        assert_eq!(field(&sink.0[0], "app").as_deref(), Some("cargo"));
        assert_eq!(field(&sink.0[0], "title").as_deref(), Some("cargo build"));
        assert_eq!(
            std::fs::read_to_string(d.path().join(START_FILE)).unwrap(),
            "1000\ncargo\n",
            "finish needs the start and the program name"
        );
        assert!(non_empty(&d.path().join(SHOWN_FILE)));
    }

    #[test]
    fn a_command_that_already_ended_is_not_reported() {
        let d = dir();
        std::fs::write(d.path().join("exit"), "0\n").unwrap();
        let mut sink = Recorder::default();

        assert!(!report_working(
            &tracked(d.path(), 1_000, "cargo", "cargo build"),
            &mut sink
        ));
        assert!(sink.0.is_empty(), "a working nobody replaces would hang");
        assert!(
            !d.path().join(START_FILE).exists(),
            "finish must find nothing to finish"
        );
        assert!(!non_empty(&d.path().join(SHOWN_FILE)));
    }

    #[test]
    fn an_empty_exit_file_means_the_command_is_still_running() {
        let d = dir();
        // What the hook leaves when it truncates `exit` before each command.
        std::fs::write(d.path().join("exit"), "").unwrap();
        let mut sink = Recorder::default();
        assert!(report_working(
            &tracked(d.path(), 1_000, "cargo", "x"),
            &mut sink
        ));
        assert_eq!(sink.0.len(), 1);
    }

    // ---- finish ------------------------------------------------------------

    fn reported(d: &Path, start_ms: u64, agent: &str, cmd: &str) {
        std::fs::write(d.join("cmd"), format!("{cmd}\n")).unwrap();
        let mut sink = Recorder::default();
        assert!(report_working(&tracked(d, start_ms, agent, cmd), &mut sink));
    }

    #[test]
    fn finish_is_silent_when_nothing_was_reported() {
        let d = dir();
        let mut sink = Recorder::default();
        finish(
            &finish_args(d.path(), Some(1)),
            &Config::default(),
            9_000,
            &mut sink,
        );
        assert!(sink.0.is_empty(), "a fast command leaves no trace");
    }

    #[test]
    fn a_success_finishes_as_done_with_its_label() {
        let d = dir();
        reported(d.path(), 1_000, "cargo", "cargo build --release");
        let mut sink = Recorder::default();
        finish(
            &finish_args(d.path(), Some(0)),
            &Config::default(),
            253_000,
            &mut sink,
        );

        assert_eq!(sink.0.len(), 1);
        let r = &sink.0[0];
        assert_eq!(field(r, "state").as_deref(), Some("done"));
        assert_eq!(field(r, "app").as_deref(), Some("cargo"));
        assert_eq!(field(r, "title").as_deref(), Some("cargo build --release"));
        assert_eq!(field(r, "msg").as_deref(), Some("ok · 4m12s"));
        assert!(
            !d.path().join(START_FILE).exists(),
            "consumed, so a second finish says nothing"
        );
        assert!(
            non_empty(&d.path().join(SHOWN_FILE)),
            "the next command clears it"
        );
    }

    #[test]
    fn a_failure_finishes_as_error() {
        let d = dir();
        reported(d.path(), 1_000, "make", "make test");
        let mut sink = Recorder::default();
        finish(
            &finish_args(d.path(), Some(2)),
            &Config::default(),
            13_000,
            &mut sink,
        );
        assert_eq!(field(&sink.0[0], "state").as_deref(), Some("error"));
        assert_eq!(field(&sink.0[0], "msg").as_deref(), Some("exit 2 · 12s"));
    }

    #[test]
    fn a_cancel_finishes_as_idle() {
        let d = dir();
        reported(d.path(), 1_000, "sleep", "sleep 60");
        let mut sink = Recorder::default();
        finish(
            &finish_args(d.path(), Some(130)),
            &Config::default(),
            4_000,
            &mut sink,
        );
        assert_eq!(field(&sink.0[0], "state").as_deref(), Some("idle"));
        assert_eq!(field(&sink.0[0], "msg").as_deref(), Some("SIGINT · 3s"));
    }

    #[test]
    fn an_unknown_exit_code_never_reads_as_done() {
        let d = dir();
        reported(d.path(), 1_000, "sleep", "sleep 60");
        let mut sink = Recorder::default();
        finish(
            &finish_args(d.path(), None),
            &Config::default(),
            4_000,
            &mut sink,
        );
        assert_eq!(field(&sink.0[0], "state").as_deref(), Some("idle"));
        assert_eq!(field(&sink.0[0], "msg").as_deref(), Some("3s"));
    }

    #[test]
    fn zero_success_stickiness_clears_the_record_at_once() {
        let d = dir();
        reported(d.path(), 1_000, "cargo", "cargo build");
        let mut cfg = Config::default();
        cfg.finish.success_sticky_ms = 0;
        let mut sink = Recorder::default();
        finish(&finish_args(d.path(), Some(0)), &cfg, 9_000, &mut sink);
        assert_eq!(sink.0, vec![osc::CLEAR.to_string()]);
        assert!(
            !d.path().join(SHOWN_FILE).exists(),
            "nothing left for the next command to clear"
        );
    }

    #[test]
    fn the_label_templates_from_config_apply() {
        let d = dir();
        reported(d.path(), 1_000, "cargo", "cargo build");
        let mut cfg = Config::default();
        cfg.labels.success = "{agent} done in {elapsed}".into();
        let mut sink = Recorder::default();
        finish(&finish_args(d.path(), Some(0)), &cfg, 6_000, &mut sink);
        assert_eq!(
            field(&sink.0[0], "msg").as_deref(),
            Some("cargo done in 5s")
        );
    }

    #[test]
    fn a_corrupt_start_file_finishes_silently() {
        let d = dir();
        std::fs::write(d.path().join(START_FILE), "garbage\n").unwrap();
        let mut sink = Recorder::default();
        finish(
            &finish_args(d.path(), Some(0)),
            &Config::default(),
            6_000,
            &mut sink,
        );
        assert!(sink.0.is_empty());
        assert!(
            !d.path().join(START_FILE).exists(),
            "and does not trip the next one"
        );
    }

    /// The race the lock exists for: the hook saw `osc-start` while the watcher
    /// was still between writing it and writing `working`. `finish` must wait,
    /// or `done` lands first and `working` overwrites it.
    #[test]
    fn finish_waits_for_a_watcher_mid_report() {
        let d = dir();
        std::fs::write(d.path().join("cmd"), "cargo build\n").unwrap();
        std::fs::write(d.path().join(START_FILE), "1000\ncargo\n").unwrap();
        let held = lock(d.path(), Duration::ZERO).expect("the lock is free");

        let path = d.path().to_path_buf();
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::<&str>::new()));
        let seen = std::sync::Arc::clone(&order);
        let finisher = std::thread::spawn(move || {
            let mut sink = Recorder::default();
            finish(
                &finish_args(&path, Some(0)),
                &Config::default(),
                9_000,
                &mut sink,
            );
            seen.lock().unwrap().push("finished");
            sink.0
        });

        std::thread::sleep(Duration::from_millis(50));
        order.lock().unwrap().push("watcher released");
        drop(held);

        let written = finisher.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["watcher released", "finished"]);
        assert_eq!(field(&written[0], "state").as_deref(), Some("done"));
    }

    #[test]
    fn a_lock_nobody_releases_does_not_hold_up_the_prompt_forever() {
        let d = dir();
        let _held = lock(d.path(), Duration::ZERO).unwrap();
        let started = Instant::now();
        assert!(lock(d.path(), Duration::from_millis(30)).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    // ---- the watcher loop --------------------------------------------------

    fn tracked<'a>(
        state_dir: &'a Path,
        start_ms: u64,
        agent: &'a str,
        title: &'a str,
    ) -> Tracked<'a> {
        Tracked {
            state_dir,
            start_ms,
            agent,
            title,
        }
    }

    fn fast_cfg() -> Config {
        Config {
            threshold_ms: 40,
            tick_ms: 5,
            ..Config::default()
        }
    }

    fn clock() -> impl Fn() -> u64 {
        let origin = Instant::now();
        move || origin.elapsed().as_millis() as u64
    }

    #[test]
    fn a_fast_command_writes_nothing() {
        let d = dir();
        let (tx, rx) = mpsc::channel();
        tx.send(libc::SIGUSR1).unwrap();
        let mut sink = Recorder::default();
        let now = clock();
        watch(
            &rx,
            &fast_cfg(),
            &tracked(d.path(), now(), "x", "x"),
            &now,
            &|| true,
            &mut sink,
        );
        assert!(sink.0.is_empty());
        assert!(!d.path().join(START_FILE).exists());
    }

    #[test]
    fn a_slow_command_is_reported_then_the_watcher_waits_for_the_shell() {
        let d = dir();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let _ = tx.send(libc::SIGUSR1);
        });
        let mut sink = Recorder::default();
        let now = clock();
        let started = Instant::now();
        watch(
            &rx,
            &fast_cfg(),
            &tracked(d.path(), now(), "sleep", "sleep 9"),
            &now,
            &|| true,
            &mut sink,
        );

        assert_eq!(sink.0.len(), 1, "one report, no ticking");
        assert_eq!(field(&sink.0[0], "state").as_deref(), Some("working"));
        assert!(
            started.elapsed() >= Duration::from_millis(120),
            "it outlives the report until the shell's SIGUSR1"
        );
    }

    #[test]
    fn a_shell_that_died_during_the_threshold_gets_no_report() {
        let d = dir();
        let (_tx, rx) = mpsc::channel::<i32>();
        let mut sink = Recorder::default();
        let now = clock();
        watch(
            &rx,
            &fast_cfg(),
            &tracked(d.path(), now(), "x", "x"),
            &now,
            &|| false,
            &mut sink,
        );
        assert!(
            sink.0.is_empty(),
            "nobody is left to see it, or to finish it"
        );
    }
}
