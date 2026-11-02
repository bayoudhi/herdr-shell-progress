# herdr-shell-progress — report slow shell commands to the Herdr sidebar.
# Source this from .zshrc. Safe to source outside Herdr: it returns immediately.

# Herdr's TUI gives each pane HERDR_PANE_ID and takes reports over its socket.
# The desktop app gives each terminal HC_TERMINAL_ID and takes them as OSC 7501
# sequences written into the terminal. The TUI wins when both are set: that is
# a Herdr pane inside an app terminal, and the pane is where the command runs.
if [[ -n "$HERDR_PANE_ID" ]]; then
  typeset -g _HSP_PANE="$HERDR_PANE_ID" _HSP_KEY="${HERDR_PANE_ID//:/-}"
  typeset -gi _HSP_OSC=0
elif [[ -n "$HC_TERMINAL_ID" ]]; then
  typeset -g _HSP_PANE="$HC_TERMINAL_ID" _HSP_KEY="hc-${HC_TERMINAL_ID//:/-}"
  typeset -gi _HSP_OSC=1
else
  return 0
fi

# Locate the binary relative to this file unless the user pinned one.
if [[ -z "$HSP_BIN" ]]; then
  HSP_BIN="${${(%):-%x}:A:h}/../target/release/herdr-shell-progress"
fi
[[ -x "$HSP_BIN" ]] || return 0

# EPOCHREALTIME is a builtin parameter; without it we would need a fork per prompt.
zmodload zsh/datetime 2>/dev/null || return 0
autoload -Uz add-zsh-hook

typeset -g _HSP_STATE_DIR="${HERDR_PLUGIN_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/herdr-shell-progress}/$_HSP_KEY"
typeset -g _HSP_PID=0
mkdir -p "$_HSP_STATE_DIR"

_hsp_preexec() {
  # $2 is the command with aliases expanded, on a single line; $1 is the raw
  # text typed, which hides `claude-personal='... command claude'` behind the
  # alias and slips it past the ignore list. $3 is expanded too but keeps the
  # original newlines, and a multi-line function definition would corrupt the
  # single-line `cmd` file the watcher reads back. $1 is the fallback because $2
  # is empty when the history mechanism is off.
  local cmd="${2:-$1}"

  # Safety net only. `precmd` zeroes _HSP_PID as soon as it has signalled the
  # watcher, so this fires just in the odd case where precmd did not run at all.
  # It deliberately does NOT reach a watcher that is lingering out a successful
  # command's sticky window: keeping a dead watcher's PID around to signal later
  # is how you eventually SIGTERM an unrelated process that inherited it. The
  # lingering watcher notices it has been replaced on its own, by watching the
  # marker file that the new watcher clears below.
  if (( _HSP_PID )); then
    kill -TERM $_HSP_PID 2>/dev/null
    _HSP_PID=0
  fi

  print -r -- "$cmd" > "$_HSP_STATE_DIR/cmd"

  # Only pay for a clear when a sticky label actually exists.
  local -a clear backend
  [[ -f "$_HSP_STATE_DIR/marker" ]] && clear=(--clear-first)

  if (( _HSP_OSC )); then
    backend=(--osc)
    # The desktop app keeps a finished command's record until it is cleared.
    # Clear it here, while the shell still owns the terminal and nothing else
    # is writing to it, and only when there is one: an ordinary command costs
    # no output at all.
    if [[ -s "$_HSP_STATE_DIR/osc-shown" ]]; then
      print -rn -- $'\e]7501;state=clear\e\\' 2>/dev/null >/dev/tty
      : >| "$_HSP_STATE_DIR/osc-shown"
    fi
    # The watcher reads a non-empty `exit` as "this command is already over",
    # and a report it never got to finish belongs to some earlier command.
    : >| "$_HSP_STATE_DIR/exit"
    : >| "$_HSP_STATE_DIR/osc-start"
  fi

  # EPOCHREALTIME is seconds.microseconds; multiply by 1000 to get milliseconds.
  local -i start_ms=$(( EPOCHREALTIME * 1000 ))

  "$HSP_BIN" watch \
    --pane "$_HSP_PANE" \
    --shell-pid $$ \
    --start-ms $start_ms \
    --state-dir "$_HSP_STATE_DIR" \
    $clear $backend >/dev/null 2>&1 &!
  _HSP_PID=$!
}

_hsp_precmd() {
  # Must be the first statement: $? is the finished command's status.
  local code=$?
  (( _HSP_PID )) || return 0
  print -r -- "$code" >| "$_HSP_STATE_DIR/exit"
  kill -USR1 $_HSP_PID 2>/dev/null
  # Forget the PID immediately. The watcher may live on (a successful command
  # keeps its label up for a while) but it is no longer ours to signal, and the
  # PID becomes reusable by the OS the moment it does exit.
  _HSP_PID=0
  # The desktop app's final report is written from here, in the foreground:
  # the command is over and the prompt not yet drawn, so nothing else is
  # writing to the terminal. `osc-start` exists only if the watcher reported
  # this command, so a fast one costs no fork. It must be tested after `exit`
  # is written — see `osc_watch.rs` for the race that ordering settles.
  if (( _HSP_OSC )) && [[ -s "$_HSP_STATE_DIR/osc-start" ]]; then
    "$HSP_BIN" finish --state-dir "$_HSP_STATE_DIR" --exit $code >/dev/null 2>&1
  fi
}

_hsp_zshexit() {
  (( _HSP_PID )) && kill -TERM $_HSP_PID 2>/dev/null
}

add-zsh-hook preexec _hsp_preexec
add-zsh-hook precmd _hsp_precmd
add-zsh-hook zshexit _hsp_zshexit
