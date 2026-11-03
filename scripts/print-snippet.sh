#!/usr/bin/env bash
# Prints the block to add to your shell's rc file. Run via:
#   herdr plugin action invoke bayoudhi.shell-progress.print-snippet
#
# Picks the hook from $SHELL, which is the login shell rather than whatever is
# running this script — the rc file you are about to edit belongs to that one.
#
# An installed copy gets the same globbing block the README shows, never its own
# path: herdr checks every install out into a fresh directory, so a hardcoded
# path keeps loading the old build after an update. A linked clone stays where
# it is, so it gets a plain `source` line. tests/snippet.rs holds the README to
# these blocks.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "$(basename "${SHELL:-}")" in
  fish) init=init.fish; rc="~/.config/fish/config.fish" ;;
  bash) init=init.bash; rc="~/.bashrc" ;;
  zsh)  init=init.zsh;  rc="~/.zshrc" ;;
  *)
    echo "# Unrecognized shell: ${SHELL:-unset}."
    echo "# Supported hooks: $(cd "${here}/shell" && echo init.*)"
    exit 0
    ;;
esac

echo "# Append to ${rc}:"
echo

case "$here" in
  */plugins/github-installations/* | */plugins/github/*) ;;
  *)
    echo "# herdr-shell-progress"
    echo "source ${here}/shell/${init}"
    exit 0
    ;;
esac

# Each block prefers the layout herdr >= 0.9.5 installs into, falls back to the
# one older herdr used, and takes the newest build. Quoted heredocs: printed
# exactly as written.
case "$init" in
  init.zsh)
    cat <<'EOF'
# herdr-shell-progress
() {
  local f=(~/.config/herdr/plugins/github-installations/bayoudhi.shell-progress-*/*/checkout/shell/init.zsh(Nom)
           ~/.config/herdr/plugins/github/bayoudhi.shell-progress-*/shell/init.zsh(Nom))
  (( $#f )) && source $f[1]
}
EOF
    ;;
  init.bash)
    cat <<'EOF'
# herdr-shell-progress
_hsp_dir=$(ls -td ~/.config/herdr/plugins/github-installations/bayoudhi.shell-progress-*/*/checkout/ 2>/dev/null | head -1)
[ -r "$_hsp_dir/shell/init.bash" ] ||
  _hsp_dir=$(ls -td ~/.config/herdr/plugins/github/bayoudhi.shell-progress-*/ 2>/dev/null | head -1)
[ -r "$_hsp_dir/shell/init.bash" ] && . "$_hsp_dir/shell/init.bash"
unset _hsp_dir
EOF
    ;;
  init.fish)
    cat <<'EOF'
# herdr-shell-progress
set -l _hsp_dirs ~/.config/herdr/plugins/github-installations/bayoudhi.shell-progress-*/*/checkout
set -q _hsp_dirs[1]; or set _hsp_dirs ~/.config/herdr/plugins/github/bayoudhi.shell-progress-*
if set -q _hsp_dirs[1]
    set _hsp_dirs (ls -td $_hsp_dirs)
    test -r "$_hsp_dirs[1]/shell/init.fish"; and source "$_hsp_dirs[1]/shell/init.fish"
end
EOF
    ;;
esac
