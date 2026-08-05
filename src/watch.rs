use crate::args::Args;
use crate::config::Config;
use crate::proto;
use crate::socket::{self, SendError};
use crate::command;
use crate::state::{self, Action, Machine};
use signal_hook::consts::{SIGTERM, SIGUSR1};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Give up after this many consecutive socket failures.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;

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

/// Must match `id` in herdr-plugin.toml.
const PLUGIN_ID: &str = "hamza.shell-progress";

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
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
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
    fn apply(&mut self, actions: Vec<Action>) -> bool {
        for action in actions {
            let outcome = match action {
                Action::ReportAgent { state, agent, message } => self.send(
                    "pane.report_agent",
                    proto::report_agent(&self.pane, &agent, state, message.as_deref()),
                ),
                Action::Metadata { title, label, ttl_ms, clear } => self.send(
                    "pane.report_metadata",
                    proto::report_metadata(&self.pane, title, label, ttl_ms, clear),
                ),
                Action::Release { agent } => {
                    self.send("pane.release_agent", proto::release_agent(&self.pane, &agent))
                }
                Action::MarkerWrite { agent } => {
                    let _ = std::fs::write(self.marker_path(), &agent);
                    Ok(())
                }
                Action::MarkerRemove => {
                    let _ = std::fs::remove_file(self.marker_path());
                    Ok(())
                }
                Action::Linger { ms } => {
                    std::thread::sleep(Duration::from_millis(ms));
                    Ok(())
                }
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

pub fn run(args: Args) -> i32 {
    let cfg = Config::load(config_dir().as_deref());

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

    // Clear a sticky label from the previous command before anything else, even
    // for an ignored command — otherwise a stale label would outlive its welcome.
    if args.clear_first {
        if let Some(prev_agent) = read_trimmed(&driver.marker_path()) {
            if !prev_agent.is_empty() {
                driver.apply(state::clear_actions(&prev_agent));
            }
        }
    }

    if cfg.is_ignored(&agent) {
        return 0;
    }

    // A very fast command can deliver SIGUSR1 before this handler is installed.
    // The default disposition for SIGUSR1 is terminate, so the watcher dies
    // having reported nothing — which is exactly right for a fast command. Do
    // not "fix" this by installing the handler earlier or blocking the signal;
    // the race resolves correctly and any change must preserve that outcome.
    let (tx, rx) = mpsc::channel::<i32>();
    let mut signals = match signal_hook::iterator::Signals::new([SIGUSR1, SIGTERM]) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    std::thread::spawn(move || {
        for sig in signals.forever() {
            if tx.send(sig).is_err() {
                break;
            }
        }
    });

    let mut machine = Machine::new(cfg, agent, title, args.start_ms);

    loop {
        let wait = machine.next_wake_ms(now_ms()).max(1);
        match rx.recv_timeout(Duration::from_millis(wait)) {
            Ok(SIGUSR1) => {
                let code = read_trimmed(&args.state_dir.join("exit"))
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(0);
                driver.apply(machine.on_finish(now_ms(), code));
                return 0;
            }
            Ok(_) => {
                driver.apply(machine.on_shell_gone());
                return 0;
            }
            Err(RecvTimeoutError::Timeout) => {
                if !shell_alive(args.shell_pid) {
                    driver.apply(machine.on_shell_gone());
                    return 0;
                }
                if !driver.apply(machine.on_tick(now_ms())) {
                    return 0;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            PathBuf::from("/Users/whoever/.config/herdr/plugins/config/hamza.shell-progress")
        );
    }

    #[test]
    fn env_value_empty_falls_back_to_home_based_path() {
        let dir = resolve_config_dir(Some(""), "/Users/whoever");
        assert_eq!(
            dir,
            PathBuf::from("/Users/whoever/.config/herdr/plugins/config/hamza.shell-progress")
        );
    }
}
