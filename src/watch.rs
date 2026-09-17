use crate::args::{Args, Start};
use crate::command;
use crate::config::Config;
use crate::proto;
use crate::socket::{self, SendError};
use crate::state::{self, Action, Machine};
use signal_hook::consts::{SIGTERM, SIGUSR1};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

/// The name the ignore list matches on, and the identity the marker carries.
///
/// zsh hands `preexec` an alias-expanded command line, so parsing `cmd` finds
/// the program the user would think to ignore. bash cannot offer both halves at
/// once: `history 1` has the whole pipeline but no alias expansion, while
/// `$BASH_COMMAND` expands aliases and then stops at the first element of a
/// pipeline. Its hook writes each to the file that wants it — the line to
/// `cmd`, which feeds the row and the title, and the program to `name`.
///
/// The file is consumed on read. A pane usually runs one shell, but a nested
/// one inherits `HERDR_PANE_ID` and so shares this state directory: a `name`
/// left behind by a shell that writes them would be matched against the next
/// command of a shell that does not.
fn ignore_name(state_dir: &Path, cmd_line: &str) -> String {
    let path = state_dir.join("name");
    if let Some(name) = read_trimmed(&path) {
        let _ = std::fs::remove_file(&path);
        if !name.is_empty() {
            return command::agent_name(&name);
        }
    }
    command::agent_name(cmd_line)
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

/// How often the probe checks on a `keylock status` it is waiting for.
const LOCK_POLL_MS: u64 = 5;

/// Upper bound on how much of a finished `keylock status`'s stdout the probe
/// ever reads. A status line is a handful of bytes; this is only a backstop.
const LOCK_REPLY_CAP: usize = 8 * 1024;

/// Reads whatever is already buffered on `pipe` without blocking.
///
/// `keylock status` has already exited by the time this runs, so its own
/// write end of the pipe is closed — but a pipe only signals EOF once every
/// write end is closed, and a misbehaving `keylock` could leave a detached
/// descendant (an auto-started daemon, say) holding stdout open. A plain
/// `read_to_string` would then block forever, well past `timeout_ms`, and the
/// watcher would never return to its loop. Putting the fd in non-blocking mode
/// first means this drains only what is already sitting in the pipe buffer —
/// exactly what the exited child actually wrote — and returns the moment
/// nothing more is immediately available.
fn read_available(pipe: &mut std::process::ChildStdout) -> String {
    let fd = pipe.as_raw_fd();
    // SAFETY: `fd` is a valid, open file descriptor for the lifetime of this
    // call (it is borrowed from `pipe`, which outlives it); `fcntl` with
    // F_GETFL/F_SETFL on it is an ordinary, side-effect-local syscall.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while buf.len() < LOCK_REPLY_CAP {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(LOCK_REPLY_CAP - buf.len());
                buf.extend_from_slice(&chunk[..take]);
            }
            // Nothing more is buffered right now — that's the point of going
            // non-blocking, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// `keylock` to run, overridable with `HSP_KEYLOCK_BIN`.
fn keylock_bin() -> std::ffi::OsString {
    std::env::var_os("HSP_KEYLOCK_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "keylock".into())
}

/// Asks keylock whether this pane's session is locked.
///
/// Built only for a tracked `keylock` command, so an ordinary slow command
/// never spawns anything. Every failure keeps the previous answer: the row must
/// never stall or flicker because keylock was slow.
struct LockProbe {
    bin: std::ffi::OsString,
    pane: String,
    timeout: Duration,
    /// Cleared when keylock turns out not to be runnable at all.
    enabled: bool,
    locked: bool,
}

impl LockProbe {
    fn new(bin: std::ffi::OsString, pane: &str, cfg: &Config, agent: &str) -> Option<LockProbe> {
        if pane.is_empty() || cfg.lock.prefix.is_empty() || !crate::lock::is_keylock(agent) {
            return None;
        }
        Some(LockProbe {
            bin,
            pane: pane.to_string(),
            timeout: Duration::from_millis(cfg.lock.timeout_ms),
            enabled: true,
            locked: false,
        })
    }

    /// The lock state for this tick.
    fn poll(&mut self) -> bool {
        if !self.enabled {
            return self.locked;
        }
        let mut child = match Command::new(&self.bin)
            .arg("status")
            .arg("--pane")
            .arg(&self.pane)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                // A binary that cannot be found or executed now will not start
                // later either: stop paying for a spawn every tick.
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) {
                    self.enabled = false;
                }
                return self.locked;
            }
        };
        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return self.locked;
                    }
                    let stdout = match child.stdout.take() {
                        Some(mut pipe) => read_available(&mut pipe),
                        None => String::new(),
                    };
                    if let Some(locked) = crate::lock::parse_status(&stdout) {
                        self.locked = locked;
                    }
                    return self.locked;
                }
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return self.locked;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(LOCK_POLL_MS)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return self.locked;
                }
            }
        }
    }
}

