#!/usr/bin/env bash
# Fresh stock Debian 13 (trixie) Lima VM -> built Home Manager (standalone) config.
# Idempotent; safe to re-run. Called by create.sh after copying the flake into the VM.
#
# Expected layout (set up by create.sh before calling this):
#   ~/vm-bootstrap/bootstrap.sh   <- this file
#   ~/vm-bootstrap/home/          <- the Home Manager flake
set -euo pipefail

BOOTSTRAP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLAKE_DIR="$BOOTSTRAP_DIR/home"

# Arch -> flake attr (aarch64 is primary, x86_64 is suffixed).
case "$(uname -m)" in
aarch64) ATTR="devstation" ;;
x86_64) ATTR="devstation-x86_64" ;;
*)
	echo "unsupported arch: $(uname -m)" >&2
	exit 2
	;;
esac

echo "==> Flake attr: $ATTR"
echo "==> Flake dir:  $FLAKE_DIR"

# --- 1. system bits Home Manager cannot own on Debian ---------------------
# apt prerequisites: Nix needs curl+xz; zsh is the login shell (HM writes
# ~/.zshrc but cannot chsh on non-NixOS); gnupg for key ops. build-essential +
# python3 are the node-gyp toolchain (make, g++, python3): npm packages with
# native addons (e.g. node-pty, pulled by the T3 Code server) compile on first
# run and fail with "not found: make" without them.
echo "==> Installing apt prerequisites (sudo)"
sudo apt-get update -y
sudo apt-get install -y curl ca-certificates git xz-utils zsh gnupg build-essential python3

# Login shell -> zsh so the HM-generated ~/.zshrc actually loads.
if [ "$(getent passwd "$USER" | cut -d: -f7)" != "/usr/bin/zsh" ]; then
	echo "==> Setting login shell to zsh"
	sudo chsh -s /usr/bin/zsh "$USER"
fi

# Swap: belt-and-suspenders against build/install memory spikes.
# Check /proc/swaps, not `swapon --show`: swapon lives in /usr/sbin, which is
# not on PATH in this execution context, so the guard would misfire on re-runs
# and crash on the already-active /swapfile.
if ! grep -q '^/' /proc/swaps && [ ! -f /swapfile ]; then
	echo "==> Creating 4G swap at /swapfile"
	sudo fallocate -l 4G /swapfile || sudo dd if=/dev/zero of=/swapfile bs=1M count=4096
	sudo chmod 600 /swapfile
	sudo mkswap /swapfile
	sudo swapon /swapfile
	grep -q '^/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab >/dev/null
fi

# sshd: let a remote forward rebind over a leftover socket file. The host keeps
# a persistent `ssh -R /home/dev/.ssh/agent.sock:$SSH_AUTH_SOCK` running (see
# host/lima-agent-forward.sh); without this, every reconnect after an unclean
# drop hits "address already in use" and the VM is left with no working agent.
SSHD_DROPIN=/etc/ssh/sshd_config.d/20-streamlocal-unlink.conf
if [ ! -f "$SSHD_DROPIN" ]; then
	echo "==> Enabling StreamLocalBindUnlink in sshd"
	echo 'StreamLocalBindUnlink yes' | sudo tee "$SSHD_DROPIN" >/dev/null
	sudo systemctl reload ssh
fi

# Mask unused boot-time services: faster restarts, less churn.
echo "==> Masking unused boot-time services"
for unit in \
	apt-daily.timer \
	apt-daily-upgrade.timer \
	man-db.timer \
	motd-news.timer \
	motd-news.service \
	systemd-networkd-wait-online.service \
	NetworkManager-wait-online.service \
	unattended-upgrades.service \
	fwupd.service \
	fwupd-refresh.timer; do
	sudo systemctl disable --now "$unit" 2>/dev/null || true
	sudo systemctl mask "$unit" 2>/dev/null || true
done

# --- 2. Nix (official multi-user installer) -------------------------------
# Put nix on PATH directly, never via nix-daemon.sh: a parent zsh (limactl
# shell runs through a login shell) exports that script's "already sourced"
# guard even when a later login step reset PATH, making re-sourcing a no-op.
# Without this, a re-run would wrongly reinstall nix.
if ! command -v nix >/dev/null 2>&1 && [ -d /nix/var/nix/profiles/default/bin ]; then
	export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"
