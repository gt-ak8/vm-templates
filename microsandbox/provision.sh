#!/usr/bin/env bash
# Per-sandbox provisioning, run as the dev user at every `wbox create`.
#
# Everything in here is unique to one VM, which is exactly why it is not in
# bootstrap.sh: a key baked into devstation-base would be shared by every
# sandbox cut from it. Idempotent; safe to re-run.
#
# Reads from the environment (injected by `wbox create`):
#   GIT_USER_NAME, GIT_USER_EMAIL, WBOX_COPILOT_PLACEHOLDER, WBOX_AUTHORIZED_KEYS,
#   WBOX_GUEST_ENV
set -euo pipefail

WBOX_DIR=/opt/wbox
KEY="$HOME/.ssh/id_ed25519"

# --- 1. the per-VM ssh key -------------------------------------------------
# No passphrase: it is the VM's own identity, used unattended by git to sign
# commits. Signing only, never authentication: git reaches GitHub over HTTPS
# with GH_TOKEN, so an SSO-enforced org has one authorized token instead of one
# key to authorize by hand per sandbox. `wbox create` registers it as a signing
# key and `wbox destroy` deregisters it.
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
	# The private key path, not `key::<pubkey>`: the literal-key form makes
	# `ssh-keygen -Y sign` look the private half up in an ssh-agent, and the
	# sandbox runs none ("Couldn't get agent socket?"). A path signs directly.
	cat >"$HOME/.config/git/identity.inc" <<EOF
[user]
	name = $GIT_USER_NAME
	email = $GIT_USER_EMAIL
	signingKey = $KEY
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
# ~/.agents is a host mount (host side: ~/.wbox/agents, copied from
# ~/lima/agents at each create), same pattern as ~/.claude/CLAUDE.md.
# Per-CLI paths symlink into it, so every VM sees the same file and edits
# persist across recreation. Dangling until the host file exists; harmless.
echo "==> Linking opencode AGENTS.md into ~/.agents"
mkdir -p "$HOME/.config/opencode"
ln -sfn "$HOME/.agents/AGENTS.md" "$HOME/.config/opencode/AGENTS.md"

# --- 5. opencode's Copilot credential -------------------------------------
# The placeholder, not the token: `wbox create` registers the real value as a
# sandbox secret, and the proxy substitutes it on requests to
# api.githubcopilot.com. opencode sends the stored value straight through as a
# bearer token, so the placeholder is all this VM ever needs to hold.
#
# `expires` is set far out because nothing here can refresh: the host's
# `opencode auth login` owns that, and the next `create` picks up whatever it
# wrote. Overwritten every run so a rotated host credential propagates.
if [ -n "${WBOX_COPILOT_PLACEHOLDER:-}" ]; then
	echo "==> Seeding the opencode Copilot credential (placeholder)"
	mkdir -p "$HOME/.local/share/opencode"
	cat >"$HOME/.local/share/opencode/auth.json" <<EOF
{
  "github-copilot": {
    "type": "oauth",
    "access": "$WBOX_COPILOT_PLACEHOLDER",
    "refresh": "$WBOX_COPILOT_PLACEHOLDER",
    "expires": 4102444800000
  }
}
EOF
	chmod 600 "$HOME/.local/share/opencode/auth.json"
else
	echo "==> No Copilot credential from the host, opencode will need a login"
fi

# --- 6. get Claude Code past its first-run prompts -------------------------
# ~/.claude is a host mount (~/.wbox/claude), but ~/.claude.json is per-VM and
# starts without the flags that mark onboarding done. Claude
# Code then opens on the theme picker, the login-method screen and the
# bypass-permissions warning on every fresh sandbox. Seed the flags it checks.
# Only missing keys are written, so a choice made inside the VM wins.
echo "==> Seeding Claude Code first-run flags"
python3 - "$HOME" <<'PYEOF'
import json, os, sys

home = sys.argv[1]
path = os.path.join(home, ".claude.json")
try:
    with open(path) as handle:
        config = json.load(handle)
except (OSError, ValueError):
    config = {}

# bypassPermissionsModeAccepted: ~/.claude/settings.json runs the sandbox in
# bypass mode, which otherwise demands an interactive acceptance.
config.setdefault("hasCompletedOnboarding", True)
config.setdefault("theme", "dark")
config.setdefault("bypassPermissionsModeAccepted", True)
# The trust dialog is per directory. $HOME covers the default landing spot;
# other directories still prompt once, which is what that dialog is for.
config.setdefault("projects", {}).setdefault(home, {}).setdefault(
    "hasTrustDialogAccepted", True
)

with open(path, "w") as handle:
    json.dump(config, handle, indent=2)
os.chmod(path, 0o600)
PYEOF

# --- 7. sshd: who may log in, and as which host ---------------------------
# The host's public keys are the only way in: sshd is key-only (bootstrap.sh)
# and its port is published on the host's loopback only. Rewritten every run
# so a key added on the host reaches the sandbox at the next create.
# Host keys are generated here, not baked (see bootstrap.sh). The ssh config
# wbox writes does not check them anyway. Starting sshd is `wbox create`'s
# job, shared with `wbox start`.
if [ -n "${WBOX_AUTHORIZED_KEYS:-}" ]; then
	echo "==> Writing authorized_keys"
	printf '%s\n' "$WBOX_AUTHORIZED_KEYS" >"$HOME/.ssh/authorized_keys"
	chmod 600 "$HOME/.ssh/authorized_keys"
fi
if ! ls /etc/ssh/ssh_host_*_key >/dev/null 2>&1; then
	echo "==> Generating the sshd host keys"
	sudo ssh-keygen -A >/dev/null
fi

# --- 8. secret placeholders for ssh sessions ------------------------------
# The sandbox secrets (GH_TOKEN, CLAUDE_CODE_OAUTH_TOKEN) are environment
# variables of agent-spawned processes only. An ssh login gets its environment
# from sshd, so ~/.zshenv (home.nix) sources this file. It holds the
# placeholders the proxy substitutes, never a token. Rewritten every run.
echo "==> Writing the login environment (secret placeholders)"
mkdir -p "$HOME/.config/wbox"
{
	echo "# written by wbox create; placeholders, not tokens"
	# Single-quoted: the GH_TOKEN placeholder is literally `$MSB_GH_TOKEN`,
	# and unquoted the shell would expand it to nothing.
	if [ -n "${WBOX_GUEST_ENV:-}" ]; then
		printf '%s\n' "$WBOX_GUEST_ENV" | sed "s/^\([^=]*\)=\(.*\)$/export \1='\2'/"
	fi
} >"$HOME/.config/wbox/env"
chmod 600 "$HOME/.config/wbox/env"

# --- 9. hand the public key back to the host ------------------------------
# `wbox create` reads this marked line off the exec output and registers the
# key with GitHub as a signing key.
echo "WBOX_PUBKEY $PUBKEY"
