//! Drives each supported shell interactively under a pty and asserts what its
//! hook did.
//!
//! The hooks are the half of this plugin that cannot be unit tested: what they
//! are for is being wired into a live shell, and the failures that matter are
//! wiring failures. bash's `DEBUG` trap firing once per pipeline element rather
//! than once per command line is not visible anywhere except in a real session.
//!
//! So each test starts a real shell on a real pty, types at it, and reads back
//! what the hook spawned. `HSP_BIN` points at a stub standing in for the
//! watcher: it records its own argv and the state files the hook wrote, waits
//! for the `SIGUSR1` that `precmd` promises, and records the exit code it finds.
//!
//! A shell that is not installed skips rather than fails. zsh is the control:
//! it is already known to work, so a harness that cannot make zsh pass is
//! measuring itself rather than the hook.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Sessions run one at a time. Each one paces its typing against a real shell
/// starting up, and several competing for the machine at once turn those pauses
/// into dropped input — a flake that looks exactly like a broken hook.
static SESSION: Mutex<()> = Mutex::new(());

/// Stands in for the watcher. Records the invocation, then lingers just long
/// enough to catch the signal `precmd` sends when the command finishes.
///
/// Traps `SIGUSR1` as its first act. The default disposition is to terminate,
/// and `precmd` fires the moment the command returns — the same race the real
/// watcher guards against, and the reason every command a test types is slow
/// enough for the stub to be up before the signal lands.
///
/// Sleeps in short steps because a POSIX shell runs a trap only once the
/// foreground command returns: one long sleep would hold the handler for its
/// whole duration.
const STUB: &str = r#"#!/bin/sh
exec 0</dev/null
state=""
prev=""
for a in "$@"; do
  [ "$prev" = "--state-dir" ] && state="$a"
  prev="$a"
done
trap 'printf "signal %s\n" "$(cat "$state/exit" 2>/dev/null)" >> "$HSP_TEST_LOG"; exit 0' USR1
{
  printf 'spawn %s\n' "$*"
  printf 'cmd %s\n' "$(cat "$state/cmd" 2>/dev/null)"
  if [ -f "$state/name" ]; then printf 'name %s\n' "$(cat "$state/name")"; fi
} >> "$HSP_TEST_LOG"
i=0
while [ $i -lt 12 ]; do sleep 0.1; i=$((i + 1)); done
"#;

/// Long enough that the stub is running before `precmd` signals it, short
/// enough to keep the suite quick. Every command a test types must outlast a
/// process spawn, or its watcher dies before recording anything.
const SLOW: &str = "sleep 0.3";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    fn binary(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::Fish => "fish",
        }
    }

    fn init_file(self) -> &'static str {
        match self {
            Shell::Zsh => "init.zsh",
            Shell::Bash => "init.bash",
            Shell::Fish => "init.fish",
        }
    }

    fn installed(self) -> bool {
        which(self.binary()).is_some()
    }

    /// The argv that starts this shell interactively, reading our init file and
    /// nothing of the user's.
    fn argv(self, home: &Path, init: &Path) -> Vec<String> {
        let s = |p: &Path| p.display().to_string();
        match self {
            // --no-globalrcs keeps /etc/zshrc out; ZDOTDIR supplies the rest.
            Shell::Zsh => vec!["--no-globalrcs".into(), "-i".into()],
            Shell::Bash => vec![
                "--noprofile".into(),
                "--rcfile".into(),
                s(&home.join("bashrc")),
                "-i".into(),
            ],
            // fish has no rcfile flag; -C runs before the first prompt, and an
            // empty XDG_CONFIG_HOME keeps the user's own config out.
            Shell::Fish => vec!["-i".into(), "-C".into(), format!("source {}", s(init))],
        }
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(bin))
            .find(|p| p.is_file())
    })
}

/// One recorded watcher spawn.
#[derive(Debug)]
struct Spawn {
    argv: String,
    cmd: String,
    name: Option<String>,
    exit: Option<String>,
}

