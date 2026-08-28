#!/usr/bin/env bash
# Per-sandbox provisioning, run as the dev user at every `wbox create`.
#
# Everything in here is unique to one VM, which is exactly why it is not in
# bootstrap.sh: a key baked into devstation-base would be shared by every
# sandbox cut from it. Idempotent; safe to re-run.
#
# Reads from the environment (injected by `wbox create`):
#   GIT_USER_NAME, GIT_USER_EMAIL
set -euo pipefail

WBOX_DIR=/opt/wbox
KEY="$HOME/.ssh/id_ed25519"

# --- 1. the per-VM ssh key -------------------------------------------------
# No passphrase: it is the VM's own identity, used unattended by git for both
# push and commit signing. `wbox create` registers it with GitHub and
# `wbox destroy` deregisters it.
mkdir -p "$HOME/.ssh"
chmod 700 "$HOME/.ssh"
if [ ! -f "$KEY" ]; then
	echo "==> Generating the per-VM ed25519 key"
	ssh-keygen -t ed25519 -N "" -C "wbox-$(hostname)" -f "$KEY" >/dev/null
fi
PUBKEY="$(cat "$KEY.pub")"

# --- 2. git identity + signing --------------------------------------------
# The HM git config includes ~/.config/git/identity.inc; a missing include is
# silently ignored, so the flake builds without it. Always rewritten so a
# re-run picks up an updated .env.
if [ -n "${GIT_USER_NAME:-}" ] && [ -n "${GIT_USER_EMAIL:-}" ]; then
	echo "==> Seeding git identity ($GIT_USER_EMAIL)"
	mkdir -p "$HOME/.config/git"
	cat >"$HOME/.config/git/identity.inc" <<EOF
[user]
	name = $GIT_USER_NAME
	email = $GIT_USER_EMAIL
	signingKey = key::$PUBKEY
EOF
	echo "$GIT_USER_EMAIL $PUBKEY" >"$HOME/.config/git/allowed_signers"
else
	echo "==> WARNING: GIT_USER_NAME/GIT_USER_EMAIL unset, git identity not configured"
fi

# --- 3. writable configs ---------------------------------------------------
# Plain writable copies: edit freely in the VM, recreate the VM to reset.
if [ ! -f "$HOME/.config/herdr/config.toml" ]; then
	echo "==> Seeding herdr config (writable copy)"
	mkdir -p "$HOME/.config/herdr"
	cp "$WBOX_DIR/vm-files/herdr-config.toml" "$HOME/.config/herdr/config.toml"
fi
if [ ! -f "$HOME/.config/worktrunk/config.toml" ]; then
	echo "==> Seeding worktrunk config (writable copy)"
	mkdir -p "$HOME/.config/worktrunk"
	cp "$WBOX_DIR/vm-files/worktrunk-config.toml" "$HOME/.config/worktrunk/config.toml"
fi

# --- 4. shared agent instructions -----------------------------------------
# ~/.agents is a host mount (host side: ~/lima/agents/AGENTS.md, yours to
# create/edit), same pattern as ~/.claude/CLAUDE.md in the claude mount.
# Per-CLI paths symlink into it, so every VM sees the same file and edits
# persist across recreation. Dangling until the host file exists; harmless.
echo "==> Linking codex/pi/opencode AGENTS.md into ~/.agents"
mkdir -p "$HOME/.codex" "$HOME/.pi/agent" "$HOME/.config/opencode"
ln -sfn "$HOME/.agents/AGENTS.md" "$HOME/.codex/AGENTS.md"
ln -sfn "$HOME/.agents/AGENTS.md" "$HOME/.pi/agent/AGENTS.md"
ln -sfn "$HOME/.agents/AGENTS.md" "$HOME/.config/opencode/AGENTS.md"

# --- 5. hand the public key back to the host ------------------------------
# `wbox create` reads this marked line off the exec output and registers the
# key with GitHub twice: as an authentication key and as a signing key.
echo "WBOX_PUBKEY $PUBKEY"
