# herdr-shell-progress

Herdr shows live progress for recognized coding agents. A pane running
`cargo build --release` for four minutes shows nothing at all.

This plugin fixes that: slow shell commands report `working` with a ticking
elapsed label, and leave a result behind when they finish.

- Fast commands are invisible — nothing flickers when you run `ls`.
- Failures stick until your next command, so you see what broke while away.
- Successes clear themselves after 20 seconds by default.
- Zero socket traffic and zero output for commands under the threshold — with
  one exception: the first command after a sticky failure label spends two
  requests wiping it, however fast that command is.

## Requirements

- Herdr >= 0.7.0
- zsh
- Rust toolchain (to build)

## Install

```bash
git clone <this-repo> ~/herdr-shell-progress
cd ~/herdr-shell-progress
cargo build --release
herdr plugin link ~/herdr-shell-progress
```

`herdr plugin link` does not run build commands, so the `cargo build` above is
required.

Then add to `.zshrc`:

```zsh
source ~/herdr-shell-progress/shell/init.zsh
```

Open a new pane. Run `sleep 5` and watch the sidebar.

## Configure

```bash
herdr plugin config-dir hamza.shell-progress
```

Copy `config.example.toml` into that directory as `config.toml`. Every key is
optional. Changes take effect on your next command — no reload, no restart.

The default `ignore` list includes agent CLIs (`claude`, `codex`, ...). Keep
them: without them this plugin and Herdr's real agent integrations would both
try to own the same pane's state.

## How it works

`preexec` spawns a detached watcher, using only zsh builtins so the sole cost is
one fork per prompt. The watcher sleeps until the threshold; if the command
finishes first it exits having never touched the socket. Otherwise it reports
via `pane.report_agent` and ticks `pane.report_metadata`. `precmd` writes the
exit code and signals the watcher, which posts the final label and exits.

The watcher writes nothing to stdout or stderr — it inherits the pane's tty, so
any output would corrupt your shell session.

## Uninstall

```bash
herdr plugin unlink hamza.shell-progress
```

Then remove the `source` line from `.zshrc`.