impl Spawn {
    fn clears_first(&self) -> bool {
        self.argv.split_whitespace().any(|a| a == "--clear-first")
    }
}

struct Session {
    spawns: Vec<Spawn>,
    state_dir: PathBuf,
    _home: tempfile::TempDir,
}

/// Runs `commands` in an interactive `shell` and returns what the hook did.
///
/// `before` gets the state directory before the shell starts, for tests that
/// need a marker in place.
fn run(shell: Shell, commands: &[&str], before: impl FnOnce(&Path)) -> Session {
    run_with_prelude(shell, commands, "", before)
}

/// As `run`, but sources `prelude` ahead of the hook. Used to put another
/// DEBUG-trap owner in place first.
fn run_with_prelude(
    shell: Shell,
    commands: &[&str],
    prelude: &str,
    before: impl FnOnce(&Path),
) -> Session {
    // A test that panicked while holding the lock has nothing to corrupt here:
    // every session owns a fresh temporary directory.
    let _guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let init = repo.join("shell").join(shell.init_file());
    assert!(
        init.is_file(),
        "no hook for {}: expected {}",
        shell.binary(),
        init.display()
    );

    let home = tempfile::tempdir().unwrap();
    let log = home.path().join("log");
    std::fs::write(&log, "").unwrap();
    let ready = home.path().join("hsp-ready");

    let stub = home.path().join("stub");
    std::fs::write(&stub, STUB).unwrap();
    make_executable(&stub);

    // Mirrors what the hooks compute: HERDR_PLUGIN_STATE_DIR plus the pane id
    // with its colons flattened.
    let pane = "t1:p1";
    let state_root = home.path().join("state");
    let state_dir = state_root.join(pane.replace(':', "-"));
    std::fs::create_dir_all(&state_dir).unwrap();
    before(&state_dir);

    // zsh finds its rc through ZDOTDIR, bash through --rcfile, fish through -C.
    let rc = format!("{prelude}\nsource {}\n", init.display());
    std::fs::write(home.path().join(".zshrc"), &rc).unwrap();
    std::fs::write(home.path().join("bashrc"), &rc).unwrap();

    let mut child = pty_command(shell, home.path(), &init)
        .env("HOME", home.path())
        .env("ZDOTDIR", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("empty-config"))
        .env("HERDR_PANE_ID", pane)
        .env("HERDR_PLUGIN_STATE_DIR", &state_root)
        .env("HSP_BIN", &stub)
        .env("HSP_TEST_LOG", &log)
        .env("HSP_READY", &ready)
        .env("PS1", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("could not start {}: {e}", shell.binary()));

    {
        // A shell flushes its input queue when it takes over the terminal, so
        // anything typed before the first prompt is drawn is simply lost. Wait
        // for proof that it is running commands rather than guessing at how
        // long it takes to start: under a loaded machine any guess is wrong.
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "touch $HSP_READY").unwrap();
        stdin.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "{} never reached a prompt",
                shell.binary()
            );
            std::thread::sleep(Duration::from_millis(25));
        }

        for c in commands {
            writeln!(stdin, "{c}").unwrap();
            stdin.flush().unwrap();
            std::thread::sleep(Duration::from_millis(400));
        }
        // The session has to be ended from the inside: closing stdin leaves
        // script(1) waiting on a pty that the lingering stubs still hold open.
        writeln!(stdin, "exit").unwrap();
    }

    child.wait().unwrap();
    settle(&log);

    Session {
        spawns: parse_log(&std::fs::read_to_string(&log).unwrap()),
        state_dir,
        _home: home,
    }
}

