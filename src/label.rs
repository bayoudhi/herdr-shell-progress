#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure(i32),
    Signal(&'static str),
}

/// Renders a duration the way a person reads a build time.
pub fn format_elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn signal_name(sig: i32) -> Option<&'static str> {
    Some(match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => return None,
    })
}

pub fn classify(exit_code: i32) -> Outcome {
    if exit_code == 0 {
        return Outcome::Success;
    }
    if exit_code > 128 && exit_code < 192 {
        if let Some(name) = signal_name(exit_code - 128) {
            return Outcome::Signal(name);
        }
    }
    Outcome::Failure(exit_code)
}

/// Substitutes `{key}` placeholders. An unknown key is left as literal text so a
/// typo in user config degrades to visible nonsense rather than a silent failure.
pub fn render(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                match vars.iter().find(|(k, _)| *k == key) {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_under_a_minute_is_seconds() {
        assert_eq!(format_elapsed(12_345), "12s");
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(59_999), "59s");
    }

    #[test]
    fn elapsed_under_an_hour_is_minutes_and_padded_seconds() {
        assert_eq!(format_elapsed(60_000), "1m00s");
        assert_eq!(format_elapsed(252_000), "4m12s");
        assert_eq!(format_elapsed(3_599_000), "59m59s");
    }

    #[test]
    fn elapsed_over_an_hour_is_hours_and_padded_minutes() {
        assert_eq!(format_elapsed(3_600_000), "1h00m");
        assert_eq!(format_elapsed(3_840_000), "1h04m");
    }

    #[test]
    fn classify_maps_zero_to_success() {
        assert_eq!(classify(0), Outcome::Success);
    }

    #[test]
    fn classify_maps_nonzero_to_failure() {
        assert_eq!(classify(1), Outcome::Failure(1));
        assert_eq!(classify(127), Outcome::Failure(127));
    }

    #[test]
    fn classify_maps_known_signal_exits_to_signal() {
        assert_eq!(classify(130), Outcome::Signal("SIGINT"));
        assert_eq!(classify(143), Outcome::Signal("SIGTERM"));
    }

    #[test]
    fn classify_falls_back_to_failure_for_unknown_signal_numbers() {
        assert_eq!(classify(200), Outcome::Failure(200));
    }

    #[test]
    fn render_substitutes_known_placeholders() {
        let vars = vec![("elapsed", "4m12s".to_string())];
        assert_eq!(render("running {elapsed}", &vars), "running 4m12s");
    }

    #[test]
    fn render_leaves_unknown_placeholders_literal() {
        let vars: Vec<(&str, String)> = vec![];
        assert_eq!(render("x {nope} y", &vars), "x {nope} y");
    }

    #[test]
    fn render_leaves_unterminated_brace_literal() {
        let vars: Vec<(&str, String)> = vec![];
        assert_eq!(render("a {oops", &vars), "a {oops");
    }
}
