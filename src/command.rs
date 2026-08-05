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
