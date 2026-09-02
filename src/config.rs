use serde::Deserialize;
use std::path::Path;

/// Floor for every interval the driver waits on. `tick_ms = 0` would otherwise
/// turn the elapsed ticker into roughly a thousand socket connects per second
/// for as long as the command runs; `threshold_ms = 0` would report every `ls`.
/// A quarter second is faster than anyone can read and still costs nothing.
pub const MIN_INTERVAL_MS: u64 = 250;

/// Floor for the sidebar row name. `max_display_len = 0` would truncate every
/// row to nothing, leaving a pane reporting `working` with no name attached to
/// it. Eight characters is cramped but still identifies a command.
pub const MIN_DISPLAY_LEN: usize = 8;

fn default_threshold_ms() -> u64 {
    2000
}
fn default_tick_ms() -> u64 {
    2000
}
fn default_max_title_len() -> usize {
    60
}
/// Narrower than the title, and narrower still than Herdr's own
/// `sidebar_max_width` (36 columns by default): the row spends columns on the
/// state icon and the elapsed label, so a cap at or above that ceiling never
/// binds — Herdr re-truncates the name and the elapsed label is what loses.
fn default_max_display_len() -> usize {
    24
}
fn default_success_sticky_ms() -> u64 {
    20_000
}
fn default_failure_sticky() -> bool {
    true
}

