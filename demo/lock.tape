# VHS tape for the README's keylock demo.
#
#   brew install vhs
#   vhs demo/lock.tape
#
# Like demo.tape, this records an ISOLATED Herdr session (--session lockdemo)
# so no real panes, tab names, or working directories end up in a published
# GIF. Tear a stuck render down with:
#
#   herdr session stop lockdemo && herdr session delete lockdemo
#
# Requires, in the shell that runs vhs:
#   - this plugin active (its hook sourced), and
#   - keylock 0.2.0 or newer on PATH (github.com/bayoudhi/keylock).
#
# To record against uninstalled local builds, export HSP_DEMO_ENV with the
# overrides before running vhs; the hidden setup line below passes it through.
# It is empty for a normal render. (`${=...}` is zsh's word-split flag: zsh does
# not split unquoted expansions, so without it env would see one giant argument.)
#
#   export HSP_DEMO_ENV='PATH=/path/to/keylock/target/release:$PATH \
#     HSP_BIN=/path/to/herdr-shell-progress/target/release/herdr-shell-progress \
#     HSP_KEYLOCK_BIN=/path/to/keylock/target/release/keylock'
#
# The sidebar's elapsed label only shows if your Herdr config has `state_text`
# in the agents rows:
#
#   [ui.sidebar.agents]
#   rows = [["state_icon", "workspace", "tab"], ["agent", "state_text"]]

Output demo/lock.gif

Set Shell zsh
Set FontSize 13
Set Width 1500
Set Height 640
Set Padding 15
Set Framerate 12
Set PlaybackSpeed 1.0

# Start an isolated session and let the UI settle. The HERDR_* vars are
# stripped because this tape is usually rendered from inside a Herdr pane, and
# Herdr refuses to launch nested. The PATH prefix is there because vhs spawns
# a non-login shell, which on macOS may not have Homebrew's bin on PATH.
Hide
# Record from a scratch directory, not the repo: a neutral workspace name keeps
# branch names and other plugins' rows (PR counts and the like) out of the GIF.
# The demo config narrows the row name so the elapsed label always fits the
# sidebar: Herdr's sidebar is a fixed ~26 columns, so the demo shortens both
# the row name and the running label, and the pane still shows the whole
# command. With the stock config the row reads `keylock run -- ./m\u2026 \u00b7 running 16s`
# and Herdr clips the tail.
Type "export PATH=/opt/homebrew/bin:/usr/local/bin:$PATH"
Enter
# A session of this name from an earlier render would be REUSED, directory and
# all, so the cd below would have no effect. Start from nothing every time.
Type "herdr session stop lockdemo >/dev/null 2>&1; herdr session delete lockdemo >/dev/null 2>&1"
Enter
Type "rm -rf /tmp/keylock-lock-demo-session && mkdir -p /tmp/keylock-lock-demo-session"
Enter
Type "echo 'max_display_len = 8' > /tmp/keylock-lock-demo-session/config.toml"
Enter
Type "echo '[labels]' >> /tmp/keylock-lock-demo-session/config.toml"
Enter
# vhs cannot carry escaped double quotes through a Type string, so the label
# uses TOML's single-quoted form.
Type "echo 'running = '\''{elapsed}'\''' >> /tmp/keylock-lock-demo-session/config.toml"
Enter
Type "cp demo/migrate-sim.sh /tmp/keylock-lock-demo-session/ && cd /tmp/keylock-lock-demo-session"
Enter
Sleep 2s
Type "env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID -u HERDR_SOCKET_PATH HERDR_PLUGIN_CONFIG_DIR=/tmp/keylock-lock-demo-session ${=HSP_DEMO_ENV} herdr --session lockdemo"
Enter
Sleep 7s

Type "clear"
Enter
Sleep 1s
Show

# A locked keylock session. demo/migrate-sim.sh aborts the moment it reads a
# key, so the counter running on is proof that nothing reached it.
#
# Keys below are pressed one at a time with a pause after each, because the
# published GIF labels them with an overlay (demo/keycast.py) whose timings
# follow this sequence. Change the sleeps here and the overlay must change too.
Type "keylock run --locked -- ./migrate-sim.sh"
Enter
Sleep 6s

# Locked: each key is dropped, and the counter keeps climbing.
Type "a"
Sleep 2s
Space
Sleep 2s
Ctrl+C
Sleep 3s

# The phrase unlocks the session; the lock leaves the row within a tick.
Type "unlock"
Sleep 4s

# Unlocked: the next key reaches the job, which aborts.
Type "x"
Sleep 4s

# Tear the demo session down with `prefix+q`. The prefix is ctrl+b out of the
# box — this line uses ctrl+a, so match it to your own binding.
Hide
Ctrl+A
Type "q"
Sleep 3s
# Fallback if your prefix is not ctrl+a: leave the pane's shell, which ends the
# session too. Harmless once the session is already gone.
Type "exit"
Enter
Sleep 2s
