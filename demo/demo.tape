# VHS tape for the README demo.
#
#   brew install vhs
#   vhs demo/demo.tape
#
# Records an ISOLATED Herdr session (--session demo) so no real panes, tab
# names, or working directories end up in a published GIF. The session is torn
# down at the end; if a render is interrupted, clean up with:
#
#   herdr session stop demo && herdr session delete demo
#
# The plugin must already be active in your shell — this records the real
# thing, it does not simulate it. The sidebar's elapsed label only shows if
# your Herdr config has `state_text` in the agents rows:
#
#   [ui.sidebar.agents]
#   rows = [["state_icon", "workspace", "tab"], ["agent", "state_text"]]

Output demo/demo.gif

Set Shell zsh
Set FontSize 15
Set Width 1100
Set Height 600
Set Padding 15
Set Framerate 12
Set PlaybackSpeed 1.0

# Start an isolated session and let the UI settle.
#
# The HERDR_* vars are stripped because this tape is usually rendered from
# inside a Herdr pane, and Herdr refuses to launch nested — without this you
# get "inception detected" and a recording of a bare shell.
Hide
Type "env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID -u HERDR_SOCKET_PATH herdr --session demo"
Enter
Sleep 6s

# Wipe whatever the login shell printed into the pane (a ~/.zprofile greeting,
# an MOTD) so the recording starts from a clean prompt.
Type "clear"
Enter
Sleep 1s
Show

# A fast command leaves no trace at all — nothing appears in the sidebar.
Type "ls"
Enter
Sleep 3s

# A slow command crosses the 2s threshold, then ticks while it runs. The row
# carries the whole command line, so it reads `sleep 6`, not a bare `sleep`.
Type "sleep 6"
Enter
Sleep 9s

# Its result lands as `ok · 6s`, and clears itself ~20s later.
Sleep 4s

# Tear the demo session down with `prefix+q`. The prefix is ctrl+b out of the
# box — this line uses ctrl+a, so match it to your own binding.
Hide
Ctrl+A
Type "q"
Sleep 2s