/// The shells are not cosmetic: a nested shell re-sources `init.zsh` with the
/// same `HERDR_PANE_ID`, so inner and outer share one state directory and one
/// marker. Without them the inner shell's `--clear-first` would release the
/// outer watcher's reservation mid-command.
fn default_ignore() -> Vec<String> {
    [
        "vim", "nvim", "less", "man", "ssh", "top", "htop", "claude", "codex", "opencode", "droid",
        "zsh", "bash", "sh", "fish",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_running() -> String {
    "running {elapsed}".to_string()
}
fn default_success() -> String {
    "ok · {elapsed}".to_string()
}
fn default_failure() -> String {
    "exit {code} · {elapsed}".to_string()
}
fn default_signal() -> String {
    "{signal} · {elapsed}".to_string()
}
/// No exit code was readable. Elapsed only: an unknown outcome must never be
/// dressed up as a success.
fn default_unknown() -> String {
    "{elapsed}".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Labels {
    pub running: String,
    pub success: String,
    pub failure: String,
    pub signal: String,
    pub unknown: String,
}

impl Default for Labels {
    fn default() -> Self {
        Self {
            running: default_running(),
            success: default_success(),
            failure: default_failure(),
            signal: default_signal(),
            unknown: default_unknown(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Finish {
    pub success_sticky_ms: u64,
    pub failure_sticky: bool,
}

impl Default for Finish {
    fn default() -> Self {
        Self {
            success_sticky_ms: default_success_sticky_ms(),
            failure_sticky: default_failure_sticky(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub threshold_ms: u64,
    pub tick_ms: u64,
    pub max_title_len: usize,
    pub max_display_len: usize,
    pub ignore: Vec<String>,
    pub ignore_extra: Vec<String>,
    pub finish: Finish,
    pub labels: Labels,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold_ms: default_threshold_ms(),
            tick_ms: default_tick_ms(),
            max_title_len: default_max_title_len(),
            max_display_len: default_max_display_len(),
            ignore: default_ignore(),
            ignore_extra: Vec::new(),
            finish: Finish::default(),
            labels: Labels::default(),
        }
    }
}

impl Config {
    /// Reads `config.toml` from `dir`. Any failure — missing directory, missing
    /// file, unreadable file, malformed TOML — yields defaults. The watcher must
    /// never fail loudly, so a broken config degrades to stock behavior.
    pub fn load(dir: Option<&Path>) -> Config {
        let mut cfg = Self::parse(dir);
        cfg.sanitize();
        cfg
    }

    fn parse(dir: Option<&Path>) -> Config {
        let Some(dir) = dir else {
            return Config::default();
        };
        let Ok(body) = std::fs::read_to_string(dir.join("config.toml")) else {
            return Config::default();
        };
        toml::from_str(&body).unwrap_or_default()
    }

    /// Pulls hostile values back into a range the driver can survive. Applied
    /// after parsing so a user config can never make the watcher busy-loop.
    fn sanitize(&mut self) {
        self.tick_ms = self.tick_ms.max(MIN_INTERVAL_MS);
        self.threshold_ms = self.threshold_ms.max(MIN_INTERVAL_MS);
        self.max_display_len = self.max_display_len.max(MIN_DISPLAY_LEN);
    }

    pub fn is_ignored(&self, agent: &str) -> bool {
        self.ignore
            .iter()
            .chain(self.ignore_extra.iter())
            .any(|entry| entry == agent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_config(dir: &std::path::Path, body: &str) {
        let mut f = std::fs::File::create(dir.join("config.toml")).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn the_display_cap_defaults_to_something_the_sidebar_can_actually_show() {
        // Herdr's own `sidebar_max_width` defaults to 36 columns, and the row
        // spends some of those on the state icon and the elapsed label. A cap
        // above that ceiling never binds: Herdr re-truncates what we already
        // truncated, and the elapsed label is what gets squeezed out.
        const HERDR_SIDEBAR_MAX_WIDTH: usize = 36;
        let cfg = Config::load(None);
        assert_eq!(cfg.max_display_len, 24);
        assert!(
            cfg.max_display_len < HERDR_SIDEBAR_MAX_WIDTH,
            "a row name wider than the sidebar is cut by Herdr, not by us"
        );
        assert!(
            cfg.max_display_len < cfg.max_title_len,
            "the sidebar row is narrower than the title field"
        );
    }

    #[test]
    fn a_zero_display_cap_is_floored_so_the_row_is_never_nameless() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "max_display_len = 0\n");
        let cfg = Config::load(Some(dir.path()));
        assert_eq!(cfg.max_display_len, MIN_DISPLAY_LEN);
    }

    #[test]
    fn defaults_are_used_when_no_directory_is_given() {
        let cfg = Config::load(None);
        assert_eq!(cfg.threshold_ms, 2000);
        assert_eq!(cfg.tick_ms, 2000);
        assert_eq!(cfg.max_title_len, 60);
        assert_eq!(cfg.finish.success_sticky_ms, 20_000);
        assert!(cfg.finish.failure_sticky);
        assert_eq!(cfg.labels.running, "running {elapsed}");
    }

    #[test]
    fn defaults_are_used_when_the_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(Some(dir.path()));
        assert_eq!(cfg.threshold_ms, 2000);
    }

    #[test]
    fn malformed_toml_falls_back_to_defaults_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "this is not = = valid toml [[[");
        let cfg = Config::load(Some(dir.path()));
        assert_eq!(cfg.threshold_ms, 2000);
    }

    #[test]
    fn partial_config_overrides_only_named_keys() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "threshold_ms = 500\n");
        let cfg = Config::load(Some(dir.path()));
        assert_eq!(cfg.threshold_ms, 500);
        assert_eq!(cfg.tick_ms, 2000, "unnamed keys keep their defaults");
        assert_eq!(cfg.labels.success, "ok · {elapsed}");
    }

    #[test]
    fn nested_tables_override_independently() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[finish]\nsuccess_sticky_ms = 0\n\n[labels]\nrunning = \"busy {elapsed}\"\n",
        );
        let cfg = Config::load(Some(dir.path()));
        assert_eq!(cfg.finish.success_sticky_ms, 0);
        assert!(
            cfg.finish.failure_sticky,
            "untouched finish key keeps default"
        );
        assert_eq!(cfg.labels.running, "busy {elapsed}");
        assert_eq!(cfg.labels.failure, "exit {code} · {elapsed}");
    }

    #[test]
    fn default_ignore_list_covers_agent_clis() {
        let cfg = Config::load(None);
        assert!(cfg.is_ignored("claude"));
        assert!(cfg.is_ignored("codex"));
        assert!(cfg.is_ignored("vim"));
        assert!(!cfg.is_ignored("cargo"));
    }

    /// The ignore list is only as good as the name it is handed. An aliased
    /// agent launcher (`claude-personal` = `CLAUDE_CONFIG_DIR=... command
    /// claude`) reached `is_ignored` as the alias, matched nothing, and the
    /// plugin registered itself on the pane the agent was already running in.
    /// Both halves of the fix are pinned here: the hook must hand over the
    /// alias-expanded line, and `agent_name` must see through the wrapper.
    #[test]
    fn an_aliased_agent_launcher_is_ignored_once_the_alias_is_expanded() {
        let cfg = Config::load(None);
        let expanded = "CLAUDE_CONFIG_DIR=/Users/x/.claude-personal command claude";
        assert!(
            cfg.is_ignored(&crate::command::agent_name(expanded)),
            "an aliased claude must reach the ignore list as `claude`"
        );
    }

    /// zsh hands `preexec` the typed line as `$1` and the alias-expanded,
    /// single-line form as `$2`. `$1` cannot see through an alias and `$3`,
    /// though also expanded, may contain newlines — a multi-line function
    /// definition would corrupt the single-line `cmd` file the watcher reads.
    #[test]
    fn the_hook_reads_the_alias_expanded_command_line() {
        let init = include_str!("../shell/init.zsh");
        assert!(
            init.contains(r#"local cmd="${2:-$1}""#),
            "preexec must prefer $2 (alias-expanded, single-line) over the typed $1"
        );
    }

    #[test]
    fn default_ignore_list_covers_nested_shells() {
        let cfg = Config::load(None);
        for shell in ["zsh", "bash", "sh", "fish"] {
            assert!(
                cfg.is_ignored(shell),
                "a nested {shell} shares the pane state dir and would fight the outer watcher"
            );
        }
    }

    /// Pulls the elements out of the commented `ignore = [...]` block so a
    /// drift between the shipped example and the compiled default is a test
    /// failure rather than a support question.
    fn commented_ignore_list(example: &str) -> Vec<String> {
        let start = example
            .find("# ignore = [")
            .expect("commented default ignore list");
        let rest = &example[start..];
        let end = rest.find(']').expect("closing bracket");
        rest[..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_example_config_documents_the_real_default_ignore_list() {
        let example = include_str!("../config.example.toml");
        assert_eq!(
            commented_ignore_list(example),
            default_ignore(),
            "config.example.toml's commented default must match default_ignore() exactly"
        );
    }

    /// The README's `Defaults:` line is prose, so it drifts silently. It listed
    /// nothing at all until a user asked for a feature (`ignore_extra`) that had
    /// existed and been tested from the first release — they simply had no way
    /// to discover it. Pin the list so the next entry added to `default_ignore`
    /// cannot quietly go undocumented.
    #[test]
    fn the_readme_documents_the_real_default_ignore_list() {
        let readme = include_str!("../README.md");
        let start = readme
            .find("Defaults: ")
            .expect("a Defaults: line in the README");
        let rest = &readme[start..];
        let end = rest.find('.').expect("the line ends in a period");
        let documented: Vec<String> = rest[..end]
            .split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect();

        let mut documented_sorted = documented.clone();
        let mut actual_sorted = default_ignore();
        documented_sorted.sort();
        actual_sorted.sort();
        assert_eq!(
            documented_sorted, actual_sorted,
            "the README's Defaults: line must list exactly default_ignore()"
        );
    }

    #[test]
    fn the_readme_explains_that_ignore_extra_adds_rather_than_replaces() {
        let readme = include_str!("../README.md");
        assert!(
            readme.contains("ignore_extra"),
            "ignore_extra is undiscoverable unless the README names it"
        );
    }

    #[test]
    fn zero_intervals_are_clamped_to_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "tick_ms = 0\nthreshold_ms = 0\n");
        let cfg = Config::load(Some(dir.path()));
        assert_eq!(cfg.tick_ms, 250, "tick_ms = 0 would spin the socket");
        assert_eq!(cfg.threshold_ms, 250);
    }

    #[test]
    fn intervals_above_the_floor_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "tick_ms = 1000\nthreshold_ms = 300\n");
        let cfg = Config::load(Some(dir.path()));
        assert_eq!(cfg.tick_ms, 1000);
        assert_eq!(cfg.threshold_ms, 300);
    }

    #[test]
    fn ignore_extra_adds_without_replacing_defaults() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "ignore_extra = [\"terraform\"]\n");
        let cfg = Config::load(Some(dir.path()));
        assert!(cfg.is_ignored("terraform"), "extra entry is honored");
        assert!(cfg.is_ignored("claude"), "defaults survive ignore_extra");
    }

    #[test]
    fn explicit_ignore_replaces_the_default_list() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "ignore = [\"only-this\"]\n");
        let cfg = Config::load(Some(dir.path()));
        assert!(cfg.is_ignored("only-this"));
        assert!(
            !cfg.is_ignored("claude"),
            "explicit ignore is a replacement"
        );
    }
}
