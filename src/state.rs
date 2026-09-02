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
        /// Rendered by the sidebar in preference to the constant agent id.
        display_agent: Option<String>,
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
    /// Unlink the marker, but only if it still names `agent`. A watcher must
    /// never delete a marker a newer watcher wrote.
    MarkerRemove {
        agent: String,
    },
    Exit,
}

pub struct Machine {
    cfg: Config,
    /// Program name only. Feeds the ignore list and the `{agent}` template
    /// variable; the sidebar row shows `display` instead.
    agent: String,
    title: String,
    display: String,
    start_ms: u64,
    reported: bool,
}

impl Machine {
    pub fn new(cfg: Config, agent: String, title: String, display: String, start_ms: u64) -> Self {
        Self {
            cfg,
            agent,
            title,
            display,
            start_ms,
            reported: false,
        }
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
            Outcome::Unknown => (&self.cfg.labels.unknown, String::new(), String::new()),
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
                Action::MarkerWrite {
                    agent: crate::proto::AGENT_ID.to_string(),
                },
                Action::ReportAgent {
                    state: AgentState::Working,
                    agent: crate::proto::AGENT_ID.to_string(),
                    message: Some(self.title.clone()),
                },
                Action::Metadata {
                    title: Some(self.title.clone()),
                    label: Some(("working", self.running_label(elapsed))),
                    ttl_ms: None,
                    clear: false,
                    display_agent: Some(self.display.clone()),
                },
            ];
        }
        // Resend the title every tick. Herdr replaces a source's metadata
        // wholesale on each report_metadata, so omitting it here would clear the
        // command line from the pane for the rest of the command's run.
        vec![Action::Metadata {
            title: Some(self.title.clone()),
            label: Some(("working", self.running_label(elapsed))),
            ttl_ms: None,
            clear: false,
            display_agent: Some(self.display.clone()),
        }]
    }

    /// `exit_code` is `None` when the exit file was missing, empty, or
    /// unparseable. That case is neither success nor failure: it renders the
    /// elapsed-only label and takes the auto-clearing finish path, because a
    /// label we cannot justify must not stick around demanding attention.
    pub fn on_finish(&mut self, now_ms: u64, exit_code: Option<i32>) -> Vec<Action> {
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
                    agent: crate::proto::AGENT_ID.to_string(),
                    message: None,
                },
                Action::Metadata {
                    title: Some(self.title.clone()),
                    label: Some(("idle", text)),
                    ttl_ms: None,
                    clear: false,
                    display_agent: Some(self.display.clone()),
                },
                Action::Exit,
            ];
        }

        if self.cfg.finish.success_sticky_ms == 0 {
            return vec![
                Action::Release {
                    agent: crate::proto::AGENT_ID.to_string(),
                },
                Action::MarkerRemove {
                    agent: crate::proto::AGENT_ID.to_string(),
                },
                Action::Exit,
            ];
        }

        let ttl = self.cfg.finish.success_sticky_ms.min(MAX_TTL_MS);
        // `ttl` is used for both the protocol field and the linger duration.
        vec![
            Action::ReportAgent {
                state: AgentState::Idle,
                agent: crate::proto::AGENT_ID.to_string(),
                message: None,
            },
            Action::Metadata {
                title: Some(self.title.clone()),
                label: Some(("idle", text)),
                ttl_ms: Some(ttl),
                clear: false,
                display_agent: Some(self.display.clone()),
            },
            // TTL expires title and state_labels but not agent/agent_status, so
            // we must outlive the window to release explicitly. Linger uses the
            // clamped value: an absurd config must not park a process for days.
            // The driver aborts the remaining actions if the linger is cut
            // short by a newer watcher taking over.
            Action::Linger { ms: ttl },
            Action::Release {
                agent: crate::proto::AGENT_ID.to_string(),
            },
            Action::MarkerRemove {
                agent: crate::proto::AGENT_ID.to_string(),
            },
            Action::Exit,
        ]
    }

    pub fn on_shell_gone(&mut self) -> Vec<Action> {
        if !self.reported {
            return vec![Action::Exit];
        }
        vec![
            Action::Release {
                agent: crate::proto::AGENT_ID.to_string(),
            },
            Action::MarkerRemove {
                agent: crate::proto::AGENT_ID.to_string(),
            },
            Action::Exit,
        ]
    }
}

