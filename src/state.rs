use crate::config::Config;
use crate::label::{self, Outcome};
use crate::proto::AgentState;

/// The largest `ttl_ms` the Herdr protocol accepts.
const MAX_TTL_MS: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    ReportAgent {
        state: AgentState,
        agent: String,
        message: Option<String>,
    },
    Metadata {
        title: Option<String>,
        label: Option<(&'static str, String)>,
        ttl_ms: Option<u64>,
        clear: bool,
    },
    Release {
        agent: String,
    },
    Linger {
        ms: u64,
    },
    MarkerWrite {
        agent: String,
    },
    MarkerRemove,
    Exit,
}

pub struct Machine {
    cfg: Config,
    agent: String,
    title: String,
    start_ms: u64,
    reported: bool,
}

impl Machine {
    pub fn new(cfg: Config, agent: String, title: String, start_ms: u64) -> Self {
        Self { cfg, agent, title, start_ms, reported: false }
    }

    fn elapsed(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.start_ms)
    }

    fn running_label(&self, elapsed: u64) -> String {
        let vars = vec![
            ("elapsed", label::format_elapsed(elapsed)),
            ("cmd", self.title.clone()),
            ("agent", self.agent.clone()),
        ];
        label::render(&self.cfg.labels.running, &vars)
    }

    fn finish_label(&self, outcome: Outcome, elapsed: u64) -> String {
        let (template, code, signal) = match outcome {
            Outcome::Success => (&self.cfg.labels.success, String::new(), String::new()),
            Outcome::Failure(c) => (&self.cfg.labels.failure, c.to_string(), String::new()),
            Outcome::Signal(name) => (&self.cfg.labels.signal, String::new(), name.to_string()),
        };
        let vars = vec![
            ("elapsed", label::format_elapsed(elapsed)),
            ("code", code),
            ("signal", signal),
            ("cmd", self.title.clone()),
            ("agent", self.agent.clone()),
        ];
        label::render(template, &vars)
    }

    /// How long the driver should wait before calling `on_tick` again.
    pub fn next_wake_ms(&self, now_ms: u64) -> u64 {
        if self.reported {
            return self.cfg.tick_ms;
        }
        self.cfg.threshold_ms.saturating_sub(self.elapsed(now_ms))
    }

    pub fn on_tick(&mut self, now_ms: u64) -> Vec<Action> {
        let elapsed = self.elapsed(now_ms);
        if !self.reported {
            if elapsed < self.cfg.threshold_ms {
                return Vec::new();
            }
            self.reported = true;
            return vec![
                Action::MarkerWrite { agent: self.agent.clone() },
                Action::ReportAgent {
                    state: AgentState::Working,
                    agent: self.agent.clone(),
                    message: Some(self.title.clone()),
                },
                Action::Metadata {
                    title: Some(self.title.clone()),
                    label: Some(("working", self.running_label(elapsed))),
                    ttl_ms: None,
                    clear: false,
                },
            ];
        }
        vec![Action::Metadata {
            title: None,
            label: Some(("working", self.running_label(elapsed))),
            ttl_ms: None,
            clear: false,
        }]
    }

    pub fn on_finish(&mut self, now_ms: u64, exit_code: i32) -> Vec<Action> {
        // Nothing was ever reported, so there is nothing to clean up.
        if !self.reported {
            return vec![Action::Exit];
        }
        let elapsed = self.elapsed(now_ms);
        let outcome = label::classify(exit_code);
        let text = self.finish_label(outcome, elapsed);
        let sticky = matches!(outcome, Outcome::Failure(_)) && self.cfg.finish.failure_sticky;

        if sticky {
            // No TTL and no MarkerRemove: the label waits for the next preexec.
            return vec![
                Action::ReportAgent {
                    state: AgentState::Idle,
                    agent: self.agent.clone(),
                    message: None,
                },
                Action::Metadata {
                    title: Some(self.title.clone()),
                    label: Some(("idle", text)),
                    ttl_ms: None,
                    clear: false,
                },
                Action::Exit,
            ];
        }

        if self.cfg.finish.success_sticky_ms == 0 {
            return vec![
                Action::Release { agent: self.agent.clone() },
                Action::MarkerRemove,
                Action::Exit,
            ];
        }

        let ttl = self.cfg.finish.success_sticky_ms.min(MAX_TTL_MS);
        // `ttl` is used for both the protocol field and the linger duration.
        vec![
            Action::ReportAgent {
                state: AgentState::Idle,
                agent: self.agent.clone(),
                message: None,
            },
            Action::Metadata {
                title: Some(self.title.clone()),
                label: Some(("idle", text)),
                ttl_ms: Some(ttl),
                clear: false,
            },
            // TTL expires title and state_labels but not agent/agent_status, so
            // we must outlive the window to release explicitly. Linger uses the
            // clamped value: an absurd config must not park a process for days.
            Action::Linger { ms: ttl },
            Action::Release { agent: self.agent.clone() },
            Action::MarkerRemove,
            Action::Exit,
        ]
    }

    pub fn on_shell_gone(&mut self) -> Vec<Action> {
        if !self.reported {
            return vec![Action::Exit];
        }
        vec![
            Action::Release { agent: self.agent.clone() },
            Action::MarkerRemove,
            Action::Exit,
        ]
    }
}

