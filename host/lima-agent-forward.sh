#!/usr/bin/env bash
# Keeps ONE persistent ssh-agent socket inside a lima VM, at a fixed path, for
# as long as the VM is up. Run under launchd (KeepAlive), installed by
# host/host-setup.sh; not meant to be run by hand except to debug.
#
# Why: `ssh: forwardAgent: true` gives every ssh session its OWN socket under
# /tmp/ssh-*/, destroyed when that session ends. Agents in the VM outlive
# logins, so anything holding such a path loses git signing / push the moment
# the session that spawned it drops. This forward is owned by launchd instead
# of a shell, so the socket at $REMOTE_SOCK is always the live one.
#
#   ./lima-agent-forward.sh [instance]     # default: sandbox01
set -uo pipefail

INSTANCE="${1:-sandbox01}"
REMOTE_SOCK="/home/dev/.ssh/agent.sock"

# launchd jobs inherit almost nothing: resolve tools and host agent socket
# explicitly. SSH_AUTH_SOCK is a per-login-session launchd path on macOS,
# so it must be read at start time, never baked into the plist.
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
HOST_SOCK="$(launchctl getenv SSH_AUTH_SOCK || true)"
if [ -z "$HOST_SOCK" ] || [ ! -S "$HOST_SOCK" ]; then
	echo "no usable host SSH_AUTH_SOCK ('$HOST_SOCK'); is the host agent running?" >&2
	exit 1
fi

# Nothing to forward into a stopped VM. Silent exit: launchd retries on its
# ThrottleInterval, and a message here would be logged every few seconds.
limactl list --format '{{.Name}} {{.Status}}' 2>/dev/null |
	grep -qx "$INSTANCE Running" || exit 0

# ControlMaster off: a multiplexed client hands the forward to the master and
# exits 0, which launchd reads as a crash-loop. ExitOnForwardFailure so a
# failed bind restarts instead of sitting with no agent. ServerAlive* to
# notice a dead VM/network fast.
echo "==> forwarding host agent to ${INSTANCE}:${REMOTE_SOCK}"
exec ssh -N -T \
	-o ControlMaster=no -o ControlPath=none \
	-o ExitOnForwardFailure=yes \
	-o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
	-R "${REMOTE_SOCK}:${HOST_SOCK}" \
	"lima-${INSTANCE}"
