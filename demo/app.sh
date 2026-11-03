#!/usr/bin/env bash
# Records demo/app.gif: the plugin in the Herdr desktop app.
#
#   bash demo/app.sh
#
# vhs records only a terminal it runs itself, so this drives the real app and
# keeps the frames it draws. `herdr-app shot` saves the window's own rendering:
# no screen-recording permission, and it works while the window is covered.
#
# Everything runs ISOLATED, so no real spaces, agents or folders end up in a
# published GIF. Spaces live in herdr-core, not in the window, so a second
# window alone would still list yours. The demo gets:
#
#   - its own core: HC_SESSION plus scratch XDG_STATE_HOME / XDG_CONFIG_HOME;
#   - its own window: HERDR_APP_HOME, started by `open`, so it inherits none of
#     this terminal's HC_* variables;
#   - a bare zsh (ZDOTDIR) that loads the installed plugin exactly as the README
#     tells you to, and sets the terminal title the way oh-my-zsh does.
#
# All of it is torn down at the end, including on Ctrl+C.
#
# Herdr turns `done` into `idle` as soon as someone looks at the terminal, and
# a terminal shown in the focused window counts. The window takes focus when it
# opens, so the script hands focus back to whatever had it before; if it can't,
# it asks you to click another window. Don't bring the demo forward while it
# records; the script warns if the last scene was lost that way.
#
# Record from main: the Spaces row shows the folder's branch.
#
# Needs Herdr.app, hc, ffmpeg and python3, with the plugin installed
# (`herdr plugin install bayoudhi/herdr-shell-progress`), on a 2x display.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out=$repo/demo/app.gif

branch=$(git -C "$repo" branch --show-current)
[[ $branch == main ]] || echo "note: on $branch, not main; the GIF will show that branch." >&2

# Short on purpose: the window's control socket lives in here, and a Unix
# socket path must fit in 104 bytes.
work=$(mktemp -d /tmp/hsp-app.XXXXXX)

unset HC_TERMINAL_ID HC_SCOPE HC_TERMINAL_SOCKET
export HC_SESSION=hspdemo
export XDG_STATE_HOME=$work/state XDG_CONFIG_HOME=$work/config
export HERDR_APP_HOME=$work/app
mkdir -p "$work/zdot" "$work/app" "$work/frames"

teardown() {
  herdr-app do app.quit >/dev/null 2>&1 || true
  sleep 1
  hc server stop >/dev/null 2>&1 || true
  [[ $work == /tmp/hsp-app.* ]] && rm -rf "$work"
}
trap teardown EXIT