/// Wraps the shell in a pty. Interactive hooks do not run without one:
/// `precmd` fires when a prompt is drawn, and a shell reading a pipe draws none.
fn pty_command(shell: Shell, home: &Path, init: &Path) -> Command {
    let bin = shell.binary();
    let args = shell.argv(home, init);

    let mut c = Command::new("script");
    if cfg!(target_os = "macos") {
        // script -q /dev/null <command...>
        c.arg("-q").arg("/dev/null").arg(bin).args(&args);
    } else {
        // util-linux takes the command as one string: script -qec "<cmd>" /dev/null
        let quoted = std::iter::once(bin.to_string())
            .chain(
                args.iter()
                    .map(|a| format!("'{}'", a.replace('\'', r"'\''"))),
            )
            .collect::<Vec<_>>()
            .join(" ");
        c.arg("-qec").arg(quoted).arg("/dev/null");
    }
    c
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Waits for the stubs to stop writing. They outlive the shell by design, so
/// the last `signal` line can land after the session has already ended.
fn settle(log: &Path) {
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut last = usize::MAX;
    let mut stable_since = Instant::now();
    while Instant::now() < deadline {
        let size = std::fs::metadata(log)
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        if size != last {
            last = size;
            stable_since = Instant::now();
        } else if stable_since.elapsed() > Duration::from_millis(300) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn parse_log(text: &str) -> Vec<Spawn> {
    let mut out: Vec<Spawn> = Vec::new();
    for line in text.lines() {
        let (tag, rest) = match line.split_once(' ') {
            Some((t, r)) => (t, r.to_string()),
            None => (line, String::new()),
        };
        match tag {
            "spawn" => out.push(Spawn {
                argv: rest,
                cmd: String::new(),
                name: None,
                exit: None,
            }),
            "cmd" => {
                if let Some(s) = out.last_mut() {
                    s.cmd = rest;
                }
            }
            "name" => {
                if let Some(s) = out.last_mut() {
                    s.name = Some(rest);
                }
            }
            // A stub's signal belongs to the spawn it was started for, which is
            // the most recent one that has not been signalled yet.
            "signal" => {
                if let Some(s) = out.iter_mut().rev().find(|s| s.exit.is_none()) {
                    s.exit = Some(rest);
                }
            }
            _ => {}
        }
    }
    out
}

/// Spawns raised by the commands a test typed.
///
/// Three kinds of spawn are not among them. The readiness probe is the
/// harness's own. The `exit` that ends every session races the shell's
/// teardown: its watcher is sometimes killed before recording anything and
/// sometimes after, occasionally catching the state files mid-write. And an
/// empty `cmd` is that same teardown race caught mid-write.
fn tracked(session: &Session) -> Vec<&Spawn> {
    session
        .spawns
        .iter()
        .filter(|s| {
            let cmd = s.cmd.trim();
            !cmd.is_empty() && cmd != "exit" && !cmd.contains("HSP_READY")
        })
        .collect()
}

// ---- the tests each shell must pass ---------------------------------------

fn skip_unless_installed(shell: Shell) -> bool {
    if shell.installed() {
        return false;
    }
    eprintln!("skipping: {} is not installed", shell.binary());
    true
}

fn one_watcher_per_command_line(shell: Shell) {
    if skip_unless_installed(shell) {
        return;
    }
    let session = run(shell, &[SLOW, &format!("{SLOW} | true"), SLOW], |_| {});
    let spawns = tracked(&session);

    assert_eq!(
        spawns.len(),
        3,
        "expected one watcher per command line, got {}: {:#?}",
        spawns.len(),
        spawns
    );
}

fn the_whole_command_line_reaches_the_watcher(shell: Shell) {
    if skip_unless_installed(shell) {
        return;
    }
    let piped = format!("{SLOW} | true");
    let session = run(shell, &[&piped], |_| {});
    let spawns = tracked(&session);

    assert_eq!(spawns.len(), 1, "{:#?}", spawns);
    assert_eq!(
        spawns[0].cmd, piped,
        "the sidebar row shows the command line, so the hook must write all of it"
    );
}

fn the_exit_code_is_reported_to_the_watcher(shell: Shell) {
    if skip_unless_installed(shell) {
        return;
    }
    let session = run(shell, &[&format!("{SLOW}; false")], |_| {});
    let spawns = tracked(&session);

    assert_eq!(spawns.len(), 1, "{:#?}", spawns);
    assert_eq!(
        spawns[0].exit.as_deref(),
        Some("1"),
        "a failure must arrive as a failure: {:#?}",
        spawns
    );
}

fn a_watcher_is_started_with_the_right_pane_and_state_dir(shell: Shell) {
    if skip_unless_installed(shell) {
        return;
    }
    let session = run(shell, &[SLOW], |_| {});
    let spawns = tracked(&session);

    assert_eq!(spawns.len(), 1, "{:#?}", spawns);
    let argv = &spawns[0].argv;
    assert!(argv.starts_with("watch "), "{argv}");
    assert!(argv.contains("--pane t1:p1"), "{argv}");
    assert!(
        argv.contains(&format!("--state-dir {}", session.state_dir.display())),
        "{argv}"
    );
    assert!(
        argv.contains("--shell-pid "),
        "the watcher gives up when the shell dies, so it needs the pid: {argv}"
    );
    assert!(
        argv.contains("--start-ms ") || argv.contains("--start-now"),
        "{argv}"
    );
}

fn clear_first_is_passed_only_when_a_marker_exists(shell: Shell) {
    if skip_unless_installed(shell) {
        return;
    }
    let without = run(shell, &[SLOW], |_| {});
    assert_eq!(tracked(&without).len(), 1);
    assert!(
        !tracked(&without)[0].clears_first(),
        "a clear costs a socket round trip and there is no label to clear"
    );

    let with = run(shell, &[SLOW], |state| {
        std::fs::write(state.join("marker"), "cargo").unwrap();
    });
    assert_eq!(tracked(&with).len(), 1);
    assert!(
        tracked(&with)[0].clears_first(),
        "a sticky failure label must be wiped by the next command"
    );
}

mod zsh {
    use super::*;

    #[test]
    fn spawns_one_watcher_per_command_line() {
        one_watcher_per_command_line(Shell::Zsh);
    }

    #[test]
    fn writes_the_whole_command_line() {
        the_whole_command_line_reaches_the_watcher(Shell::Zsh);
    }

    #[test]
    fn reports_the_exit_code() {
        the_exit_code_is_reported_to_the_watcher(Shell::Zsh);
    }

    #[test]
    fn starts_the_watcher_with_the_pane_and_state_dir() {
        a_watcher_is_started_with_the_right_pane_and_state_dir(Shell::Zsh);
    }

    #[test]
    fn clears_a_sticky_label_only_when_one_exists() {
        clear_first_is_passed_only_when_a_marker_exists(Shell::Zsh);
    }
}

mod bash {
    use super::*;

    #[test]
    fn spawns_one_watcher_per_command_line() {
        one_watcher_per_command_line(Shell::Bash);
    }

    #[test]
    fn writes_the_whole_command_line() {
        the_whole_command_line_reaches_the_watcher(Shell::Bash);
    }

    #[test]
    fn reports_the_exit_code() {
        the_exit_code_is_reported_to_the_watcher(Shell::Bash);
    }

    #[test]
    fn starts_the_watcher_with_the_pane_and_state_dir() {
        a_watcher_is_started_with_the_right_pane_and_state_dir(Shell::Bash);
    }

    #[test]
    fn clears_a_sticky_label_only_when_one_exists() {
        clear_first_is_passed_only_when_a_marker_exists(Shell::Bash);
    }

    /// bash is the only shell that writes a `name`: it cannot get the whole
    /// pipeline and the alias-expanded program out of the same source.
    #[test]
    fn names_the_expanded_leading_command_alongside_the_command_line() {
        if skip_unless_installed(Shell::Bash) {
            return;
        }
        let session = run(
            Shell::Bash,
            &["alias hsptest='sleep 0.3'", "hsptest | true"],
            |_| {},
        );
        let spawns = tracked(&session);
        let piped = spawns
            .iter()
            .find(|s| s.cmd.contains('|'))
            .unwrap_or_else(|| panic!("no piped command recorded: {spawns:#?}"));

        assert_eq!(
            piped.cmd, "hsptest | true",
            "the row shows the line as typed"
        );
        assert_eq!(
            piped.name.as_deref(),
            Some("sleep 0.3"),
            "the whole expanded command is handed over, for the watcher to \
             reduce the same way it reduces a command line: {piped:#?}"
        );
    }
}

/// A stand-in for bash-preexec, following the contract the real one documents:
/// it owns the DEBUG trap, hands `preexec_functions` the whole command line,
/// and restores `$?` before running `precmd_functions`.
const BASH_PREEXEC_SHIM: &str = r#"
preexec_functions=()
precmd_functions=()
__bp_armed=0
__bp_set_ret_value() { return ${1:-0}; }
__bp_debug() {
  [[ "$__bp_armed" == 1 ]] || return 0
  __bp_armed=0
  local line
  line="$(HISTTIMEFORMAT= history 1)"
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line#* }"
  line="${line#"${line%%[![:space:]]*}"}"
  local f
  for f in "${preexec_functions[@]}"; do "$f" "$line"; done
}
__bp_precmd() {
  local ret=$?
  local f
  for f in "${precmd_functions[@]}"; do
    __bp_set_ret_value "$ret"
    "$f"
  done
  __bp_armed=1
}
trap '__bp_debug' DEBUG
PROMPT_COMMAND='__bp_precmd'
"#;

mod bash_preexec {
    use super::*;

    /// Atuin and iTerm2's shell integration both load bash-preexec, and it owns
    /// the DEBUG trap. Installing a second one would start two watchers for
    /// every command.
    #[test]
    fn hooks_into_bash_preexec_rather_than_taking_its_debug_trap() {
        if skip_unless_installed(Shell::Bash) {
            return;
        }
        let session = run_with_prelude(
            Shell::Bash,
            &[SLOW, &format!("{SLOW} | true")],
            BASH_PREEXEC_SHIM,
            |_| {},
        );
        let spawns = tracked(&session);

        assert_eq!(
            spawns.len(),
            2,
            "one watcher per command line, not one per DEBUG trap: {spawns:#?}"
        );
    }

    #[test]
    fn still_reports_the_exit_code_through_bash_preexec() {
        if skip_unless_installed(Shell::Bash) {
            return;
        }
        let session = run_with_prelude(
            Shell::Bash,
            &[&format!("{SLOW}; false")],
            BASH_PREEXEC_SHIM,
            |_| {},
        );
        let spawns = tracked(&session);

        assert_eq!(spawns.len(), 1, "{spawns:#?}");
        assert_eq!(
            spawns[0].exit.as_deref(),
            Some("1"),
            "bash-preexec restores $? for precmd functions: {spawns:#?}"
        );
    }
}

mod fish {
    use super::*;

    #[test]
    fn spawns_one_watcher_per_command_line() {
        one_watcher_per_command_line(Shell::Fish);
    }

    #[test]
    fn writes_the_whole_command_line() {
        the_whole_command_line_reaches_the_watcher(Shell::Fish);
    }

    #[test]
    fn reports_the_exit_code() {
        the_exit_code_is_reported_to_the_watcher(Shell::Fish);
    }

    #[test]
    fn starts_the_watcher_with_the_pane_and_state_dir() {
        a_watcher_is_started_with_the_right_pane_and_state_dir(Shell::Fish);
    }

    #[test]
    fn clears_a_sticky_label_only_when_one_exists() {
        clear_first_is_passed_only_when_a_marker_exists(Shell::Fish);
    }
}
