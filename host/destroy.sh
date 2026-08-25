#!/usr/bin/env bash
# Fully destroys a VM: the lima instance plus every per-VM host leftover.
#   ./destroy.sh [name]   (default: sandbox01)
# Idempotent; safe on a VM that is already partially gone.
#
# Global host state (shared dirs ~/lima/*, ssh Include, lima-* mux override)
# is intentionally kept: it serves all other VMs.
set -euo pipefail

NAME="${1:-sandbox01}"
LABEL="vm-templates.lima-agent-forward.${NAME}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

# launchd agent first: KeepAlive would otherwise respawn the forward against
# the dead VM every ThrottleInterval, forever.
if launchctl print "gui/$UID/$LABEL" >/dev/null 2>&1; then
	echo "==> Removing launchd agent $LABEL"
	launchctl bootout "gui/$UID/$LABEL"
else
	echo "==> launchd agent $LABEL not loaded, skipping"
fi
rm -f "$PLIST" "$HOME/Library/Logs/lima-agent-forward-${NAME}.log"

# The VM itself. Deleting also removes ~/.lima/<name>/ (its ssh.config entry
# picked up by the Include, known hosts, disk).
if limactl list --format '{{.Name}}' 2>/dev/null | grep -qx "$NAME"; then
	echo "==> Deleting lima VM $NAME"
	limactl delete -f "$NAME"
else
	echo "==> lima VM $NAME does not exist, skipping"
fi

echo "==> Done. Shared dirs ~/lima/claude and ~/lima/agents kept."
