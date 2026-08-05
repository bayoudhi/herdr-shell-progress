use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub pane: String,
    pub shell_pid: i32,
    pub start_ms: u64,
    pub state_dir: PathBuf,
    pub clear_first: bool,
}

/// Hand-rolled rather than using a CLI crate: this binary is spawned once per
/// shell prompt, so startup time and binary size matter more than niceties.
pub fn parse(argv: &[String]) -> Option<Args> {
    let mut it = argv.iter();
    if it.next().map(String::as_str) != Some("watch") {
        return None;
    }

    let mut pane = None;
    let mut shell_pid = None;
    let mut start_ms = None;
    let mut state_dir = None;
    let mut clear_first = false;

    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--pane" => pane = it.next().cloned(),
            "--shell-pid" => shell_pid = it.next()?.parse::<i32>().ok(),
            "--start-ms" => start_ms = it.next()?.parse::<u64>().ok(),
            "--state-dir" => state_dir = it.next().map(PathBuf::from),
            "--clear-first" => clear_first = true,
            _ => return None,
        }
    }

    Some(Args {
        pane: pane?,
        shell_pid: shell_pid?,
        start_ms: start_ms?,
        state_dir: state_dir?,
        clear_first,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_a_full_invocation() {
        let a = parse(&argv(&[
            "watch",
            "--pane",
            "w1:p2",
            "--shell-pid",
            "4242",
            "--start-ms",
            "1754400000123",
            "--state-dir",
            "/tmp/hsp",
        ]))
        .unwrap();
        assert_eq!(a.pane, "w1:p2");
        assert_eq!(a.shell_pid, 4242);
        assert_eq!(a.start_ms, 1_754_400_000_123);
        assert_eq!(a.state_dir, std::path::PathBuf::from("/tmp/hsp"));
        assert!(!a.clear_first);
    }

    #[test]
    fn parses_the_clear_first_flag() {
        let a = parse(&argv(&[
            "watch",
            "--pane",
            "w1:p2",
            "--shell-pid",
            "1",
            "--start-ms",
            "1",
            "--state-dir",
            "/tmp/hsp",
            "--clear-first",
        ]))
        .unwrap();
        assert!(a.clear_first);
    }

    #[test]
    fn rejects_a_missing_subcommand() {
        assert!(parse(&argv(&["--pane", "w1:p2"])).is_none());
    }

    #[test]
    fn rejects_missing_required_flags() {
        assert!(parse(&argv(&["watch", "--pane", "w1:p2"])).is_none());
    }

    #[test]
    fn rejects_a_non_numeric_pid() {
        assert!(parse(&argv(&[
            "watch",
            "--pane",
            "w1:p2",
            "--shell-pid",
            "abc",
            "--start-ms",
            "1",
            "--state-dir",
            "/tmp/hsp",
        ]))
        .is_none());
    }
}
