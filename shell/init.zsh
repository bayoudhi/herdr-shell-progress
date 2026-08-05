# herdr-shell-progress — report slow shell commands to the Herdr sidebar.
# Source this from .zshrc. Safe to source outside Herdr: it returns immediately.

[[ -n "$HERDR_PANE_ID" ]] || return 0

# Locate the binary relative to this file unless the user pinned one.
if [[ -z "$HSP_BIN" ]]; then
  HSP_BIN="${${(%):-%x}:A:h}/../target/release/herdr-shell-progress"
fi
[[ -x "$HSP_BIN" ]] || return 0

# EPOCHREALTIME is a builtin parameter; without it we would need a fork per prompt.
zmodload zsh/datetime 2>/dev/null || return 0
autoload -Uz add-zsh-hook

typeset -g _HSP_STATE_DIR="${HERDR_PLUGIN_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/herdr-shell-progress}/${HERDR_PANE_ID//:/-}"
typeset -g _HSP_PID=0
mkdir -p "$_HSP_STATE_DIR"

_hsp_preexec() {
  local cmd="$1"

  # A watcher from a previous command should never outlive it.
  if (( _HSP_PID )); then
    kill -TERM $_HSP_PID 2>/dev/null
    _HSP_PID=0
  fi

  print -r -- "$cmd" > "$_HSP_STATE_DIR/cmd"

  # Only pay for a clear when a sticky label actually exists.
  local -a clear
  [[ -f "$_HSP_STATE_DIR/marker" ]] && clear=(--clear-first)

  # EPOCHREALTIME is seconds.microseconds; strip the dot and divide to get ms.
  local -i start_ms=$(( ${EPOCHREALTIME/./} / 1000 ))

  "$HSP_BIN" watch \
    --pane "$HERDR_PANE_ID" \
    --shell-pid $$ \
    --start-ms $start_ms \
    --state-dir "$_HSP_STATE_DIR" \
    $clear >/dev/null 2>&1 &!
  _HSP_PID=$!
}

_hsp_precmd() {
  # Must be the first statement: $? is the finished command's status.
  local code=$?
  (( _HSP_PID )) || return 0
  print -r -- "$code" > "$_HSP_STATE_DIR/exit"
  kill -USR1 $_HSP_PID 2>/dev/null
  _HSP_PID=0
}

_hsp_zshexit() {
  (( _HSP_PID )) && kill -TERM $_HSP_PID 2>/dev/null
}

add-zsh-hook preexec _hsp_preexec
add-zsh-hook precmd _hsp_precmd
add-zsh-hook zshexit _hsp_zshexit