cat >"$work/zdot/.zshrc" <<'EOF'
PROMPT='%F{blue}%1~%f %F{magenta}❯%f '
# The terminal title, set the way oh-my-zsh sets it: the command line while it
# runs, user@host: folder at the prompt. The agent row puts a telling title after
# the space name and skips user@host ones, so it reads `demo · sleep 6`, then
# `demo`. A shell that never sets a title shows `t1` there instead.
preexec() { print -rn -- $'\e]2;'"$1"$'\a'; }
precmd() { print -Pn '\e]2;user@host: %~\a'; }
() {
  local f=(~/.config/herdr/plugins/github-installations/bayoudhi.shell-progress-*/*/checkout/shell/init.zsh(Nom)
           ~/.config/herdr/plugins/github/bayoudhi.shell-progress-*/shell/init.zsh(Nom))
  (( $#f )) && source $f[1]
}
EOF

# A fresh home opens with a welcome dialog, and a newer app with its notes.
version=$(herdr-app --version | awk '{print $2}')
printf '%s\n' '{"layout": "main", "workspace.panels": [], "frame.welcomed": "finished"}' >"$HERDR_APP_HOME/shared.json"
printf '{"last_run": "%s", "presented": []}\n' "$version" >"$HERDR_APP_HOME/update-notes.json"

# Dark, like the TUI demo, and silent.
settings() {
  printf '%s\n' '{"zoom": 1.0, "sounds": {"enabled": false}, "minimum_contrast": 1.0, "appearance": "dark"}' \
    >"$HERDR_APP_HOME/settings.json"
}
settings

# Terminals inherit the core's environment, so ZDOTDIR goes here.
ZDOTDIR=$work/zdot hc server start >/dev/null

front() { lsappinfo info -only pid "$(lsappinfo front)" | grep -oE 'pid = [0-9]+' | grep -oE '[0-9]+'; }
before=$(front)

# Background and hidden is only how it starts: it shows itself and takes focus
# anyway, which is handled below.
open -g -j -n -a /Applications/Herdr.app \
  --env HC_SESSION="$HC_SESSION" \
  --env XDG_STATE_HOME="$XDG_STATE_HOME" \
  --env XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
  --env HERDR_APP_HOME="$HERDR_APP_HOME"

for _ in $(seq 60); do
  herdr-app status >/dev/null 2>&1 && break
  sleep 0.5
done
herdr-app status >/dev/null

# A first start may open a space of its own; the demo wants exactly one.
herdr-app ls | python3 -c 'import json,sys; [print(s["id"]) for s in json.load(sys.stdin)["spaces"]]' |
  while read -r id; do
    herdr-app do space.close "{\"id\":\"$id\",\"confirmed\":true}" >/dev/null
  done
settings
herdr-app do app.theme '{"name":"graphite"}' >/dev/null

# In case a dialog opened anyway.
for _ in 1 2 3; do
  herdr-app status | python3 -c 'import json,sys; sys.exit(not json.load(sys.stdin)["overlays"])' || break
  herdr-app do overlay.close >/dev/null
done

term=$(herdr-app do space.new "{\"name\":\"demo\",\"folder\":\"$repo\"}" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["terminal"])')

# Wait for the prompt rather than guessing how long zsh takes to start.
for _ in $(seq 40); do
  hc read "$term" 2>/dev/null | grep -q '❯' && break
  sleep 0.25
done

# The window takes focus when it opens, however it is launched, and Herdr
# counts a terminal shown in the focused window as looked at: `done` would turn
# `idle` on screen. It has to stay on screen to keep drawing (minimized, it
# stops), so focus goes back to whatever had it before.
demo=$(pgrep -n -f /Applications/Herdr.app/Contents/MacOS/Herdr)
osascript -l JavaScript -e "ObjC.import('AppKit'); \$.NSRunningApplication.runningApplicationWithProcessIdentifier($before).activateWithOptions(2)" >/dev/null 2>&1 || true
sleep 0.5
if [[ $(front) == "$demo" ]]; then
  echo "Click any window other than the demo to start recording." >&2
  for _ in $(seq 240); do
    [[ $(front) != "$demo" ]] && break
    sleep 0.25
  done
fi
sleep 1

type_keys() {
  local text=$1 c i
  for ((i = 0; i < ${#text}; i++)); do
    c=${text:i:1}
    [[ $c == " " ]] && c=space
    hc keys "$term" "$c" >/dev/null
    sleep 0.09
  done
  hc keys "$term" enter >/dev/null
}

# Frames with the time each was taken, so the GIF plays back in real time.
frames=$work/frames
(
  i=0
  while [[ ! -e $frames/stop ]]; do
    if herdr-app shot "$(printf '%s/f%04d.png' "$frames" "$i")" >/dev/null 2>&1; then
      python3 -c 'import time; print(time.time())' >>"$frames/times.txt"
      i=$((i + 1))
    fi
    sleep 0.04
  done
) &
capture=$!

# A fast command leaves no trace. A slow one crosses the 2s threshold, shows
# under Working, and lands under Done when it finishes.
sleep 1.5
type_keys "ls shell"
sleep 2.5
type_keys "sleep 6"
sleep 10
touch "$frames/stop"
wait "$capture"

if hc get "$term" | python3 -c 'import json,sys; sys.exit(json.load(sys.stdin)["labels"].get("herdr.seen", "0") != "0")'; then
  :
else
  echo "warning: the demo terminal was looked at, so it ends under Idle, not Done." >&2
  echo "         Keep the demo window in the background and run this again." >&2
fi

python3 - "$frames" <<'EOF'
import sys
d = sys.argv[1]
t = [float(x) for x in open(f"{d}/times.txt")]
with open(f"{d}/list.txt", "w") as f:
    for i in range(len(t)):
        hold = t[i + 1] - t[i] if i + 1 < len(t) else 3.0  # linger on the result
        f.write(f"file 'f{i:04d}.png'\nduration {hold:.3f}\n")
    f.write(f"file 'f{len(t) - 1:04d}.png'\n")
EOF

# The sidebar and the start of the terminal, at 2x then scaled down.
ffmpeg -loglevel error -y -f concat -safe 0 -i "$frames/list.txt" -fps_mode vfr \
  -vf "crop=1320:1290:0:0,scale=880:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff:max_colors=96[p];[b][p]paletteuse=dither=none:diff_mode=rectangle" \
  -loop 0 "$out"
echo "wrote $out"
