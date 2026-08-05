#!/usr/bin/env bash
# Prints the line to add to .zshrc. Run via:
#   herdr plugin action invoke hamza.shell-progress.print-snippet
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
echo "# herdr-shell-progress"
echo "source ${here}/shell/init.zsh"
