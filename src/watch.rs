use crate::args::Args;
use crate::command;
use crate::config::Config;
use crate::proto;
use crate::socket::{self, SendError};
use crate::state::{self, Action, Machine};
use signal_hook::consts::{SIGTERM, SIGUSR1};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Give up after this many consecutive socket failures.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;

/// How often a lingering watcher re-checks whether it still owns the pane.
/// Deliberately shorter than `config::MIN_INTERVAL_MS`: a replacement watcher
/// clears the marker at `preexec` and only writes its own once its command
/// crosses the threshold, so a poll strictly faster than that floor is
/// guaranteed to land inside the gap. Costs one small file read, never a socket.
const LINGER_POLL_MS: u64 = 100;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn socket_path() -> PathBuf {
    match std::env::var("HERDR_SOCKET_PATH") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config/herdr/herdr.sock")
        }
    }
}

/// Must match `id` in herdr-plugin.toml — pinned by a test, not by discipline.
const PLUGIN_ID: &str = "bayoudhi.shell-progress";

/// Pure fallback logic for the config directory, split out from `config_dir`
/// so the `$HOME`-based default can be unit tested without mutating
/// process-global environment state.
fn resolve_config_dir(env_value: Option<&str>, home: &str) -> PathBuf {
    match env_value {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(home)
            .join(".config/herdr/plugins/config")
            .join(PLUGIN_ID),
    }
}

/// This watcher is spawned by the user's zsh `preexec` hook, not by Herdr, so
/// `HERDR_PLUGIN_CONFIG_DIR` (which Herdr injects only into processes it
/// spawns itself) is never set here in practice. Always fall back to the
/// real config directory Herdr uses for this plugin.
fn config_dir() -> Option<PathBuf> {
    let env_value = std::env::var("HERDR_PLUGIN_CONFIG_DIR").ok();
    let home = std::env::var("HOME").unwrap_or_default();
    Some(resolve_config_dir(env_value.as_deref(), &home))
}

/// True while the shell that spawned us is still alive. Signal 0 checks for
/// existence without delivering anything.
fn shell_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// The exit code `precmd` wrote, or `None` when the file is missing, empty, or
/// unparseable. Never defaults to `0`: that would render a failed command as a
/// success, which is the one thing this plugin exists to prevent.
fn read_exit_code(state_dir: &Path) -> Option<i32> {
    read_trimmed(&state_dir.join("exit"))?.parse::<i32>().ok()
}

/// The agent name held by the marker, if any.
///
/// A zero-byte marker is removed on sight. It cannot satisfy `clear_actions`
/// (which needs an agent name to release), yet `preexec` only tests for the
/// file's existence — left alone it would switch `--clear-first` on for every
/// command from now until the end of time.
fn read_marker(path: &Path) -> Option<String> {
    let agent = read_trimmed(path)?;
    if agent.is_empty() {
        let _ = std::fs::remove_file(path);
        return None;
    }
    Some(agent)
}

/// The name the ownership probe looks for when checking whether this watcher
/// still owns the pane.
///
/// This must equal whatever `Action::MarkerWrite` puts in the marker. The two
/// live in different files — the writer is `state.rs`, which uses
/// `proto::AGENT_ID` directly — and they have drifted apart once already: when
/// the reported agent id became a constant, only the writer followed, leaving
/// this probe hunting for a name the marker would never contain, permanently
/// false. The shared constant is what keeps them equal; the test
/// `the_linger_probe_watches_the_name_the_marker_actually_receives` is what
/// notices if that ever stops being true.
fn own_agent_id() -> String {
    proto::AGENT_ID.to_string()
}

/// True while the marker still names `agent`.
fn marker_names(path: &Path, agent: &str) -> bool {
    read_trimmed(path).as_deref() == Some(agent)
}

/// Unlinks the marker only if it still names `agent`. Two watchers can be alive
/// at once — a lingering success watcher and the one for the command you just
/// started — and the older one must never delete the younger one's marker: that
/// marker is what makes the next `preexec` pass `--clear-first`, and without it
/// a sticky failure label can never be cleared again.
fn remove_marker_owned_by(path: &Path, agent: &str) -> bool {
    if !marker_names(path, agent) {
        return false;
    }
    std::fs::remove_file(path).is_ok()
}

/// How an interruptible linger ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lingered {
    /// The sticky window ran out (or the shell died). Finish the cleanup.
    Completed,
    /// A newer watcher owns this pane now. Everything queued behind the linger
    /// belongs to somebody else's command and must not run.
    Superseded,
}

