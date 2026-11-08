# Installing herdr-shell-progress: instructions for coding agents

You are installing [herdr-shell-progress](README.md) for the user you are
working with. Follow these steps in order. Each step says what to check and
what to do with the answer. Do not improvise around a failed check: stop and
tell the user what failed.

The install has two parts, and both are required:

1. **The watcher binary**, built from source with `cargo build --release`.
2. **A hook block in the user's shell rc file**, which is what actually runs the
   plugin. Installing the binary alone does nothing.

## Rules

- **Never run `sudo`.** Nothing here needs it.
- **Never install Rust or any other toolchain yourself.** If `cargo` is
  missing, stop and tell the user.
- **Never edit Herdr's own `config.toml`** (`~/.config/herdr/config.toml`). A
  wrong key there makes Herdr reject the whole file and fall back to default
  settings, losing the user's keybindings and theme. Step 7 says how to offer
  the one optional change instead.
- **Ask before you write to the rc file.** Show the user the exact lines and
  the file they go in, and wait for a yes.
- **Copy the hook blocks below exactly**, byte for byte. Do not shorten,
  reformat or "simplify" them.

## Step 1: Which Herdr does the user run?

Run:

```bash
command -v herdr && herdr --version
```

- **`herdr` is found and the version is 0.7.0 or newer:** you will take the
  [TUI path](#step-3a-tui-path) in step 3. This is also right if the user runs
  the desktop app as well — the same hook serves both.
- **`herdr` is found but older than 0.7.0:** stop. Tell the user the plugin
  needs Herdr 0.7.0 or newer.
- **`herdr` is not found:** ask the user whether they run the Herdr desktop
  app. If yes, you will take the
  [desktop-app path](#step-3b-desktop-app-path) in step 3. If no, stop: the
  plugin needs one of the two.

Either way, do step 2 next. Do not skip it.

## Step 2: Check the prerequisites

Run each check. If one fails, stop and tell the user which one.

- **OS:** `uname -s` prints `Darwin` or `Linux`. Nothing else is supported.
- **Rust:** `cargo --version` succeeds. If not, tell the user to install Rust
  from <https://rustup.rs> and to ask you again afterwards.
- **Shell:** `basename "$SHELL"` prints `zsh`, `bash` or `fish`. Any other
  shell is not supported; stop.
  - bash: `bash --version` must report 3.2 or newer.
  - fish: `fish --version` must report 3.1 or newer.

Remember the shell name: it picks the rc file and the hook block in step 4.

## Step 3a: TUI path

```bash
herdr plugin install bayoudhi/herdr-shell-progress
```

This clones the plugin and runs `cargo build --release` itself, which can take
a minute or two. If it reports that the plugin is already installed, that is
fine: continue.

Then go to [step 4](#step-4-add-the-hook-to-the-rc-file) and use the
**installed** block for the user's shell.

## Step 3b: Desktop-app path

There is no `herdr` CLI here, so build from a clone:

```bash
git clone https://github.com/bayoudhi/herdr-shell-progress ~/herdr-shell-progress
cd ~/herdr-shell-progress
cargo build --release
```

If `~/herdr-shell-progress` already exists, check it is this repo
(`git -C ~/herdr-shell-progress remote get-url origin` ends in
`bayoudhi/herdr-shell-progress` or `bayoudhi/herdr-shell-progress.git`). If it
is, run `git pull` and `cargo build --release` inside it instead of cloning. If
it is something else, stop and ask the user where to clone.

Then go to [step 4](#step-4-add-the-hook-to-the-rc-file) and use the
**desktop-app** line for the user's shell.

## Step 4: Add the hook to the rc file

The rc file for each shell:

| Shell | rc file |
|---|---|
| zsh | `${ZDOTDIR:-$HOME}/.zshrc` |
| bash | `~/.bashrc` |
| fish | `~/.config/fish/config.fish` |

**First, check it is not already there:**

```bash
grep -n 'herdr-shell-progress' <rc file>
```

If that finds a line, the hook is already installed. Do not add a second one;
skip to [step 5](#step-5-check-the-hook-loads). If it finds nothing, or the rc
file does not exist yet (grep then exits 2 with "No such file"), the hook is
not installed.

Show the user the block below and the rc file it goes in, and ask for a yes.
Then:

- **The rc file exists:** append the block at the end, after a blank line.
- **It does not exist:** create it with just the block in it. For fish, create
  its directory first: `mkdir -p ~/.config/fish`.

### Installed block (TUI path)

**zsh:**

```zsh
# herdr-shell-progress
() {
  local f=(~/.config/herdr/plugins/github-installations/bayoudhi.shell-progress-*/*/checkout/shell/init.zsh(Nom)
           ~/.config/herdr/plugins/github/bayoudhi.shell-progress-*/shell/init.zsh(Nom))
  (( $#f )) && source $f[1]
}
```

**bash:**

```bash
# herdr-shell-progress
_hsp_dir=$(ls -td ~/.config/herdr/plugins/github-installations/bayoudhi.shell-progress-*/*/checkout/ 2>/dev/null | head -1)
[ -r "$_hsp_dir/shell/init.bash" ] ||
  _hsp_dir=$(ls -td ~/.config/herdr/plugins/github/bayoudhi.shell-progress-*/ 2>/dev/null | head -1)
[ -r "$_hsp_dir/shell/init.bash" ] && . "$_hsp_dir/shell/init.bash"
unset _hsp_dir
```

**fish:**

```fish
# herdr-shell-progress
set -l _hsp_dirs ~/.config/herdr/plugins/github-installations/bayoudhi.shell-progress-*/*/checkout
set -q _hsp_dirs[1]; or set _hsp_dirs ~/.config/herdr/plugins/github/bayoudhi.shell-progress-*
if set -q _hsp_dirs[1]
    set _hsp_dirs (ls -td $_hsp_dirs)
    test -r "$_hsp_dirs[1]/shell/init.fish"; and source "$_hsp_dirs[1]/shell/init.fish"
end
```

### Desktop-app line (desktop-app path)

Two lines, for the user's shell:

| Shell | Lines |
|---|---|
| zsh | `# herdr-shell-progress` then `source ~/herdr-shell-progress/shell/init.zsh` |
| bash | `# herdr-shell-progress` then `source ~/herdr-shell-progress/shell/init.bash` |
| fish | `# herdr-shell-progress` then `source ~/herdr-shell-progress/shell/init.fish` |

### bash on macOS

macOS terminals usually start bash as a login shell, which reads
`~/.bash_profile` rather than `~/.bashrc`. If the user's shell is bash and
`uname -s` printed `Darwin`, check that `~/.bash_profile` sources `~/.bashrc`:

```bash
grep -n bashrc ~/.bash_profile
```

If that finds a line, nothing to do. If it finds nothing, or `~/.bash_profile`
does not exist, tell the user that login shells will not load the hook, and
offer to add this line to `~/.bash_profile` (creating the file if it is
missing):

```bash
[ -f ~/.bashrc ] && . ~/.bashrc
```

Add it only after a yes. On a no, carry on: step 5 then checks only
non-login shells, so say so.

## Step 5: Check the hook loads

The hook does nothing outside a Herdr terminal, so check it by starting a new
interactive shell that pretends to be one, and asking whether the hook defined
its function. Run the line for the user's shell:

```bash
env -u HERDR_PANE_ID HC_TERMINAL_ID=hsp-check zsh  -ic 'typeset -f _hsp_preexec >/dev/null && echo HSP_LOADED'
env -u HERDR_PANE_ID HC_TERMINAL_ID=hsp-check bash -ic 'declare -F _hsp_preexec >/dev/null && echo HSP_LOADED'
env -u HERDR_PANE_ID HC_TERMINAL_ID=hsp-check fish -ic 'functions -q _hsp_preexec; and echo HSP_LOADED'
```

For bash on macOS, also run the login-shell variant, which reads
`~/.bash_profile` the way the user's terminal will:

```bash
env -u HERDR_PANE_ID HC_TERMINAL_ID=hsp-check bash -lic 'declare -F _hsp_preexec >/dev/null && echo HSP_LOADED'
```

It prints `HSP_LOADED` when the hook loaded. A shell may also print noise such
as `bash: no job control in this shell` on stderr; that is harmless and comes
from running an interactive shell without a terminal.

Then remove the state directory the check created, and its parent if the
check left it empty:

```bash
rm -rf "${XDG_STATE_HOME:-$HOME/.local/state}/herdr-shell-progress/hc-hsp-check"
rmdir "${XDG_STATE_HOME:-$HOME/.local/state}/herdr-shell-progress" 2>/dev/null || true
```

If it does not print `HSP_LOADED`:

- Check the binary was built: `target/release/herdr-shell-progress` must exist
  and be executable in the plugin directory (TUI path:
  `herdr plugin list --json` shows it as `plugin_root`; desktop-app path:
  `~/herdr-shell-progress`). If it is missing, the build failed; show the user
  the build output.
- Check the block landed in the rc file from step 4, unchanged.
- Otherwise stop and tell the user what you saw.

## Step 6: Tell the user how to see it

Tell the user, in these words or close to them. Give only the version for the
path you took.

TUI path:

> Open a **new** pane — an rc file only runs for new shells — and run
> `sleep 5`. After two seconds it shows in the Herdr sidebar as `sleep 5`.

Desktop-app path:

> Open a **new** terminal in the app — an rc file only runs for new shells —
> and run `sleep 5`. After two seconds the terminal shows under **Working** in
> the Agents list.

## Step 7 (TUI path only): Offer the elapsed label

Herdr's default sidebar rows do not show the plugin's `running 4s` and
`ok · 4s` labels. Tell the user that adding this to their Herdr `config.toml`
shows them, and **let them make the change themselves**:

```toml
[ui.sidebar.agents]
rows = [["state_icon", "workspace", "tab"], ["agent", "state_text"]]
```

followed by `herdr config check` and `herdr server reload-config`.

Do not write it for them (see [Rules](#rules)). The plugin's other settings are
optional, and the defaults are good; see [Configure](README.md#configure) if the
user asks.
