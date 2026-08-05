use serde::Deserialize;
use std::path::Path;

fn default_threshold_ms() -> u64 {
    2000
}
fn default_tick_ms() -> u64 {
    2000
}
fn default_max_title_len() -> usize {
    60
}
fn default_success_sticky_ms() -> u64 {
    20_000
}
fn default_failure_sticky() -> bool {
    true
}

fn default_ignore() -> Vec<String> {
    [
        "vim", "nvim", "less", "man", "ssh", "top", "htop", "claude", "codex", "opencode",
        "droid",
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Labels {
    pub running: String,
    pub success: String,
    pub failure: String,
    pub signal: String,
}

impl Default for Labels {
    fn default() -> Self {
        Self {
            running: default_running(),
            success: default_success(),
            failure: default_failure(),
            signal: default_signal(),
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
        let Some(dir) = dir else {
            return Config::default();
        };
        let Ok(body) = std::fs::read_to_string(dir.join("config.toml")) else {
            return Config::default();
        };
        toml::from_str(&body).unwrap_or_default()
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
        assert!(cfg.finish.failure_sticky, "untouched finish key keeps default");
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
        assert!(!cfg.is_ignored("claude"), "explicit ignore is a replacement");
    }
}
