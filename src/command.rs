/// The label shown when we cannot identify a command name.
const FALLBACK: &str = "shell";

/// Words that run another program and take no options of their own worth
/// parsing. Skipping them is what lets an aliased agent launcher —
/// `CLAUDE_CONFIG_DIR=... command claude` — reach the ignore list under the
/// name the user would actually think to ignore.
///
/// Only wrappers whose first non-assignment argument IS the program belong
/// here. `sudo` does not: `sudo -u root make` would resolve to `root`.
const WRAPPERS: &[&str] = &["command", "builtin", "exec", "env", "nohup"];

/// Extracts the program name from a command line: first token that is neither a
/// `VAR=value` assignment nor a transparent wrapper, reduced to its basename.
///
/// A flag aimed at a wrapper (`env -u FOO claude`) is still returned as the
/// name — reading those would mean tracking each wrapper's option grammar. It
/// costs a cosmetic label, never a missed ignore rule, since the shorthand
/// forms that launch agents do not use them.
pub fn agent_name(cmd_line: &str) -> String {
    for token in cmd_line.split_whitespace() {
        if token.contains('=') && !token.starts_with('/') {
            continue;
        }
        let base = token.rsplit('/').next().unwrap_or(token);
        if base.is_empty() || WRAPPERS.contains(&base) {
            continue;
        }
        return base.to_string();
    }
    FALLBACK.to_string()
}

/// The name the sidebar row shows: the command line as typed, with runs of
/// whitespace collapsed so the row stays tight, cut to `max` characters.
///
/// Deliberately not `agent_name`: a row reading `npm` tells you nothing that
/// `npm run start` does not tell you better. The basename stripping and wrapper
/// skipping stay in `agent_name`, which still feeds the ignore list and the
/// `{agent}` template variable.
///
/// Collapsing whitespace is cosmetic and can visibly alter a quoted argument
/// (`echo "a   b"`). The row is a name, not a transcript.
pub fn display_name(cmd_line: &str, max: usize) -> String {
    let collapsed = cmd_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return FALLBACK.to_string();
    }
    truncate(&collapsed, max)
}

/// Shortens a string to `max` characters, marking the cut with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_name_takes_the_first_word() {
        assert_eq!(agent_name("cargo build --release"), "cargo");
    }

    #[test]
    fn agent_name_strips_the_directory() {
        assert_eq!(agent_name("/usr/local/bin/npm test"), "npm");
    }

    #[test]
    fn agent_name_skips_leading_env_assignments() {
        assert_eq!(agent_name("RUST_LOG=debug cargo test"), "cargo");
        assert_eq!(agent_name("A=1 B=2 make all"), "make");
    }

    #[test]
    fn agent_name_handles_empty_and_whitespace_input() {
        assert_eq!(agent_name(""), "shell");
        assert_eq!(agent_name("   "), "shell");
    }

    #[test]
    fn agent_name_falls_back_when_everything_is_an_assignment() {
        assert_eq!(agent_name("FOO=1 BAR=2"), "shell");
    }

    #[test]
    fn agent_name_looks_past_the_command_builtin() {
        // The real report this fixes: `claude-personal` is an alias for
        // `CLAUDE_CONFIG_DIR=... command claude`. Naming the wrapper instead of
        // the program hid a running agent from the ignore list, and the plugin
        // claimed the pane its agent was already living in.
        assert_eq!(
            agent_name("CLAUDE_CONFIG_DIR=/Users/x/.claude-personal command claude --version"),
            "claude"
        );
    }

    #[test]
    fn agent_name_looks_past_the_other_transparent_wrappers() {
        assert_eq!(agent_name("env FOO=1 codex"), "codex");
        assert_eq!(agent_name("nohup /usr/bin/npm start"), "npm");
        assert_eq!(agent_name("exec zsh"), "zsh");
        assert_eq!(agent_name("builtin cd /tmp"), "cd");
    }

    /// `sudo` is deliberately not a wrapper. Its options are position-dependent
    /// and some take a value (`sudo -u root make` would name `root`), so seeing
    /// through it needs a table of sudo's own flags that would rot in silence.
    /// Naming `sudo` is only cosmetic: nobody launches an agent CLI under it, so
    /// the ignore list — the thing this fix exists for — is unaffected.
    #[test]
    fn agent_name_does_not_try_to_see_through_sudo() {
        assert_eq!(agent_name("sudo -u root make install"), "sudo");
    }

    #[test]
    fn agent_name_falls_back_when_the_line_is_only_wrappers() {
        assert_eq!(agent_name("command"), "shell");
        assert_eq!(agent_name("env FOO=1"), "shell");
    }

    #[test]
    fn a_wrapper_named_after_a_path_is_still_seen_through() {
        assert_eq!(agent_name("/usr/bin/env claude"), "claude");
    }

    #[test]
    fn display_name_keeps_the_whole_command_line() {
        assert_eq!(
            display_name("cargo build --release", 60),
            "cargo build --release"
        );
    }

    #[test]
    fn display_name_collapses_runs_of_whitespace() {
        assert_eq!(display_name("sleep    6", 60), "sleep 6");
        assert_eq!(display_name("  npm\trun start  ", 60), "npm run start");
    }

    #[test]
    fn display_name_truncates_to_the_cap() {
        assert_eq!(
            display_name("git push origin main --force", 12),
            "git push or\u{2026}"
        );
    }

    #[test]
    fn display_name_falls_back_when_there_is_no_command() {
        assert_eq!(display_name("", 60), "shell");
        assert_eq!(display_name("   ", 60), "shell");
    }

    #[test]
    fn display_name_leaves_a_typed_path_alone() {
        // Unlike `agent_name`, the row shows what the user actually typed.
        assert_eq!(
            display_name("/usr/local/bin/npm test", 60),
            "/usr/local/bin/npm test"
        );
    }

    #[test]
    fn truncate_leaves_short_strings_alone() {
        assert_eq!(truncate("cargo build", 60), "cargo build");
    }

    #[test]
    fn truncate_adds_an_ellipsis_when_cutting() {
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        assert_eq!(truncate("ααααααα", 3), "αα…");
    }
}
