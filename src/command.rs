/// The label shown when we cannot identify a command name.
const FALLBACK: &str = "shell";

/// Extracts the program name from a command line: first token that is not a
/// `VAR=value` assignment, reduced to its basename.
pub fn agent_name(cmd_line: &str) -> String {
    for token in cmd_line.split_whitespace() {
        if token.contains('=') && !token.starts_with('/') {
            continue;
        }
        let base = token.rsplit('/').next().unwrap_or(token);
        if !base.is_empty() {
            return base.to_string();
        }
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
