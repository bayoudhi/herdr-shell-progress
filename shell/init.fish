# herdr-shell-progress — report slow shell commands to the Herdr sidebar.
# Source this from config.fish. Safe to source outside Herdr: it returns
# immediately. Needs fish >= 3.1 for $last_pid.

set -q HERDR_PANE_ID; or return 0

# Locate the binary relative to this file unless the user pinned one.
if not set -q HSP_BIN
    set -g HSP_BIN (status dirname)/../target/release/herdr-shell-progress
end
test -x "$HSP_BIN"; or return 0

set -l root
if set -q HERDR_PLUGIN_STATE_DIR
    set root $HERDR_PLUGIN_STATE_DIR
else if set -q XDG_STATE_HOME
    set root $XDG_STATE_HOME/herdr-shell-progress
else
    set root $HOME/.local/state/herdr-shell-progress
end

set -g _hsp_state_dir $root/(string replace -a ':' '-' -- $HERDR_PANE_ID)
set -g _hsp_pid 0
mkdir -p $_hsp_state_dir

function _hsp_preexec --on-event fish_preexec
    # Safety net only. `fish_postexec` zeroes _hsp_pid as soon as it has
    # signalled the watcher, so this fires just in the odd case where postexec
    # did not run at all. It deliberately does NOT reach a watcher that is
    # lingering out a successful command's sticky window: keeping a dead
    # watcher's PID around to signal later is how you eventually SIGTERM an
    # unrelated process that inherited it. The lingering watcher notices it has
    # been replaced on its own, by watching the marker file the new one clears.
    if test $_hsp_pid -ne 0
        kill -TERM $_hsp_pid 2>/dev/null
        set -g _hsp_pid 0
    end

    # fish has no aliases in the zsh sense — its abbreviations are already
    # expanded in the line the user submitted, and its functions are named the
    # thing you would ignore — so the line as typed feeds both the sidebar row
    # and the ignore list, and no `name` file is needed.
    #
    # Newlines are collapsed: a multi-line command would otherwise corrupt the
    # single-line `cmd` file the watcher reads back.
    printf '%s\n' (string replace -a \n ' ' -- $argv[1]) >$_hsp_state_dir/cmd

    # Only pay for a clear when a sticky label actually exists.
    set -l clear
    if test -f $_hsp_state_dir/marker
        set clear --clear-first
    end

    # fish has no cheap millisecond clock, so the watcher reads its own.
    $HSP_BIN watch \
        --pane $HERDR_PANE_ID \
        --shell-pid $fish_pid \
        --start-now \
        --state-dir $_hsp_state_dir \
        $clear >/dev/null 2>&1 &
    set -g _hsp_pid $last_pid
    disown $_hsp_pid 2>/dev/null
end

function _hsp_postexec --on-event fish_postexec
    # Must be the first statement: $status is the finished command's.
    set -l code $status
    test $_hsp_pid -ne 0; or return 0
    printf '%s\n' $code >$_hsp_state_dir/exit
    # fish has no kill builtin, so this is a fork the zsh hook does not pay.
    kill -USR1 $_hsp_pid 2>/dev/null
    # Forget the PID immediately. The watcher may live on (a successful command
    # keeps its label up for a while) but it is no longer ours to signal, and
    # the PID becomes reusable by the OS the moment it does exit.
    set -g _hsp_pid 0
end

function _hsp_exit --on-event fish_exit
    test $_hsp_pid -ne 0; and kill -TERM $_hsp_pid 2>/dev/null
end