pub fn run(args: Args) -> i32 {
    // Read the clock before anything else: a shell that passed `--start-now`
    // has no start of its own, so every millisecond spent below would otherwise
    // be charged to the command rather than to this process.
    let start_ms = match args.start {
        Start::At(ms) => ms,
        Start::Now => now_ms(),
    };

    let cfg = Config::load(config_dir().as_deref());
    let tick_ms = cfg.tick_ms;

    let cmd_line = read_trimmed(&args.state_dir.join("cmd")).unwrap_or_default();
    let agent = ignore_name(&args.state_dir, &cmd_line);
    let title = command::truncate(&cmd_line, cfg.max_title_len);
    let display = command::display_name(&cmd_line, cfg.max_display_len);

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
    let mut lock_probe = LockProbe::new(keylock_bin(), &args.pane, &cfg, &agent);
    let mut machine = Machine::new(cfg, agent, title, display, start_ms);

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
                if let Some(probe) = lock_probe.as_mut() {
                    machine.set_locked(probe.poll());
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
    use std::os::unix::fs::PermissionsExt;
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
        let mut m = Machine::new(
            Config::default(),
            "sleep".into(),
            "sleep 9".into(),
            "sleep 9".into(),
            0,
        );
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

    /// `[ui.sidebar.agents.rows_by_agent]` is validated against Herdr's own
    /// canonical agent ids and rejects anything else — by refusing to parse the
    /// whole config file, which drops the user's entire Herdr configuration to
    /// defaults. The README once told people to add a `shell` key there and
    /// broke a real config. Never ship that advice again.
    #[test]
    fn the_readme_never_tells_users_to_add_a_rows_by_agent_rule() {
        let readme = include_str!("../README.md");
        for line in readme.lines() {
            let is_snippet = line
                .trim_start()
                .starts_with(&format!("{} =", proto::AGENT_ID))
                || line.contains(&format!("\n{} =", proto::AGENT_ID));
            assert!(
                !is_snippet,
                "README appears to define a rows_by_agent rule for `{}`: {line}",
                proto::AGENT_ID
            );
        }
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

    // ---- the ignore name a shell may supply -------------------------------

    #[test]
    fn the_ignore_name_comes_from_the_command_line_by_default() {
        let dir = state_dir();
        assert_eq!(ignore_name(dir.path(), "npm run build | tee log"), "npm");
    }

    #[test]
    fn a_shell_written_name_file_wins_over_the_command_line() {
        let dir = state_dir();
        std::fs::write(dir.path().join("name"), "claude\n").unwrap();
        assert_eq!(ignore_name(dir.path(), "cc --resume"), "claude");
    }

    /// The hook hands over a whole command rather than a guess at its head, so
    /// that `VAR=value` assignments and transparent wrappers are skipped here,
    /// by the parser that already knows how.
    #[test]
    fn a_name_file_is_reduced_to_the_program_it_names() {
        let dir = state_dir();
        std::fs::write(dir.path().join("name"), "env FOO=1 npm run build").unwrap();
        assert_eq!(ignore_name(dir.path(), "irrelevant"), "npm");
    }

    #[test]
    fn a_name_file_is_reduced_to_its_basename() {
        let dir = state_dir();
        std::fs::write(dir.path().join("name"), "/opt/homebrew/bin/npm").unwrap();
        assert_eq!(ignore_name(dir.path(), "irrelevant"), "npm");
    }

    #[test]
    fn an_empty_name_file_falls_back_to_the_command_line() {
        let dir = state_dir();
        std::fs::write(dir.path().join("name"), "   \n").unwrap();
        assert_eq!(ignore_name(dir.path(), "cargo build"), "cargo");
    }

    #[test]
    fn a_name_file_is_consumed_so_it_cannot_go_stale() {
        let dir = state_dir();
        let name = dir.path().join("name");
        std::fs::write(&name, "claude").unwrap();

        assert_eq!(ignore_name(dir.path(), "cargo build"), "claude");
        assert!(
            !name.exists(),
            "a name left behind would rename the next command from a shell that writes none"
        );
        assert_eq!(ignore_name(dir.path(), "cargo build"), "cargo");
    }

    // ---- keylock probe ------------------------------------------------------

    /// Writes a fake `keylock` that prints `body` and exits `code`.
    ///
    /// macOS validates a newly created executable on its first run, which can
    /// take hundreds of milliseconds and would land inside the probe's timeout;
    /// Linux refuses to exec a file another process still holds open for
    /// writing. So the script is run once here, retrying while it reports
    /// `ExecutableFileBusy`.
    fn fake_keylock(dir: &Path, body: &str) -> std::ffi::OsString {
        let bin = dir.join("keylock");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\n[ -n \"$HSP_FAKE_WARMUP\" ] && exit 0\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        for attempt in 0..50 {
            match Command::new(&bin).env("HSP_FAKE_WARMUP", "1").status() {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 49 => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("warming up the fake keylock failed: {e}"),
            }
        }
        bin.into_os_string()
    }

    fn lock_cfg(prefix: &str, timeout_ms: u64) -> Config {
        let mut cfg = Config::default();
        cfg.lock.prefix = prefix.to_string();
        cfg.lock.timeout_ms = timeout_ms;
        cfg
    }

    #[test]
    fn the_probe_is_built_only_for_a_keylock_command_in_a_pane() {
        let cfg = lock_cfg("🔒 ", 300);
        let bin = std::ffi::OsString::from("keylock");
        assert!(LockProbe::new(bin.clone(), "w1:p1", &cfg, "keylock").is_some());
        assert!(LockProbe::new(bin.clone(), "w1:p1", &cfg, "cargo").is_none());
        assert!(LockProbe::new(bin.clone(), "", &cfg, "keylock").is_none());
        assert!(LockProbe::new(bin, "w1:p1", &lock_cfg("", 300), "keylock").is_none());
    }

    #[test]
    fn the_probe_reads_keylocks_answer() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_keylock(dir.path(), "echo 'locked pid=1 cmd=./m.sh'");
        let mut probe = LockProbe::new(bin, "w1:p1", &lock_cfg("🔒 ", 300), "keylock").unwrap();
        assert!(probe.poll());

        let dir = tempfile::tempdir().unwrap();
        let bin = fake_keylock(dir.path(), "echo 'unlocked pid=1 cmd=./m.sh'");
        let mut probe = LockProbe::new(bin, "w1:p1", &lock_cfg("🔒 ", 300), "keylock").unwrap();
        assert!(!probe.poll());
    }

    #[test]
    fn the_probe_passes_the_pane_to_keylock() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let bin = fake_keylock(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$*\" >> '{}'; echo 'locked pid=1 cmd=x'",
                log.display()
            ),
        );
        let mut probe = LockProbe::new(bin, "w9:p3", &lock_cfg("🔒 ", 300), "keylock").unwrap();
        probe.poll();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "status --pane w9:p3"
        );
    }

    #[test]
    fn an_unreadable_answer_keeps_the_previous_state() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_keylock(dir.path(), "echo 'locked pid=1 cmd=x'");
        let mut probe = LockProbe::new(bin, "w1:p1", &lock_cfg("🔒 ", 300), "keylock").unwrap();
        assert!(probe.poll(), "locked to begin with");

        let dir = tempfile::tempdir().unwrap();
        let failing = fake_keylock(
            dir.path(),
            "echo 'keylock: no session in pane w1:p1' >&2; exit 1",
        );
        probe.bin = failing;
        assert!(probe.poll(), "a failed check keeps the last known state");
    }

    /// Exit 0 with output `parse_status` cannot read — usage text from a
    /// keylock older than 0.2.0 is exactly this shape — is the third fallback
    /// the spec names: it must not be read as unlocked, only as "unknown".
    #[test]
    fn output_that_does_not_parse_also_keeps_the_previous_state() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_keylock(dir.path(), "echo 'locked pid=1 cmd=x'");
        let mut probe = LockProbe::new(bin, "w1:p1", &lock_cfg("🔒 ", 300), "keylock").unwrap();
        assert!(probe.poll(), "locked to begin with");

        let dir = tempfile::tempdir().unwrap();
        let unparsable = fake_keylock(
            dir.path(),
            "echo 'keylock 0.1.0 -- usage: keylock run [--locked] ...'",
        );
        probe.bin = unparsable;
        assert!(
            probe.poll(),
            "an exit-0 reply parse_status can't read keeps the last known state"
        );
    }

    #[test]
    fn a_slow_keylock_is_killed_and_the_state_stands() {
        let dir = tempfile::tempdir().unwrap();
        let locked = fake_keylock(dir.path(), "echo 'locked pid=1 cmd=x'");
        let mut probe = LockProbe::new(locked, "w1:p1", &lock_cfg("🔒 ", 100), "keylock").unwrap();
        assert!(probe.poll(), "locked to begin with");

        let dir = tempfile::tempdir().unwrap();
        let slow = fake_keylock(dir.path(), "exec sleep 10");
        probe.bin = slow;
        let start = Instant::now();
        assert!(
            probe.poll(),
            "a timeout keeps the last known state, not false"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }

    /// A pipe only reaches EOF once every write end is closed. If `keylock
    /// status` ever left a detached descendant holding stdout open (an
    /// auto-started daemon, say), a plain `read_to_string` after the parent
    /// exits would block on that descendant forever. The probe must instead
    /// read only what the parent actually wrote before it exited, and return
    /// promptly regardless of who else still holds the pipe open.
    #[test]
    fn a_child_holding_stdout_open_does_not_block_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_keylock(dir.path(), "echo 'locked pid=1 cmd=x'; sleep 30 &");
        let mut probe = LockProbe::new(bin, "w1:p1", &lock_cfg("🔒 ", 300), "keylock").unwrap();
        let start = Instant::now();
        assert!(
            probe.poll(),
            "the status line is read even though a descendant still holds stdout open"
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "must not block on the lingering child: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_missing_keylock_switches_the_probe_off() {
        let mut probe = LockProbe::new(
            std::ffi::OsString::from("/nonexistent/keylock"),
            "w1:p1",
            &lock_cfg("🔒 ", 300),
            "keylock",
        )
        .unwrap();
        assert!(!probe.poll());
        assert!(!probe.enabled, "one failed spawn, not one per tick");
    }
}