/// Wipes a sticky label left by a previous command. `prev_agent` comes from the
/// marker file, because `pane.release_agent` requires the agent name.
pub fn clear_actions(prev_agent: &str) -> Vec<Action> {
    vec![
        Action::Metadata {
            title: None,
            label: None,
            ttl_ms: None,
            clear: true,
            display_agent: None,
        },
        Action::Release {
            agent: prev_agent.to_string(),
        },
        Action::MarkerRemove {
            agent: prev_agent.to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn machine() -> Machine {
        Machine::new(
            Config::default(),
            "cargo".into(),
            "cargo build".into(),
            "cargo build".into(),
            1_000_000,
        )
    }

    #[test]
    fn the_row_name_is_the_command_line_while_the_agent_var_stays_the_program() {
        // The sidebar row reads `npm run start`, not `npm`; templates asking for
        // `{agent}` still get the program name the ignore list matches on.
        let mut cfg = Config::default();
        cfg.labels.running = "{agent} · {elapsed}".into();
        let mut m = Machine::new(
            cfg,
            "npm".into(),
            "npm run start".into(),
            "npm run start".into(),
            1_000_000,
        );

        let meta = m
            .on_tick(1_002_000)
            .into_iter()
            .find_map(|act| match act {
                Action::Metadata {
                    display_agent,
                    label,
                    ..
                } => Some((display_agent, label)),
                _ => None,
            })
            .expect("crossing the threshold reports metadata");

        assert_eq!(meta.0, Some("npm run start".to_string()));
        assert_eq!(meta.1, Some(("working", "npm · 2s".to_string())));
    }

    #[test]
    fn a_fast_command_never_reports_anything() {
        let mut m = machine();
        assert!(
            m.on_tick(1_000_500).is_empty(),
            "below threshold, stay silent"
        );
        assert_eq!(m.on_finish(1_000_800, Some(0)), vec![Action::Exit]);
    }

    #[test]
    fn crossing_the_threshold_reports_working_and_marks() {
        let mut m = machine();
        let actions = m.on_tick(1_002_000);
        assert_eq!(
            actions,
            vec![
                Action::MarkerWrite {
                    agent: "shell".into()
                },
                Action::ReportAgent {
                    state: AgentState::Working,
                    agent: "shell".into(),
                    message: Some("cargo build".into()),
                },
                Action::Metadata {
                    title: Some("cargo build".into()),
                    label: Some(("working", "running 2s".into())),
                    ttl_ms: None,
                    clear: false,
                    display_agent: Some("cargo build".into()),
                },
            ]
        );
    }

    #[test]
    fn every_command_reports_the_same_agent_id_but_its_own_display_name() {
        // The reported agent id is constant so one rows_by_agent rule can style
        // every shell entry; the command name reaches the sidebar via
        // display_agent, which Herdr renders in preference to the id.
        let mut a = Machine::new(
            Config::default(),
            "cargo".into(),
            "cargo build".into(),
            "cargo build".into(),
            1_000_000,
        );
        let mut b = Machine::new(
            Config::default(),
            "make".into(),
            "make all".into(),
            "make all".into(),
            1_000_000,
        );

        let ids: Vec<String> = [&mut a, &mut b]
            .iter_mut()
            .flat_map(|m| m.on_tick(1_002_000))
            .filter_map(|act| match act {
                Action::ReportAgent { agent, .. } => Some(agent),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["shell".to_string(), "shell".to_string()]);

        let shown: Vec<Option<String>> = [&mut a, &mut b]
            .iter_mut()
            .flat_map(|m| m.on_tick(1_010_000))
            .filter_map(|act| match act {
                Action::Metadata { display_agent, .. } => Some(display_agent),
                _ => None,
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                Some("cargo build".to_string()),
                Some("make all".to_string())
            ]
        );
    }

    #[test]
    fn later_ticks_resend_the_title_alongside_the_label() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_tick(1_014_000);
        // Herdr replaces a source's metadata wholesale on every report_metadata,
        // so omitting the title here CLEARS it. Verified live: the command line
        // vanished from the pane for the whole middle of a long command and only
        // reappeared at completion. The title must be resent on every tick.
        assert_eq!(
            actions,
            vec![Action::Metadata {
                title: Some("cargo build".into()),
                label: Some(("working", "running 14s".into())),
                ttl_ms: None,
                clear: false,
                display_agent: Some("cargo build".into()),
            }]
        );
    }

    #[test]
    fn failure_sticks_with_no_ttl_and_keeps_the_marker() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_252_000, Some(1));
        assert_eq!(
            actions,
            vec![
                Action::ReportAgent {
                    state: AgentState::Idle,
                    agent: "shell".into(),
                    message: None,
                },
                Action::Metadata {
                    title: Some("cargo build".into()),
                    label: Some(("idle", "exit 1 · 4m12s".into())),
                    ttl_ms: None,
                    clear: false,
                    display_agent: Some("cargo build".into()),
                },
                Action::Exit,
            ]
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::MarkerRemove { .. })),
            "the marker must survive so the next preexec clears this label"
        );
    }

    #[test]
    fn success_uses_a_ttl_then_lingers_and_releases() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_252_000, Some(0));
        assert_eq!(
            actions,
            vec![
                Action::ReportAgent {
                    state: AgentState::Idle,
                    agent: "shell".into(),
                    message: None,
                },
                Action::Metadata {
                    title: Some("cargo build".into()),
                    label: Some(("idle", "ok · 4m12s".into())),
                    ttl_ms: Some(20_000),
                    clear: false,
                    display_agent: Some("cargo build".into()),
                },
                Action::Linger { ms: 20_000 },
                Action::Release {
                    agent: "shell".into()
                },
                Action::MarkerRemove {
                    agent: "shell".into()
                },
                Action::Exit,
            ]
        );
    }

    #[test]
    fn zero_sticky_success_releases_immediately() {
        let mut cfg = Config::default();
        cfg.finish.success_sticky_ms = 0;
        let mut m = Machine::new(
            cfg,
            "cargo".into(),
            "cargo build".into(),
            "cargo build".into(),
            1_000_000,
        );
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_010_000, Some(0));
        assert_eq!(
            actions,
            vec![
                Action::Release {
                    agent: "shell".into()
                },
                Action::MarkerRemove {
                    agent: "shell".into()
                },
                Action::Exit,
            ]
        );
    }

    #[test]
    fn a_signal_exit_follows_the_success_path() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, Some(130));
        assert_eq!(
            actions[1],
            Action::Metadata {
                title: Some("cargo build".into()),
                label: Some(("idle", "SIGINT · 12s".into())),
                ttl_ms: Some(20_000),
                clear: false,
                display_agent: Some("cargo build".into()),
            },
            "a deliberate cancel auto-clears rather than nagging like a failure"
        );
    }

    #[test]
    fn failure_sticky_disabled_makes_failures_auto_clear() {
        let mut cfg = Config::default();
        cfg.finish.failure_sticky = false;
        let mut m = Machine::new(
            cfg,
            "cargo".into(),
            "cargo build".into(),
            "cargo build".into(),
            1_000_000,
        );
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, Some(1));
        assert!(actions.contains(&Action::MarkerRemove {
            agent: "shell".into()
        }));
        assert!(actions.contains(&Action::Linger { ms: 20_000 }));
    }

    #[test]
    fn ttl_is_clamped_to_the_protocol_maximum() {
        let mut cfg = Config::default();
        cfg.finish.success_sticky_ms = 999_999_999;
        let mut m = Machine::new(
            cfg,
            "cargo".into(),
            "cargo build".into(),
            "cargo build".into(),
            1_000_000,
        );
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, Some(0));
        match &actions[1] {
            Action::Metadata { ttl_ms, .. } => assert_eq!(*ttl_ms, Some(86_400_000)),
            other => panic!("expected metadata, got {other:?}"),
        }
    }

    #[test]
    fn linger_uses_the_clamped_ttl_not_the_raw_config() {
        let mut cfg = Config::default();
        cfg.finish.success_sticky_ms = 999_999_999;
        let mut m = Machine::new(
            cfg,
            "cargo".into(),
            "cargo build".into(),
            "cargo build".into(),
            1_000_000,
        );
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, Some(0));
        assert!(
            actions.contains(&Action::Linger { ms: 86_400_000 }),
            "an absurd config must not park the process for days"
        );
    }

    #[test]
    fn an_unknown_exit_code_renders_elapsed_only_not_ok() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, None);
        assert_eq!(
            actions[1],
            Action::Metadata {
                title: Some("cargo build".into()),
                label: Some(("idle", "12s".into())),
                ttl_ms: Some(20_000),
                clear: false,
                display_agent: Some("cargo build".into()),
            },
            "an unreadable exit code must not be dressed up as a success"
        );
    }

    #[test]
    fn an_unknown_exit_code_is_distinct_from_success_and_failure() {
        let mut unknown = machine();
        unknown.on_tick(1_002_000);
        let unknown = unknown.on_finish(1_012_000, None);

        let mut ok = machine();
        ok.on_tick(1_002_000);
        let ok = ok.on_finish(1_012_000, Some(0));

        let mut bad = machine();
        bad.on_tick(1_002_000);
        let bad = bad.on_finish(1_012_000, Some(1));

        assert_ne!(unknown[1], ok[1], "unknown must not read as ok");
        assert_ne!(unknown[1], bad[1], "unknown must not claim an exit code");
    }

    #[test]
    fn an_unknown_exit_code_auto_clears_rather_than_sticking() {
        let mut m = machine();
        m.on_tick(1_002_000);
        let actions = m.on_finish(1_012_000, None);
        assert!(
            actions.contains(&Action::Linger { ms: 20_000 }),
            "a label we cannot justify must expire on its own"
        );
        assert!(actions.contains(&Action::MarkerRemove {
            agent: "shell".into()
        }));
    }

    #[test]
    fn every_marker_removal_names_the_agent_that_wrote_it() {
        for actions in [
            {
                let mut m = machine();
                m.on_tick(1_002_000);
                m.on_finish(1_012_000, Some(0))
            },
            {
                let mut m = machine();
                m.on_tick(1_002_000);
                m.on_shell_gone()
            },
            clear_actions("shell"),
        ] {
            let removes: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::MarkerRemove { agent } => Some(agent.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(removes, vec!["shell".to_string()]);
        }
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
                Action::Release {
                    agent: "shell".into()
                },
                Action::MarkerRemove {
                    agent: "shell".into()
                },
                Action::Exit,
            ]
        );
    }

    #[test]
    fn next_wake_targets_the_threshold_then_the_tick_interval() {
        let mut m = machine();
        assert_eq!(
            m.next_wake_ms(1_000_000),
            2000,
            "wait out the full threshold"
        );
        assert_eq!(m.next_wake_ms(1_001_500), 500, "only the remainder");
        m.on_tick(1_002_000);
        assert_eq!(m.next_wake_ms(1_002_000), 2000, "then the tick interval");
    }

    #[test]
    fn clear_actions_releases_whatever_the_marker_names_even_a_legacy_command_id() {
        // Markers now hold the constant AGENT_ID, but a marker written by an
        // older build holds a command name. On upgrade, a pane left showing a
        // sticky failure still has such a marker, and its reservation was made
        // under that name — releasing AGENT_ID instead would strand it forever.
        // So clear_actions must stay generic rather than hardcoding AGENT_ID.
        let actions = clear_actions("cargo");
        assert!(actions.contains(&Action::Release {
            agent: "cargo".into()
        }));
        assert!(actions.contains(&Action::MarkerRemove {
            agent: "cargo".into()
        }));
    }

    #[test]
    fn clear_actions_wipe_metadata_and_release_the_previous_agent() {
        assert_eq!(
            clear_actions("npm"),
            vec![
                Action::Metadata {
                    title: None,
                    label: None,
                    ttl_ms: None,
                    clear: true,
                    display_agent: None,
                },
                Action::Release {
                    agent: "npm".into()
                },
                Action::MarkerRemove {
                    agent: "npm".into()
                },
            ]
        );
    }
}
