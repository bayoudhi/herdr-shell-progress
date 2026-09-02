#!/usr/bin/env bash
# Prints the line to add to your shell's rc file. Run via:
#   herdr plugin action invoke bayoudhi.shell-progress.print-snippet
#
# Picks the hook from $SHELL, which is the login shell rather than whatever is
# running this script — the rc file you are about to edit belongs to that one.
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

echo "# herdr-shell-progress — append to ${rc}"
echo "source ${here}/shell/${init}"