fi
if ! command -v nix >/dev/null 2>&1; then
	echo "==> Installing Nix (official multi-user)"
	sh <(curl -L https://nixos.org/nix/install) --daemon --yes
	# shellcheck disable=SC1091
	. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
fi
if ! command -v nix >/dev/null 2>&1; then
	echo "    nix still not on PATH. Open a NEW shell and re-run bootstrap.sh."
	exit 1
fi

# Enable flakes for convenience after bootstrap.
NIX_CONF="$HOME/.config/nix/nix.conf"
if [ ! -f "$NIX_CONF" ] || ! grep -q 'experimental-features' "$NIX_CONF"; then
	echo "==> Enabling nix-command + flakes in $NIX_CONF"
	mkdir -p "$(dirname "$NIX_CONF")"
	echo 'experimental-features = nix-command flakes' >>"$NIX_CONF"
fi

# --- 3. home-manager switch -----------------------------------------------
# `path:` prefix: nix reads all files in the dir without requiring git tracking
# (the flake was copied here by create.sh, not cloned as a git repo).
# `-b hm-bak` backs up pre-existing target files instead of aborting.
echo "==> Applying home-manager config (#$ATTR)"
if command -v home-manager >/dev/null 2>&1; then
	home-manager switch -b hm-bak --flake "path:${FLAKE_DIR}#${ATTR}"
else
	nix --extra-experimental-features 'nix-command flakes' \
		run home-manager/release-26.05 -- \
		switch -b hm-bak --flake "path:${FLAKE_DIR}#${ATTR}"
fi

# --- 4. writable configs + shared agent files ------------------------------
# The repo seeds the VM at creation; no live link back to it. Configs land as
# plain writable files (edit freely in the VM; recreate the VM to reset).

# Git identity: not in the repo. create.sh ships a gitignored .env alongside
# this script; the HM git config includes ~/.config/git/identity.inc.
# Always rewritten so a re-run picks up an updated .env.
if [ -f "$BOOTSTRAP_DIR/.env" ]; then
	# shellcheck disable=SC1091
	. "$BOOTSTRAP_DIR/.env"
	: "${GIT_USER_NAME:?missing in .env}" "${GIT_USER_EMAIL:?missing in .env}" "${GIT_SIGNING_PUBKEY:?missing in .env}"
	echo "==> Seeding git identity ($GIT_USER_EMAIL)"
	mkdir -p "$HOME/.config/git"
	cat >"$HOME/.config/git/identity.inc" <<EOF
[user]
	name = $GIT_USER_NAME
	email = $GIT_USER_EMAIL
	signingKey = key::$GIT_SIGNING_PUBKEY
EOF
	echo "$GIT_USER_EMAIL $GIT_SIGNING_PUBKEY" >"$HOME/.config/git/allowed_signers"
else
	echo "==> WARNING: no .env found, git identity/signing not configured"
fi
if [ ! -f "$HOME/.config/herdr/config.toml" ]; then
	echo "==> Seeding herdr config (writable copy)"
	mkdir -p "$HOME/.config/herdr"
	cp "$BOOTSTRAP_DIR/vm-files/herdr-config.toml" "$HOME/.config/herdr/config.toml"
fi

if [ ! -f "$HOME/.config/worktrunk/config.toml" ]; then
	echo "==> Seeding worktrunk config (writable copy)"
	mkdir -p "$HOME/.config/worktrunk"
	cp "$BOOTSTRAP_DIR/vm-files/worktrunk-config.toml" "$HOME/.config/worktrunk/config.toml"
fi

# Shared agent instructions live in the ~/.agents host mount (host side:
# ~/lima/agents/AGENTS.md, yours to create/edit), same pattern as
# ~/.claude/CLAUDE.md in the claude mount. Per-CLI paths symlink into it, so
# every VM sees the same file and edits persist across recreation. Dangling
# until the host file exists; harmless.
echo "==> Linking codex/pi/opencode AGENTS.md into ~/.agents"
mkdir -p "$HOME/.codex" "$HOME/.pi/agent" "$HOME/.config/opencode"
ln -sfn "$HOME/.agents/AGENTS.md" "$HOME/.codex/AGENTS.md"
ln -sfn "$HOME/.agents/AGENTS.md" "$HOME/.pi/agent/AGENTS.md"
ln -sfn "$HOME/.agents/AGENTS.md" "$HOME/.config/opencode/AGENTS.md"

# --- 5. claude code (official installer, not nix) -------------------------
# Kept out of Home Manager: the nix store is read-only so Claude Code's
# self-update cannot work there. The installer drops versions in
# ~/.local/share/claude/versions and points ~/.local/bin/claude at the current
# one; the shell init puts ~/.local/bin on PATH. Skipped if already present.
if [ ! -x "$HOME/.local/bin/claude" ]; then
	echo "==> Installing Claude Code (official installer)"
	curl -fsSL https://claude.ai/install.sh | bash
fi

# OpenCode: same self-update rationale. Installs to ~/.opencode/bin (path is
# hardcoded upstream). --no-modify-path: it would otherwise append to zshrc/
# zshenv, which are read-only Home Manager symlinks; PATH is handled in
# home.nix instead. Update in-VM with `opencode upgrade`.
if [ ! -x "$HOME/.opencode/bin/opencode" ]; then
	echo "==> Installing OpenCode (official installer)"
	curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path
fi

# --- 6. rust toolchain -----------------------------------------------------
# rustup itself comes from Home Manager (read-only store); the toolchains it
# manages live in ~/.rustup, so this only has to seed a default one. minimal
# profile + rustfmt/clippy: skips rust-docs (~100MB) nobody reads in a VM.
# Absolute path: $HOME/.nix-profile/bin is not necessarily on PATH in the
# shell that just ran the HM switch above.
RUSTUP="$HOME/.nix-profile/bin/rustup"
if [ -x "$RUSTUP" ] && ! "$RUSTUP" toolchain list 2>/dev/null | grep -q '^stable'; then
	echo "==> Installing stable Rust toolchain (rustup)"
	"$RUSTUP" toolchain install stable \
		--profile minimal --component rustfmt --component clippy
	"$RUSTUP" default stable
fi

# --- 7. mise (per-project language/toolchain versions) --------------------
# mise itself comes from Home Manager (read-only store); the toolchains it
# installs live in ~/.local/share/mise (writable), same split as rustup. This
# only seeds a writable global config: settings + a default bun (bun is not in
# nixpkgs, unlike Node). A project's mise.toml/.tool-versions/.nvmrc overrides
# the global default, and not_found_auto_install fetches a pinned version the
# first time its tool runs. Absolute path: ~/.nix-profile/bin may not be on
# PATH in the shell that just ran the HM switch.
MISE="$HOME/.nix-profile/bin/mise"
if [ -x "$MISE" ] && [ ! -f "$HOME/.config/mise/config.toml" ]; then
	echo "==> Seeding global mise config + bun default"
	mkdir -p "$HOME/.config/mise"
	cat >"$HOME/.config/mise/config.toml" <<'TOML'
[settings]
# Fetch a project-pinned version the first time its tool is invoked instead of
# erroring, so entering a repo that pins e.g. node@20 just works.
not_found_auto_install = true
# Honor legacy per-language version files, not only mise.toml/.tool-versions:
# .nvmrc, .node-version, .python-version, .java-version, .go-version. Rust is
# left out on purpose: rustup owns rust-toolchain.toml.
idiomatic_version_file_enable_tools = ["node", "python", "java", "go"]

[tools]
# Global default so `bun` works in any project. A repo pinning a specific bun
# (mise.toml / .tool-versions) overrides this.
bun = "latest"
TOML
	# Install the global bun now so `bun` is present right after creation;
	# reshim populates ~/.local/share/mise/shims for non-interactive PATH.
	"$MISE" install
	"$MISE" reshim
fi

# --- 8. done ---------------------------------------------------------------
cat <<EOF

==> Done.
Open a NEW shell to pick up zsh + the Nix environment, then:
  - gh auth login                 # GitHub CLI credentials (if needed)
  - claude                        # first run prompts for auth

Notes:
  - Login shell changed to zsh; reconnect from the host once:
      limactl shell --reconnect <name>
  - SSH commit signing needs the forwarded host agent (host/lima-agent-forward.sh).
    ~/.ssh/agent.sock is stabilized by zshenv; smoke test: ssh-add -l
EOF
