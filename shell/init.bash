# herdr-shell-progress — report slow shell commands to the Herdr sidebar.
# Source this from .bashrc. Safe to source outside Herdr: it returns
# immediately. Written for bash 3.2, the version macOS still ships.

[[ -n "$HERDR_PANE_ID" ]] || return 0

# Locate the binary relative to this file unless the user pinned one.
if [[ -z "$HSP_BIN" ]]; then
  HSP_BIN="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../target/release/herdr-shell-progress"
fi
[[ -x "$HSP_BIN" ]] || return 0

_HSP_STATE_DIR="${HERDR_PLUGIN_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/herdr-shell-progress}/${HERDR_PANE_ID//:/-}"
_HSP_PID=0
_HSP_LAST_STATUS=0
# The DEBUG trap fires before every simple command, including each element of a
# pipeline. Only a fire while armed starts a watcher, and `precmd` re-arms once
# per prompt, which turns "every command" into "every command line".
_HSP_ARMED=0
mkdir -p "$_HSP_STATE_DIR"

# The command line as typed, on one line.
#
# `$BASH_COMMAND` would be cheaper — no subshell — but the DEBUG trap sees a
# pipeline one element at a time, so it holds `npm run build` where the user
# wrote `npm run build | tee log`. The sidebar row shows the whole line, so the
# whole line is what gets written.
_hsp_command_line() {
  local line
  line="$(HISTTIMEFORMAT= history 1)"
  # Strip the leading index that `history` prints: trim, drop the first field,
  # trim again.
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line#* }"
  line="${line#"${line%%[![:space:]]*}"}"
  # A multi-line command would corrupt the single-line file the watcher reads.
  line="${line//$'\n'/ }"
  # History can be off, in which case the first element of the pipeline is
  # still better than nothing.
  [[ -n "$line" ]] || line="$BASH_COMMAND"
  printf '%s' "$line"
}

# $1 is the command line as typed. $2 is the alias-expanded leading command,
# which only our own DEBUG trap can supply.
_hsp_preexec() {
  local cmd="$1"
  local expanded="$2"

  # Safety net only. `precmd` zeroes _HSP_PID as soon as it has signalled the
  # watcher, so this fires just in the odd case where precmd did not run at all.
  # It deliberately does NOT reach a watcher that is lingering out a successful
  # command's sticky window: keeping a dead watcher's PID around to signal later
  # is how you eventually SIGTERM an unrelated process that inherited it. The
  # lingering watcher notices it has been replaced on its own, by watching the
  # marker file that the new watcher clears below.
  if [[ "$_HSP_PID" != 0 ]]; then
    kill -TERM "$_HSP_PID" 2>/dev/null
    _HSP_PID=0
  fi

  printf '%s\n' "$cmd" > "$_HSP_STATE_DIR/cmd"

  # `cmd` carries the line as typed, which hides `cc='claude'` behind the alias
  # and would slip it past the ignore list. `$BASH_COMMAND` is expanded, so it
  # names the program that actually ran. The watcher reduces it the same way it
  # reduces a command line — skipping VAR=value assignments and transparent
  # wrappers — so it gets the whole leading command, not a guess at its head.
  #
  # Under bash-preexec there is no expanded form to pass on, and a name left
  # from an earlier command must not be matched against this one.
  if [[ -n "$expanded" ]]; then
    printf '%s\n' "$expanded" > "$_HSP_STATE_DIR/name"
  elif [[ -f "$_HSP_STATE_DIR/name" ]]; then
    rm -f "$_HSP_STATE_DIR/name"
  fi

  # Only pay for a clear when a sticky label actually exists.
  local clear=""
  [[ -f "$_HSP_STATE_DIR/marker" ]] && clear="--clear-first"

  # bash gained EPOCHREALTIME in 5.0, and macOS still ships 3.2, so the watcher
  # reads its own clock.
  "$HSP_BIN" watch \
    --pane "$HERDR_PANE_ID" \
    --shell-pid $$ \
    --start-now \
    --state-dir "$_HSP_STATE_DIR" \
    $clear >/dev/null 2>&1 &
  _HSP_PID=$!
  disown $_HSP_PID 2>/dev/null
}

# Runs first in PROMPT_COMMAND, before anything else can overwrite `$?`.
_hsp_capture_status() {
  _HSP_LAST_STATUS=$?
}

# Runs last in PROMPT_COMMAND. Arming here rather than on entry means no other
# PROMPT_COMMAND entry can consume the arm before the user's next command does.
_hsp_precmd() {
  if [[ "$_HSP_PID" != 0 ]]; then
    printf '%s\n' "$_HSP_LAST_STATUS" > "$_HSP_STATE_DIR/exit"
    kill -USR1 "$_HSP_PID" 2>/dev/null
    # Forget the PID immediately. The watcher may live on (a successful command
    # keeps its label up for a while) but it is no longer ours to signal, and
    # the PID becomes reusable by the OS the moment it does exit.
    _HSP_PID=0
  fi
  _HSP_ARMED=1
}

_hsp_debug_trap() {
  # First statement: $BASH_COMMAND tracks whatever is running, including the
  # commands this handler runs itself.
  local expanded="$BASH_COMMAND"
  [[ "$_HSP_ARMED" == 1 ]] || return 0
  _HSP_ARMED=0
  _hsp_preexec "$(_hsp_command_line)" "$expanded"
}

_hsp_exit() {
  [[ "$_HSP_PID" != 0 ]] && kill -TERM "$_HSP_PID" 2>/dev/null
}

# `${preexec_functions+x}` looks like the natural test and is wrong: it reports
# an array that exists but is empty — the normal state of a fresh bash-preexec —
# as unset, because it resolves element zero. `declare -p` sees the array
# itself.
if declare -p preexec_functions >/dev/null 2>&1; then
  # bash-preexec is already loaded — by Atuin, by iTerm2's shell integration,
  # or by the user. It owns the DEBUG trap, and installing a second one would
  # start two watchers per command, so hook into it instead. It hands preexec
  # the whole command line itself, so `_hsp_command_line` is not needed.
  preexec_functions+=(_hsp_preexec)
  precmd_functions=(_hsp_capture_status "${precmd_functions[@]}" _hsp_precmd)
  _hsp_install_exit_trap=1
elif [[ -z "$(trap -p DEBUG)" ]]; then
  trap '_hsp_debug_trap' DEBUG
  PROMPT_COMMAND="_hsp_capture_status${PROMPT_COMMAND:+;$PROMPT_COMMAND};_hsp_precmd"
  _hsp_install_exit_trap=1
fi

# Never take an EXIT trap that already belongs to someone else.
if [[ -n "$_hsp_install_exit_trap" && -z "$(trap -p EXIT)" ]]; then
  trap '_hsp_exit' EXIT
fi
unset _hsp_install_exit_trap
# A DEBUG trap already belongs to someone else and bash-preexec is not there to
# share it. Taking it would break whatever installed it, so this plugin stays
# inert rather than fighting for it; loading bash-preexec first makes both work.