/// Waits out the post-success sticky window without going deaf.
///
/// A plain `thread::sleep` here was the bug that corrupted the next command:
/// the process sat outside the signal channel for the whole window, then woke
/// up and ran its release and marker removal against a pane a newer watcher had
/// already claimed. Two independent escapes now cut it short:
///
/// * a signal — `SIGTERM` from `zshexit`, or `SIGUSR1` that can only belong to
///   a command we are not watching;
/// * losing the marker — the replacement watcher removes it at `preexec` time
///   (that is what `--clear-first` does), which is visible to us on the next
///   poll and does not depend on any signal being deliverable.
fn linger(
    rx: &Receiver<i32>,
    total_ms: u64,
    poll_ms: u64,
    still_ours: &dyn Fn() -> bool,
    alive: &dyn Fn() -> bool,
) -> Lingered {
    let deadline = Instant::now() + Duration::from_millis(total_ms);
    let poll = Duration::from_millis(poll_ms.max(1));
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Lingered::Completed;
        }
        let slice = remaining.min(poll);
        match rx.recv_timeout(slice) {
            Ok(_) => return Lingered::Superseded,
            Err(RecvTimeoutError::Timeout) => {}
            // No signal will ever arrive again; keep the ownership and orphan
            // checks running on a timer rather than releasing early.
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(slice),
        }
        if !still_ours() {
            return Lingered::Superseded;
        }
        if !alive() {
            // Nobody is left to clear this pane for us. Release now instead of
            // holding the reservation for the rest of the window.
            return Lingered::Completed;
        }
    }
}

/// Idles until the shell signals us or dies, touching neither socket nor disk.
///
/// Every path that has nothing left to report ends here rather than returning.
/// `precmd` sends `kill -USR1 $_HSP_PID` whenever the command finishes, which
/// may be minutes later; a watcher that exited early leaves the shell holding a
/// PID the OS is free to hand to somebody else, and SIGUSR1's default
/// disposition is to terminate whoever receives it.
fn park(rx: &Receiver<i32>, poll_ms: u64, alive: &dyn Fn() -> bool) -> i32 {
    let poll = Duration::from_millis(poll_ms.max(1));
    loop {
        match rx.recv_timeout(poll) {
            Ok(_) => return 0,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(poll),
        }
        if !alive() {
            return 0;
        }
    }
}

struct Driver {
    pane: String,
    state_dir: PathBuf,
    socket: PathBuf,
    failures: u32,
    seq: u64,
}

impl Driver {
    fn marker_path(&self) -> PathBuf {
        self.state_dir.join("marker")
    }

    fn send(&mut self, method: &str, params: serde_json::Value) -> Result<(), SendError> {
        self.seq += 1;
        let id = format!("hsp-{}", self.seq);
        let result = socket::send(&self.socket, &id, method, params);
        match result {
            Ok(()) => self.failures = 0,
            Err(SendError::Io) => self.failures += 1,
            Err(SendError::PaneNotFound) => {}
        }
        result
    }

    /// Returns false when the driver should stop entirely.
    fn apply(&mut self, actions: Vec<Action>, linger: &mut dyn FnMut(u64) -> Lingered) -> bool {
        for action in actions {
            let outcome = match action {
                Action::ReportAgent {
                    state,
                    agent,
                    message,
                } => self.send(
                    "pane.report_agent",
                    proto::report_agent(&self.pane, &agent, state, message.as_deref()),
                ),
                Action::Metadata {
                    title,
                    label,
                    ttl_ms,
                    clear,
                    display_agent,
                } => self.send(
                    "pane.report_metadata",
                    proto::report_metadata(
                        &self.pane,
                        title,
                        label,
                        ttl_ms,
                        clear,
                        display_agent.as_deref(),
                    ),
                ),
                Action::Release { agent } => self.send(
                    "pane.release_agent",
                    proto::release_agent(&self.pane, &agent),
                ),
                Action::MarkerWrite { agent } => {
                    let _ = std::fs::write(self.marker_path(), &agent);
                    Ok(())
                }
                Action::MarkerRemove { agent } => {
                    remove_marker_owned_by(&self.marker_path(), &agent);
                    Ok(())
                }
                Action::Linger { ms } => match linger(ms) {
                    Lingered::Completed => Ok(()),
                    // Drop the release and the marker removal on the floor: a
                    // newer watcher owns this pane and both would corrupt it.
                    Lingered::Superseded => return false,
                },
                Action::Exit => return false,
            };

            match outcome {
                Err(SendError::PaneNotFound) => return false,
                Err(SendError::Io) if self.failures >= MAX_CONSECUTIVE_FAILURES => return false,
                _ => {}
            }
        }
        true
    }
}

