#!/usr/bin/env bash
# Host prep. Run once per VM name; idempotent, safe to re-run anytime.
#   ./host-setup.sh [name]   (default: sandbox01)
#
# Global (no-op after the first run, whatever the name):
# - shared dirs backing the lima.yaml mounts
# - `Include ~/.lima/*/ssh.config` at the TOP of ~/.ssh/config, above every Host
#   block (never duplicated; an older config with it lower down is repaired)
# - ControlMaster off for lima-* hosts (guarded; now inert, see below)
# Per-VM (the only part that depends on [name]):
# - launchd agent holding the persistent ssh-agent forward into the named VM;
#   re-run replaces it in place (see host/lima-agent-forward.sh for the mechanism)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="${1:-sandbox01}"

echo "==> Creating shared dirs (~/lima/claude, ~/lima/agents)"
mkdir -p ~/lima/claude ~/lima/agents

CONFIG="$HOME/.ssh/config"
mkdir -p "$HOME/.ssh"
chmod 700 "$HOME/.ssh"
touch "$CONFIG"

# The mux override goes in first so the Include, prepended after it, ends up
# ABOVE it. The Include must sit above every Host block: placed after one it
# belongs to that block, and VS Code's Remote-SSH reads only top-level hosts into
# its "Connect to Host" list. Nested, the lima-* hosts are invisible there, and
# typing one offers "Add New SSH Host", which writes a `HostName lima-<name>`
# entry that shadows the real one and cannot connect.
# Cost of that order: ssh keeps the first value it obtains for a keyword, so
# lima's own ControlMaster/ControlPath/ControlPersist now win and this override
# is inert. Kept because it costs nothing and documents the intent; a stale
# control socket makes ssh warn and fall back to a direct connection.
MUX_HOST='Host lima-*'
if grep -qF "$MUX_HOST" "$CONFIG"; then
	echo "==> lima mux override already present in $CONFIG"
else
	echo "==> Prepending '$MUX_HOST' mux override to $CONFIG"
	printf '%s\n  ControlMaster no\n  ControlPath none\n  ControlPersist no\n\n' "$MUX_HOST" |
		cat - "$CONFIG" >"$CONFIG.new"
	chmod 600 "$CONFIG.new"
	mv "$CONFIG.new" "$CONFIG"
fi

INCLUDE='Include ~/.lima/*/ssh.config'
# Left untouched when it is already above every Host/Match block, wherever and
# however it is laid out there; only a missing or nested one is (re)prepended, so
# a config written by an older run gets repaired without reformatting a good one.
if awk -v inc="$INCLUDE" '
	$0 == inc { found = 1; exit }
	/^[[:space:]]*(Host|Match)[[:space:]]/ { exit }
	END { exit !found }
' "$CONFIG"; then
	echo "==> ssh Include already at the top of $CONFIG"
else
	if grep -qxF "$INCLUDE" "$CONFIG"; then
		echo "==> Hoisting ssh Include above the Host blocks in $CONFIG"
	else
		echo "==> Prepending '$INCLUDE' to $CONFIG"
	fi
	{
		printf '%s\n\n' "$INCLUDE"
		{ grep -vxF "$INCLUDE" "$CONFIG" || true; } | awk 'NF { seen = 1 } seen'
	} >"$CONFIG.new"
	chmod 600 "$CONFIG.new"
	mv "$CONFIG.new" "$CONFIG"
fi

# --- persistent ssh-agent forward into the VM -----------------------------
# Distinct label per VM so multiple instances coexist (and coexist with
# agentic-vms.lima-agent-forward if that repo is also in use).
LABEL="vm-templates.lima-agent-forward.${NAME}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
chmod +x "$SCRIPT_DIR/lima-agent-forward.sh"
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
echo "==> Installing launchd agent $LABEL"
cat >"$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$SCRIPT_DIR/lima-agent-forward.sh</string>
    <string>$NAME</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>30</integer>
  <key>StandardOutPath</key><string>$HOME/Library/Logs/lima-agent-forward-${NAME}.log</string>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/lima-agent-forward-${NAME}.log</string>
</dict>
</plist>
PLIST_EOF
launchctl bootout "gui/$UID/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$UID" "$PLIST"
echo "    log: ~/Library/Logs/lima-agent-forward-${NAME}.log"

echo "==> Done."
