use serde_json::{json, Map, Value};

/// Identifies this reporter to Herdr. Kept distinct from any real agent
/// integration's source so the two never contend for a pane.
pub const SOURCE: &str = "shell-progress";

/// The agent id reported for every shell command, deliberately constant.
///
/// Reporting the command name here would mint a new agent identity per command
/// (`cargo`, `sleep`, `make`), which is what made shell entries indistinguishable
/// from real agents in the sidebar. With one stable id, a single config rule
/// styles them all:
///
/// ```toml
/// [ui.sidebar.agents.rows_by_agent]
/// shell = [["state_icon", "workspace", "tab"], ["$cmd"]]
/// ```
///
/// The command name still reaches the row via `display_agent`.
pub const AGENT_ID: &str = "shell";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Idle,
}

impl AgentState {
    fn as_str(self) -> &'static str {
        match self {
            AgentState::Working => "working",
            AgentState::Idle => "idle",
        }
    }
}

pub fn report_agent(pane_id: &str, agent: &str, state: AgentState, message: Option<&str>) -> Value {
    let mut map = Map::new();
    map.insert("pane_id".into(), json!(pane_id));
    map.insert("source".into(), json!(SOURCE));
    map.insert("agent".into(), json!(agent));
    map.insert("state".into(), json!(state.as_str()));
    if let Some(message) = message {
        map.insert("message".into(), json!(message));
    }
    Value::Object(map)
}

pub fn report_metadata(
    pane_id: &str,
    title: Option<String>,
    label: Option<(&str, String)>,
    ttl_ms: Option<u64>,
    clear: bool,
    display_agent: Option<&str>,
) -> Value {
    let mut map = Map::new();
    map.insert("pane_id".into(), json!(pane_id));
    map.insert("source".into(), json!(SOURCE));
    map.insert("applies_to_source".into(), json!(SOURCE));
    let title_text = title.clone();
    if let Some(title) = title {
        map.insert("title".into(), json!(title));
    }
    // The sidebar renders `display_agent` in preference to `agent`, verified
    // live. That split is what lets us report a constant agent id — so a single
    // `rows_by_agent` rule can style every shell entry — while the row still
    // shows the actual command.
    if let Some(display) = display_agent {
        map.insert("display_agent".into(), json!(display));
        let mut tokens = Map::new();
        // `$cmd` in a custom row means the same thing as `{cmd}` in a label
        // template: the whole command line. `display_agent` already carries the
        // short name, so a token duplicating it would be worth nothing.
        tokens.insert(
            "cmd".into(),
            json!(title_text.unwrap_or_else(|| display.to_string())),
        );
        map.insert("tokens".into(), Value::Object(tokens));
    }
    if let Some((state_key, text)) = label {
        let mut labels = Map::new();
        labels.insert(state_key.to_string(), json!(text));
        map.insert("state_labels".into(), Value::Object(labels));
    }
    if let Some(ttl) = ttl_ms {
        map.insert("ttl_ms".into(), json!(ttl));
    }
    if clear {
        map.insert("clear_title".into(), json!(true));
        map.insert("clear_state_labels".into(), json!(true));
    }
    Value::Object(map)
}

pub fn release_agent(pane_id: &str, agent: &str) -> Value {
    json!({
        "pane_id": pane_id,
        "source": SOURCE,
        "agent": agent,
    })
}

/// Builds one newline-delimited wire line. The server reads exactly one request
/// per connection and then closes.
pub fn envelope(id: &str, method: &str, params: Value) -> String {
    let mut line = serde_json::to_string(&json!({
        "id": id,
        "method": method,
        "params": params,
    }))
    .unwrap_or_else(|_| String::from("{}"));
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_is_the_agreed_constant() {
        assert_eq!(SOURCE, "shell-progress");
    }

    #[test]
    fn report_agent_carries_required_fields() {
        let v = report_agent("w1:p2", "cargo", AgentState::Working, Some("cargo build"));
        assert_eq!(v["pane_id"], "w1:p2");
        assert_eq!(v["source"], "shell-progress");
        assert_eq!(v["agent"], "cargo");
        assert_eq!(v["state"], "working");
        assert_eq!(v["message"], "cargo build");
    }

    #[test]
    fn report_agent_omits_message_when_absent() {
        let v = report_agent("w1:p2", "cargo", AgentState::Idle, None);
        assert_eq!(v["state"], "idle");
        assert!(
            v.get("message").is_none(),
            "absent message must not be sent"
        );
    }

    #[test]
    fn report_metadata_sets_label_under_its_state_key() {
        let v = report_metadata(
            "w1:p2",
            Some("cargo build".to_string()),
            Some(("working", "running 12s".to_string())),
            None,
            false,
            None,
        );
        assert_eq!(v["pane_id"], "w1:p2");
        assert_eq!(v["source"], "shell-progress");
        assert_eq!(v["applies_to_source"], "shell-progress");
        assert_eq!(v["title"], "cargo build");
        assert_eq!(v["state_labels"]["working"], "running 12s");
    }

    #[test]
    fn report_metadata_includes_ttl_when_given() {
        let v = report_metadata(
            "w1:p2",
            None,
            Some(("idle", "ok · 4s".to_string())),
            Some(20_000),
            false,
            None,
        );
        assert_eq!(v["ttl_ms"], 20_000);
        assert!(v.get("title").is_none());
    }

    #[test]
    fn report_metadata_clear_sets_both_clear_flags() {
        let v = report_metadata("w1:p2", None, None, None, true, None);
        assert_eq!(v["clear_title"], true);
        assert_eq!(v["clear_state_labels"], true);
        assert!(v.get("state_labels").is_none());
    }

    #[test]
    fn display_agent_is_short_while_the_cmd_token_is_the_whole_command_line() {
        let v = report_metadata(
            "w1:p2",
            Some("cargo build --release".to_string()),
            None,
            None,
            false,
            Some("cargo"),
        );
        assert_eq!(v["display_agent"], "cargo", "the row shows the short name");
        assert_eq!(
            v["tokens"]["cmd"], "cargo build --release",
            "$cmd means what {{cmd}} means in a label template: the full command"
        );
    }

    #[test]
    fn the_cmd_token_falls_back_to_the_display_name_without_a_title() {
        let v = report_metadata("w1:p2", None, None, None, false, Some("cargo"));
        assert_eq!(v["tokens"]["cmd"], "cargo");
    }

    #[test]
    fn display_agent_and_tokens_are_omitted_when_absent() {
        let v = report_metadata("w1:p2", None, None, None, false, None);
        assert!(v.get("display_agent").is_none());
        assert!(v.get("tokens").is_none());
    }

    #[test]
    fn agent_id_is_constant_so_one_config_rule_can_style_every_shell_entry() {
        assert_eq!(AGENT_ID, "shell");
    }

    #[test]
    fn release_agent_includes_the_agent_name() {
        let v = release_agent("w1:p2", "cargo");
        assert_eq!(v["pane_id"], "w1:p2");
        assert_eq!(v["source"], "shell-progress");
        assert_eq!(v["agent"], "cargo", "release_agent requires the agent name");
    }

    #[test]
    fn envelope_is_one_newline_terminated_json_line() {
        let line = envelope("abc", "pane.release_agent", serde_json::json!({"x": 1}));
        assert!(line.ends_with('\n'), "wire format is newline-delimited");
        assert_eq!(
            line.matches('\n').count(),
            1,
            "exactly one line per request"
        );
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["id"], "abc");
        assert_eq!(parsed["method"], "pane.release_agent");
        assert_eq!(parsed["params"]["x"], 1);
    }
}
