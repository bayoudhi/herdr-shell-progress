//! The rc-file blocks that load the hooks.
//!
//! They are the one part of an install typed in by hand, copied from the README
//! or printed by the `print-snippet` action, so the two copies must agree. And
//! both must keep finding the plugin wherever herdr puts it: herdr 0.9.5 moved
//! installs from `plugins/github/<id>` to
//! `plugins/github-installations/<id>/<generation>/checkout`, and a block that
//! knew only the old place went on loading the stale copy left behind there.
//!
//! Each block is run for real, in its own shell, against a throwaway `HOME`
//! whose fake installs print which one was loaded. A shell that is not
//! installed skips rather than fails.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const REPO: &str = env!("CARGO_MANIFEST_DIR");

/// Where herdr >= 0.9.5 keeps one install, short of the generation directory.
const NEW: &str = ".config/herdr/plugins/github-installations/bayoudhi.shell-progress-0123456789ab";

/// Where older herdr kept it.
const OLD: &str = ".config/herdr/plugins/github/bayoudhi.shell-progress-0123456789ab";

#[derive(Clone, Copy, Debug)]
enum Shell {
    Zsh,
    Bash,
    Fish,
}

const SHELLS: [Shell; 3] = [Shell::Zsh, Shell::Bash, Shell::Fish];

impl Shell {
    /// Also the hook's file extension and the README's code-fence language.
    fn name(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::Fish => "fish",
        }
    }

    fn installed(self) -> bool {
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|d| d.join(self.name()).is_file())
        })
    }

    /// Sources `block` the way an rc file would, with nothing of the user's
    /// loaded first. Returns stdout, after asserting stderr stayed empty: a
    /// block that complains on every shell start is broken too.
    fn source(self, home: &Path, block: &Path) -> String {
        let mut cmd = Command::new(self.name());
        match self {
            Shell::Zsh => cmd.args(["-f", "-c", r#"source "$1""#, "zsh"]),
            Shell::Bash => cmd.args(["--norc", "--noprofile", "-c", r#". "$1""#, "bash"]),
            Shell::Fish => cmd.args(["--no-config", "-c", "source $argv[1]"]),
        };
        let out = cmd
            .arg(block)
            .env("HOME", home)
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.is_empty(), "{self:?} printed to stderr:\n{stderr}");
        String::from_utf8(out.stdout).unwrap()
    }
}

/// Runs `scripts/print-snippet.sh` from `root`, with `$SHELL` set to `shell`.
fn print_snippet(root: &Path, shell: &str) -> String {
    let out = Command::new("bash")
        .arg(root.join("scripts/print-snippet.sh"))
        .env("SHELL", format!("/bin/{shell}"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The block an installed copy prints for `shell`, without the line that says
/// which rc file it goes in.
fn installed_block(shell: Shell) -> String {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(NEW).join("1-0/checkout");
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("shell")).unwrap();
    fs::copy(
        Path::new(REPO).join("scripts/print-snippet.sh"),
        root.join("scripts/print-snippet.sh"),
    )
    .unwrap();
    let out = print_snippet(&root, shell.name());
    let (header, block) = out.split_once("\n\n").unwrap();
    assert!(header.starts_with("# Append to ~/"), "{header}");
    block.to_string()
}

/// Plants a fake install at `home/dir` whose hooks print `LOADED <label>`, and
/// backdates it by `age` so tests can say which build is newest.
fn plant(home: &Path, dir: &str, label: &str, age: Duration) {
    let root = home.join(dir);
    let hooks = root.join("shell");
    fs::create_dir_all(&hooks).unwrap();
    let when = SystemTime::now() - age;
    for shell in SHELLS {
        let hook = hooks.join(format!("init.{}", shell.name()));
        fs::write(&hook, format!("echo LOADED {label}\n")).unwrap();
        File::open(&hook).unwrap().set_modified(when).unwrap();
    }
    // Directories last: writing into one moves its mtime.
    File::open(&hooks).unwrap().set_modified(when).unwrap();
    File::open(&root).unwrap().set_modified(when).unwrap();
}

/// What each installed shell's block loads from `home`, keyed by shell.
fn loaded(home: &Path) -> Vec<(Shell, String)> {
    let blocks = tempfile::tempdir().unwrap();
    SHELLS
        .into_iter()
        .filter(|shell| {
            let ok = shell.installed();
            if !ok {
                eprintln!("skipping: {} is not installed", shell.name());
            }
            ok
        })
        .map(|shell| {
            let path: PathBuf = blocks.path().join(format!("block.{}", shell.name()));
            fs::write(&path, installed_block(shell)).unwrap();
            (shell, shell.source(home, &path))
        })
        .collect()
}

fn assert_loads(home: &Path, want: &str) {
    for (shell, out) in loaded(home) {
        assert_eq!(out, want, "{shell:?}");
    }
}

const HOUR: Duration = Duration::from_secs(3600);

#[test]
fn the_readme_shows_exactly_what_print_snippet_prints() {
    let readme = fs::read_to_string(Path::new(REPO).join("README.md")).unwrap();
    for shell in SHELLS {
        let fenced = format!("```{}\n{}```\n", shell.name(), installed_block(shell));
        assert!(
            readme.contains(&fenced),
            "README.md has no copy of the {shell:?} block print-snippet prints:\n{fenced}"
        );
    }
}

#[test]
fn a_linked_clone_gets_a_plain_source_line() {
    let out = print_snippet(Path::new(REPO), "zsh");
    let line = out.lines().last().unwrap();
    let path = line.strip_prefix("source ").unwrap();
    assert_eq!(
        fs::canonicalize(path).unwrap(),
        fs::canonicalize(Path::new(REPO).join("shell/init.zsh")).unwrap()
    );
}

#[test]
fn an_unknown_shell_gets_a_note_instead_of_a_block() {
    let out = print_snippet(Path::new(REPO), "tcsh");
    assert!(out.starts_with("# Unrecognized shell: /bin/tcsh."), "{out}");
    assert!(out.lines().all(|l| l.starts_with('#')), "{out}");
}

#[test]
fn nothing_installed_loads_nothing_and_says_nothing() {
    let home = tempfile::tempdir().unwrap();
    assert_loads(home.path(), "");
}

#[test]
fn finds_an_install_from_herdr_0_9_5_and_later() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), &format!("{NEW}/1-0/checkout"), "new", HOUR);
    assert_loads(home.path(), "LOADED new\n");
}

#[test]
fn finds_an_install_from_older_herdr() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), OLD, "old", HOUR);
    assert_loads(home.path(), "LOADED old\n");
}

/// Upgrading herdr leaves the old copy where it was. It must lose even when it
/// looks newer than the real install.
#[test]
fn a_copy_left_behind_by_older_herdr_never_wins() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), &format!("{NEW}/1-0/checkout"), "new", 2 * HOUR);
    plant(home.path(), OLD, "old", HOUR);
    assert_loads(home.path(), "LOADED new\n");
}

#[test]
fn takes_the_newest_generation() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), &format!("{NEW}/2-0/checkout"), "second", HOUR);
    plant(
        home.path(),
        &format!("{NEW}/1-0/checkout"),
        "first",
        2 * HOUR,
    );
    assert_loads(home.path(), "LOADED second\n");
}