/// Wipes a sticky label left by a previous command. `prev_agent` comes from the
/// marker file, because `pane.release_agent` requires the agent name.
pub fn clear_actions(prev_agent: &str) -> Vec<Action> {
    vec![
        Action::Metadata { title: None, label: None, ttl_ms: None, clear: true },
        Action::Release { agent: prev_agent.to_string() },
        Action::MarkerRemove,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn machine() -> Machine {
        Machine::new(Config::default(), "cargo".into(), "cargo build".into(), 1_000_000)
    }

    #[test]
    fn a_fast_command_never_reports_anything() {
        let mut m = machine();
        assert!(m.on_tick(1_000_500).is_empty(), "below threshold, stay silent");
        assert_eq!(m.on_finish(1_000_800, 0), vec![Action::Exit]);
    }

    #[test]
    fn crossing_the_threshold_reports_working_and_marks() {
        let mut m = machine();
        let actions = m.on_tick(1_002_000);
        assert_eq!(
            actions,
            vec![
                Action::MarkerWrite { agent: "cargo".into() },
                Action::ReportAgent {
                    state: AgentState::Working,
                    agent: "cargo".into(),
                    message: Some("cargo build".into()),
                },
                Action::Metadata {
                    title: Some("cargo build".into()),
                    label: Some(("working", "running 2s".into())),
                    ttl_ms: None,
                    clear: false,
                },
            ]
        );
    }

    #[test]
    fn later_ticks_only_refresh_the_label() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_tick(1_014_000);
        assert_eq!(
            actions,
            vec![Action::Metadata {
                title: None,
                label: Some(("working", "running 14s".into())),
                ttl_ms: None,
                clear: false,
            }]
        );
    }

    #[test]
    fn failure_sticks_with_no_ttl_and_keeps_the_marker() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_252_000, 1);
        assert_eq!(
            actions,
            vec![
                Action::ReportAgent {
                    state: AgentState::Idle,
                    agent: "cargo".into(),
                    message: None,
                },
                Action::Metadata {
                    title: Some("cargo build".into()),
                    label: Some(("idle", "exit 1 · 4m12s".into())),
                    ttl_ms: None,
                    clear: false,
                },
                Action::Exit,
            ]
        );
        assert!(
            !actions.contains(&Action::MarkerRemove),
            "the marker must survive so the next preexec clears this label"
        );
    }

    #[test]
    fn success_uses_a_ttl_then_lingers_and_releases() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_252_000, 0);
        assert_eq!(
            actions,
            vec![
                Action::ReportAgent {
                    state: AgentState::Idle,
                    agent: "cargo".into(),
                    message: None,
                },
                Action::Metadata {
                    title: Some("cargo build".into()),
                    label: Some(("idle", "ok · 4m12s".into())),
                    ttl_ms: Some(20_000),
                    clear: false,
                },
                Action::Linger { ms: 20_000 },
                Action::Release { agent: "cargo".into() },
                Action::MarkerRemove,
                Action::Exit,
            ]
        );
    }

    #[test]
    fn zero_sticky_success_releases_immediately() {
        let mut cfg = Config::default();
        cfg.finish.success_sticky_ms = 0;
        let mut m = Machine::new(cfg, "cargo".into(), "cargo build".into(), 1_000_000);
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_010_000, 0);
        assert_eq!(
            actions,
            vec![
                Action::Release { agent: "cargo".into() },
                Action::MarkerRemove,
                Action::Exit,
            ]
        );
    }

    #[test]
    fn a_signal_exit_follows_the_success_path() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, 130);
        assert_eq!(
            actions[1],
            Action::Metadata {
                title: Some("cargo build".into()),
                label: Some(("idle", "SIGINT · 12s".into())),
                ttl_ms: Some(20_000),
                clear: false,
            },
            "a deliberate cancel auto-clears rather than nagging like a failure"
        );
    }

    #[test]
    fn failure_sticky_disabled_makes_failures_auto_clear() {
        let mut cfg = Config::default();
        cfg.finish.failure_sticky = false;
        let mut m = Machine::new(cfg, "cargo".into(), "cargo build".into(), 1_000_000);
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, 1);
        assert!(actions.contains(&Action::MarkerRemove));
        assert!(actions.contains(&Action::Linger { ms: 20_000 }));
    }

    #[test]
    fn ttl_is_clamped_to_the_protocol_maximum() {
        let mut cfg = Config::default();
        cfg.finish.success_sticky_ms = 999_999_999;
        let mut m = Machine::new(cfg, "cargo".into(), "cargo build".into(), 1_000_000);
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, 0);
        match &actions[1] {
            Action::Metadata { ttl_ms, .. } => assert_eq!(*ttl_ms, Some(86_400_000)),
            other => panic!("expected metadata, got {other:?}"),
        }
    }

    #[test]
    fn linger_uses_the_clamped_ttl_not_the_raw_config() {
        let mut cfg = Config::default();
        cfg.finish.success_sticky_ms = 999_999_999;
        let mut m = Machine::new(cfg, "cargo".into(), "cargo build".into(), 1_000_000);
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, 0);
        assert!(
            actions.contains(&Action::Linger { ms: 86_400_000 }),
            "an absurd config must not park the process for days"
        );
    }

    #[test]
    fn shell_death_releases_only_if_we_reported() {
        let mut silent = machine();
        assert_eq!(silent.on_shell_gone(), vec![Action::Exit]);

        let mut reported = machine();
        reported.on_tick(1_002_000);
        assert_eq!(
            reported.on_shell_gone(),
            vec![
                Action::Release { agent: "cargo".into() },
                Action::MarkerRemove,
                Action::Exit,
            ]
        );
    }

    #[test]
    fn next_wake_targets_the_threshold_then_the_tick_interval() {
        let mut m = machine();
        assert_eq!(m.next_wake_ms(1_000_000), 2000, "wait out the full threshold");
        assert_eq!(m.next_wake_ms(1_001_500), 500, "only the remainder");
        m.on_tick(1_002_000);
        assert_eq!(m.next_wake_ms(1_002_000), 2000, "then the tick interval");
    }

    #[test]
    fn clear_actions_wipe_metadata_and_release_the_previous_agent() {
        assert_eq!(
            clear_actions("npm"),
            vec![
                Action::Metadata { title: None, label: None, ttl_ms: None, clear: true },
                Action::Release { agent: "npm".into() },
                Action::MarkerRemove,
            ]
        );
    }
}
