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
Set FontSize 15
Set Width 1100
Set Height 600
Set Padding 15
Set Framerate 12
Set PlaybackSpeed 1.0

# Start an isolated session and let the UI settle. The HERDR_* vars are
# stripped because this tape is usually rendered from inside a Herdr pane, and
# Herdr refuses to launch nested. The PATH prefix is there because vhs spawns
# a non-login shell, which on macOS may not have Homebrew's bin on PATH.
Hide
Type "env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID -u HERDR_SOCKET_PATH PATH=/opt/homebrew/bin:/usr/local/bin:$PATH ${=HSP_DEMO_ENV} herdr --session lockdemo"
Enter
Sleep 6s

Type "clear"
Enter
Sleep 1s
Show

# A locked keylock session: every key is dropped, so the command cannot be
# interrupted. The sidebar row wears the lock while it is locked.
Type "keylock run --locked -- sleep 25"
Enter
Sleep 6s

# Typing does nothing at all — not even Ctrl+C reaches the command.
Type "these keys go nowhere"
Sleep 2s
Ctrl+C
Sleep 4s

# The phrase unlocks it, and the lock leaves the row within a tick.
Type "unlock"
Sleep 5s

# Unlocked again: Ctrl+C now reaches the command and ends it.
Ctrl+C
Sleep 5s

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