/// For action lists that cannot contain `Linger`.
fn no_linger() -> impl FnMut(u64) -> Lingered {
    |_| Lingered::Completed
}

pub fn run(args: Args) -> i32 {
    let cfg = Config::load(config_dir().as_deref());
    let tick_ms = cfg.tick_ms;

    let cmd_line = read_trimmed(&args.state_dir.join("cmd")).unwrap_or_default();
    let agent = command::agent_name(&cmd_line);
    let title = command::truncate(&cmd_line, cfg.max_title_len);

    let mut driver = Driver {
        pane: args.pane.clone(),
        state_dir: args.state_dir.clone(),
        socket: socket_path(),
        failures: 0,
        seq: 0,
    };

    // A very fast command can deliver SIGUSR1 before this handler is installed.
    // The default disposition for SIGUSR1 is terminate, so the watcher dies
    // having reported nothing — which is exactly right for a fast command. Do
    // not "fix" this by blocking the signal; the race resolves correctly and any
    // change must preserve that outcome. Installing the handler this early is
    // safe because it preserves it deliberately: an early SIGUSR1 that we do
    // catch finds `reported == false` and exits just as silently.
    let (tx, rx) = mpsc::channel::<i32>();
    let mut signals = match signal_hook::iterator::Signals::new([SIGUSR1, SIGTERM]) {
        Ok(s) => s,
        // Nothing to install and nothing safe to do: without a handler this
        // process dies on the shell's SIGUSR1 anyway.
        Err(_) => return 0,
    };
    std::thread::spawn(move || {
        for sig in signals.forever() {
            if tx.send(sig).is_err() {
                break;
            }
        }
    });

    // Clear a sticky label from the previous command before anything else, even
    // for an ignored command — otherwise a stale label would outlive its welcome.
    let mut pane_usable = true;
    if args.clear_first {
        if let Some(prev_agent) = read_marker(&driver.marker_path()) {
            pane_usable = driver.apply(state::clear_actions(&prev_agent), &mut no_linger());
        }
    }

    // Ignored commands and dead panes have nothing left to say, but they must
    // still outlive `precmd`'s SIGUSR1. See `park`.
    if !pane_usable || cfg.is_ignored(&agent) {
        return park(&rx, tick_ms, &|| shell_alive(args.shell_pid));
    }

    let marker = driver.marker_path();
    let own_agent = own_agent_id();
    let mut machine = Machine::new(cfg, agent, title, args.start_ms);

    loop {
        let wait = machine.next_wake_ms(now_ms()).max(1);
        match rx.recv_timeout(Duration::from_millis(wait)) {
            Ok(SIGUSR1) => {
                let code = read_exit_code(&args.state_dir);
                let actions = machine.on_finish(now_ms(), code);
                driver.apply(actions, &mut |ms| {
                    linger(
                        &rx,
                        ms,
                        LINGER_POLL_MS,
                        &|| marker_names(&marker, &own_agent),
                        &|| shell_alive(args.shell_pid),
                    )
                });
                return 0;
            }
            Ok(_) => {
                driver.apply(machine.on_shell_gone(), &mut no_linger());
                return 0;
            }
            Err(RecvTimeoutError::Timeout) => {
                if !shell_alive(args.shell_pid) {
                    driver.apply(machine.on_shell_gone(), &mut no_linger());
                    return 0;
                }
                if !driver.apply(machine.on_tick(now_ms()), &mut no_linger()) {
                    // The pane is gone or the socket gave up. Nothing more can
                    // be reported, but the shell still holds our PID.
                    return park(&rx, tick_ms, &|| shell_alive(args.shell_pid));
                }
            }
            // The signal thread is gone, so the finish signal will never
            // arrive. Do not leave the pane parked at `working`.
            Err(RecvTimeoutError::Disconnected) => {
                driver.apply(machine.on_shell_gone(), &mut no_linger());
                return 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    /// The linger's ownership probe asks "does the marker still name me?", so
    /// the name it watches for MUST be the one the marker actually receives.
    /// These lived in two files and drifted: the marker started carrying the
    /// constant agent id while the probe still looked for the command basename,
    /// so the compare was permanently false, every linger reported Superseded on
    /// its first poll, and the queued `release_agent` was silently dropped —
    /// leaking the pane reservation on every successful slow command. Nothing
    /// tied the two values together, so all 95 tests passed regardless.
    #[test]
    fn the_linger_probe_watches_the_name_the_marker_actually_receives() {
        let mut m = Machine::new(Config::default(), "sleep".into(), "sleep 9".into(), 0);
        let written = m
            .on_tick(10_000)
            .into_iter()
            .find_map(|a| match a {
                Action::MarkerWrite { agent } => Some(agent),
                _ => None,
            })
            .expect("crossing the threshold writes a marker");
        assert_eq!(
            written,
            own_agent_id(),
            "the marker's contents and the linger probe must agree"
        );
    }

    /// Two manifests carry a version: Cargo's and Herdr's. Herdr shows its own
    /// in `plugin list`, so if they drift, a user reporting a bug names a
    /// version that does not correspond to the code they are running.
    #[test]
    fn the_cargo_and_plugin_manifest_versions_agree() {
        fn version_of(manifest: &str) -> &str {
            manifest
                .lines()
                .find_map(|l| l.strip_prefix("version = "))
                .expect("a version field")
                .trim_matches('"')
        }
        assert_eq!(
            version_of(include_str!("../Cargo.toml")),
            version_of(include_str!("../herdr-plugin.toml")),
            "Cargo.toml and herdr-plugin.toml must declare the same version"
        );
    }

    /// The README's install block hardcodes the plugin id inside a glob. Rename
    /// the plugin and that glob silently matches nothing — the install
    /// instructions then appear to succeed while doing absolutely nothing, which
    /// is exactly how the first published version shipped broken.
    #[test]
    fn the_readme_install_glob_matches_the_real_plugin_id() {
        let readme = include_str!("../README.md");
        let expected = format!("{PLUGIN_ID}-*/shell/init.zsh");
        assert!(
            readme.contains(&expected),
            "the README's source glob must contain `{expected}`"
        );
    }

    /// `herdr plugin action invoke` prints an invocation record on stdout and
    /// routes the action's own output to the plugin log. The README must not
    /// tell people to paste what that command "prints".
    #[test]
    fn the_readme_does_not_present_action_invoke_as_printing_the_snippet() {
        let readme = include_str!("../README.md");
        if let Some(idx) = readme.find("plugin action invoke") {
            let window = &readme[idx..readme.len().min(idx + 400)];
            assert!(
                window.contains("plugin log list"),
                "wherever action invoke is mentioned, say its output goes to the log"
            );
        }
    }

    #[test]
    fn env_value_set_and_nonempty_is_used_verbatim() {
        let dir = resolve_config_dir(Some("/custom/config/dir"), "/Users/whoever");
        assert_eq!(dir, PathBuf::from("/custom/config/dir"));
    }

    #[test]
    fn env_value_unset_falls_back_to_home_based_path() {
        let dir = resolve_config_dir(None, "/Users/whoever");
        assert_eq!(
            dir,
            PathBuf::from("/Users/whoever/.config/herdr/plugins/config/bayoudhi.shell-progress")
        );
    }

    #[test]
    fn env_value_empty_falls_back_to_home_based_path() {
        let dir = resolve_config_dir(Some(""), "/Users/whoever");
        assert_eq!(
            dir,
            PathBuf::from("/Users/whoever/.config/herdr/plugins/config/bayoudhi.shell-progress")
        );
    }

    #[test]
    fn the_plugin_id_matches_the_manifest() {
        let manifest = include_str!("../herdr-plugin.toml");
        // Only the top-level table: `[[actions]]` further down has its own `id`.
        let id = manifest
            .lines()
            .take_while(|l| !l.trim_start().starts_with('['))
            .find_map(|l| l.trim().strip_prefix("id"))
            .and_then(|rest| rest.trim().strip_prefix('='))
            .map(|v| v.trim().trim_matches('"'))
            .expect("herdr-plugin.toml must declare a top-level id");
        assert_eq!(
            id, PLUGIN_ID,
            "renaming the plugin id without updating PLUGIN_ID silently moves the config directory"
        );
    }

    // ---- marker ownership -------------------------------------------------

    fn state_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_marker_is_removed_only_by_the_agent_that_owns_it() {
        let dir = state_dir();
        let marker = dir.path().join("marker");
        std::fs::write(&marker, "npm").unwrap();

        assert!(!remove_marker_owned_by(&marker, "cargo"));
        assert!(
            marker.exists(),
            "a marker written by another watcher must survive"
        );

        assert!(remove_marker_owned_by(&marker, "npm"));
        assert!(!marker.exists());
    }

    #[test]
    fn removing_an_absent_marker_is_a_no_op() {
        let dir = state_dir();
        assert!(!remove_marker_owned_by(&dir.path().join("marker"), "cargo"));
    }

    #[test]
    fn a_trailing_newline_does_not_break_marker_ownership() {
        let dir = state_dir();
        let marker = dir.path().join("marker");
        std::fs::write(&marker, "cargo\n").unwrap();
        assert!(marker_names(&marker, "cargo"));
        assert!(remove_marker_owned_by(&marker, "cargo"));
    }

    #[test]
    fn an_empty_marker_is_removed_rather_than_left_to_force_clear_first() {
        let dir = state_dir();
        let marker = dir.path().join("marker");
        std::fs::write(&marker, "").unwrap();

        assert_eq!(read_marker(&marker), None);
        assert!(
            !marker.exists(),
            "an empty marker would switch --clear-first on forever"
        );
    }

    #[test]
    fn a_whitespace_only_marker_is_also_removed() {
        let dir = state_dir();
        let marker = dir.path().join("marker");
        std::fs::write(&marker, "\n  \n").unwrap();
        assert_eq!(read_marker(&marker), None);
        assert!(!marker.exists());
    }

    #[test]
    fn a_populated_marker_is_read_and_kept() {
        let dir = state_dir();
        let marker = dir.path().join("marker");
        std::fs::write(&marker, "cargo\n").unwrap();
        assert_eq!(read_marker(&marker), Some("cargo".to_string()));
        assert!(
            marker.exists(),
            "the clear path still has to release this agent"
        );
    }

    // ---- exit code --------------------------------------------------------

    #[test]
    fn a_missing_exit_file_reads_as_unknown_not_zero() {
        let dir = state_dir();
        assert_eq!(read_exit_code(dir.path()), None);
    }

    #[test]
    fn an_empty_or_unparseable_exit_file_reads_as_unknown() {
        let dir = state_dir();
        std::fs::write(dir.path().join("exit"), "").unwrap();
        assert_eq!(read_exit_code(dir.path()), None);
        std::fs::write(dir.path().join("exit"), "not-a-number").unwrap();
        assert_eq!(read_exit_code(dir.path()), None);
    }

    #[test]
    fn a_real_exit_file_reads_its_code() {
        let dir = state_dir();
        std::fs::write(dir.path().join("exit"), "1\n").unwrap();
        assert_eq!(read_exit_code(dir.path()), Some(1));
        std::fs::write(dir.path().join("exit"), "0\n").unwrap();
        assert_eq!(read_exit_code(dir.path()), Some(0));
    }

    // ---- linger -----------------------------------------------------------

    fn dead_channel() -> (mpsc::Sender<i32>, Receiver<i32>) {
        mpsc::channel()
    }

    #[test]
    fn a_signal_cuts_the_linger_short() {
        let (tx, rx) = dead_channel();
        tx.send(SIGTERM).unwrap();
        let started = Instant::now();
        let outcome = linger(&rx, 60_000, 10, &|| true, &|| true);
        assert_eq!(outcome, Lingered::Superseded);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "SIGTERM must not wait out the sticky window"
        );
    }

    #[test]
    fn a_finish_signal_also_cuts_the_linger_short() {
        let (tx, rx) = dead_channel();
        tx.send(SIGUSR1).unwrap();
        assert_eq!(
            linger(&rx, 60_000, 10, &|| true, &|| true),
            Lingered::Superseded
        );
    }

    #[test]
    fn losing_the_marker_cuts_the_linger_short_without_any_signal() {
        let (_tx, rx) = dead_channel();
        let started = Instant::now();
        let outcome = linger(&rx, 60_000, 5, &|| false, &|| true);
        assert_eq!(
            outcome,
            Lingered::Superseded,
            "the replacement watcher clears the marker; that alone must be enough"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn an_undisturbed_linger_runs_to_completion() {
        let (_tx, rx) = dead_channel();
        let started = Instant::now();
        assert_eq!(linger(&rx, 60, 5, &|| true, &|| true), Lingered::Completed);
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "it really waited"
        );
    }

    #[test]
    fn a_dead_shell_ends_the_linger_but_still_releases() {
        let (_tx, rx) = dead_channel();
        assert_eq!(
            linger(&rx, 60_000, 5, &|| true, &|| false),
            Lingered::Completed,
            "nobody is left to clear the pane, so hand the reservation back now"
        );
    }

    // ---- park -------------------------------------------------------------

    #[test]
    fn park_returns_when_signalled() {
        let (tx, rx) = dead_channel();
        tx.send(SIGUSR1).unwrap();
        assert_eq!(park(&rx, 10, &|| true), 0);
    }

    #[test]
    fn park_returns_when_the_shell_dies() {
        let (_tx, rx) = dead_channel();
        assert_eq!(park(&rx, 5, &|| false), 0);
    }

    #[test]
    fn park_idles_instead_of_exiting_while_the_shell_is_alive() {
        let (tx, rx) = dead_channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            let _ = tx.send(SIGUSR1);
        });
        let started = Instant::now();
        park(&rx, 5, &|| true);
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "exiting early would leave the shell signalling a recyclable PID"
        );
    }

    // ---- apply ------------------------------------------------------------

    /// Stands in for Herdr: one request per connection, then close. Records the
    /// method of every request it is sent.
    fn fake_server(dir: &Path) -> (PathBuf, Arc<Mutex<Vec<String>>>) {
        let path = dir.join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    break;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                    if let Some(m) = v["method"].as_str() {
                        sink.lock().unwrap().push(m.to_string());
                    }
                }
                let mut stream = reader.into_inner();
                let _ = stream.write_all(b"{\"id\":\"1\",\"result\":{\"type\":\"ok\"}}\n");
            }
        });
        (path, log)
    }

    fn driver_for(state: &Path, socket: PathBuf) -> Driver {
        Driver {
            pane: "w1:p2".into(),
            state_dir: state.into(),
            socket,
            failures: 0,
            seq: 0,
        }
    }

    /// Exactly what `on_finish` emits for a successful slow command.
    fn success_tail() -> Vec<Action> {
        vec![
            Action::Linger { ms: 20_000 },
            Action::Release {
                agent: "cargo".into(),
            },
            Action::MarkerRemove {
                agent: "cargo".into(),
            },
            Action::Exit,
        ]
    }

    #[test]
    fn a_superseded_linger_neither_releases_nor_removes_the_marker() {
        let sockdir = state_dir();
        let (socket, log) = fake_server(sockdir.path());
        let state = state_dir();
        let marker = state.path().join("marker");
        // The replacement watcher's marker, which happens to name a different
        // command. The stale watcher must leave it completely alone.
        std::fs::write(&marker, "npm").unwrap();
        let mut d = driver_for(state.path(), socket);

        let keep_going = d.apply(success_tail(), &mut |_| Lingered::Superseded);

        assert!(!keep_going, "a superseded watcher stops immediately");
        assert!(
            marker.exists(),
            "the marker belongs to the newer watcher now"
        );
        assert!(
            log.lock().unwrap().is_empty(),
            "releasing here would drop the live reservation mid-command"
        );
    }

    #[test]
    fn a_completed_linger_releases_and_removes_its_own_marker() {
        let sockdir = state_dir();
        let (socket, log) = fake_server(sockdir.path());
        let state = state_dir();
        let marker = state.path().join("marker");
        std::fs::write(&marker, "cargo").unwrap();
        let mut d = driver_for(state.path(), socket);

        let keep_going = d.apply(success_tail(), &mut |_| Lingered::Completed);

        assert!(!keep_going, "the action list ends in Exit");
        assert!(!marker.exists(), "our own marker goes away with us");
        assert_eq!(*log.lock().unwrap(), vec!["pane.release_agent".to_string()]);
    }

    #[test]
    fn apply_never_deletes_a_marker_another_watcher_wrote() {
        let sockdir = state_dir();
        let (socket, _log) = fake_server(sockdir.path());
        let state = state_dir();
        let marker = state.path().join("marker");
        std::fs::write(&marker, "npm").unwrap();
        let mut d = driver_for(state.path(), socket);

        d.apply(
            vec![Action::MarkerRemove {
                agent: "cargo".into(),
            }],
            &mut no_linger(),
        );

        assert!(
            marker.exists(),
            "without this guard the next command's sticky failure could never be cleared"
        );
    }
}
