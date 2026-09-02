#!/usr/bin/env bash
# Stock debian:13 microsandbox sandbox -> the devstation base image.
#
# Runs ONCE as root inside the throwaway sandbox that `wbox build` snapshots as
# `devstation-base`. Everything here is shared by every VM cut from that
# snapshot, so nothing per-VM belongs in this file: keys, git identity and the
# writable configs are provision.sh's job, run at every `wbox create`.
#
# Idempotent; safe to re-run. Expected layout (copied in by `wbox build`):
#   /opt/wbox/bootstrap.sh   <- this file
#   /opt/wbox/home/          <- the Home Manager flake
#   /opt/wbox/vm-files/      <- seed configs, read by provision.sh
set -euo pipefail

WBOX_DIR=/opt/wbox
FLAKE_DIR="$WBOX_DIR/home"
DEV_USER=dev
DEV_HOME=/home/dev

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

# Run a command as the dev user with a login-ish environment. No `su -`: the
# login shell is zsh, whose HM-generated rc files do not exist yet during
# bootstrap. PATH is set explicitly instead of relying on profile scripts.
as_dev() {
	runuser -u "$DEV_USER" -- /bin/bash -c "
		export HOME='$DEV_HOME'
		export USER='$DEV_USER'
		export PATH=\"\$HOME/.nix-profile/bin:\$PATH\"
		set -euo pipefail
		$1
	"
}

# --- 1. system bits Home Manager cannot own on Debian ---------------------
# Nix needs curl+xz; zsh is the login shell (HM writes ~/.zshrc but cannot
# chsh on non-NixOS); python3 runs provision.sh's Claude first-run seeding;
# build-essential + python3 are the node-gyp toolchain (make, g++): npm
# packages with native addons compile on first run and fail with
# "not found: make" without them, same as in the Lima template;
# sudo so the dev user can install apt packages later (a language runtime a
# project needs) without a root shell; openssh-server is how the host gets in
# (section 3), procps for the pgrep that guards its start.
echo "==> Installing apt prerequisites"
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y \
	curl ca-certificates git xz-utils zsh python3 sudo build-essential \
	openssh-server procps

# --- 2. the dev user -------------------------------------------------------
if ! id -u "$DEV_USER" >/dev/null 2>&1; then
	echo "==> Creating the $DEV_USER user"
	useradd --create-home --home-dir "$DEV_HOME" --shell /usr/bin/zsh "$DEV_USER"
fi
if [ "$(getent passwd "$DEV_USER" | cut -d: -f7)" != "/usr/bin/zsh" ]; then
	chsh -s /usr/bin/zsh "$DEV_USER"
fi
echo "==> Granting passwordless sudo to $DEV_USER"
echo "$DEV_USER ALL=(ALL) NOPASSWD:ALL" >"/etc/sudoers.d/90-$DEV_USER"
chmod 0440 "/etc/sudoers.d/90-$DEV_USER"

# --- 3. sshd ---------------------------------------------------------------
# The guest runs a real sshd on 22; `wbox create` publishes it on a loopback
# port of the host. Key-only, dev only. Nothing starts it here: the guest has
# no init, so `wbox create` and `wbox start` launch it through the agent.
echo "==> Configuring sshd"
cat >/etc/ssh/sshd_config.d/wbox.conf <<EOF
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
AllowUsers $DEV_USER
EOF
# apt generated host keys; baked in, every sandbox cut from this snapshot
# would present the same ones. provision.sh generates a set per sandbox.
rm -f /etc/ssh/ssh_host_*_key /etc/ssh/ssh_host_*_key.pub

# --- 4. Nix (single-user, no daemon) --------------------------------------
# Single-user: a microVM sandbox has no init managing nix-daemon, and the store
# only ever serves the one dev user. /nix is created up front so the installer
# does not need to sudo for it.
if [ ! -d /nix ]; then
	echo "==> Creating /nix owned by $DEV_USER"
	mkdir -p /nix
	chown "$DEV_USER:$DEV_USER" /nix
fi
if [ ! -e "$DEV_HOME/.nix-profile/bin/nix" ]; then
	echo "==> Installing Nix (single-user)"
	# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
	as_dev 'sh <(curl -L https://nixos.org/nix/install) --no-daemon --yes'
fi

# Flakes: needed by the home-manager switch below and convenient afterwards.
echo "==> Enabling nix-command + flakes"
# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
as_dev '
	NIX_CONF="$HOME/.config/nix/nix.conf"
	if [ ! -f "$NIX_CONF" ] || ! grep -q experimental-features "$NIX_CONF"; then
		mkdir -p "$(dirname "$NIX_CONF")"
		echo "experimental-features = nix-command flakes" >>"$NIX_CONF"
	fi
'

# --- 5. home-manager switch ------------------------------------------------
# `path:` prefix: nix reads all files in the dir without requiring git tracking
# (the flake was copied in by `wbox build`, not cloned as a git repo).
# `-b hm-bak` backs up pre-existing target files instead of aborting.
# The CLI comes from this flake's own `packages.<system>.home-manager`, so it
# is taken from flake.lock. Running `home-manager/release-26.05` instead
# resolves that branch through the GitHub commits API on every build, with no
# token, and fails the build outright once the host IP is rate limited.
echo "==> Applying home-manager config (#$ATTR)"
as_dev "
	if command -v home-manager >/dev/null 2>&1; then
		home-manager switch -b hm-bak --flake 'path:${FLAKE_DIR}#${ATTR}'
	else
		nix --extra-experimental-features 'nix-command flakes' \
			run 'path:${FLAKE_DIR}#home-manager' -- \
			switch -b hm-bak --flake 'path:${FLAKE_DIR}#${ATTR}'
	fi
"

# --- 6. self-updating tools (official installers, not nix) ----------------
# Kept out of Home Manager: the nix store is read-only so Claude Code's
# self-update cannot work there. The installer drops versions in
# ~/.local/share/claude/versions and points ~/.local/bin/claude at the current
# one; the shell init puts ~/.local/bin on PATH.
echo "==> Installing Claude Code and OpenCode (official installers)"
# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
as_dev '
	if [ ! -x "$HOME/.local/bin/claude" ]; then
		curl -fsSL https://claude.ai/install.sh | bash
	fi
'
# OpenCode: same self-update rationale. Installs to ~/.opencode/bin (path is
# hardcoded upstream). --no-modify-path: it would otherwise append to zshrc/
# zshenv, which are read-only Home Manager symlinks; PATH is handled in
# home.nix instead.
#
# Pinned: left to resolve "latest" the installer calls the GitHub API
# unauthenticated, and the whole build fails with "Failed to fetch version
# information" whenever this egress IP has hit the anonymous rate limit.
# Only the seed version is pinned; opencode self-updates from here.
OPENCODE_VERSION=1.18.25
# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
as_dev "
	if [ ! -x \"\$HOME/.opencode/bin/opencode\" ]; then
		curl -fsSL https://opencode.ai/install | VERSION=$OPENCODE_VERSION bash -s -- --no-modify-path
	fi
"

# mise: also out of Home Manager, and for a sharper reason than self-update
# convenience. nixpkgs pins it per release channel, so nixos-26.05 is stuck on
# 2026.5.12 for the life of the channel, and a repo whose mise.toml sets
# `min_version` above that fails outright. From mise.run it lands in
# ~/.local/bin (already on PATH) and `mise self-update` works.
#
# Not pinned, unlike opencode above: the version is baked into the install
# script rather than looked up, so there is no unauthenticated GitHub API call
# to be rate limited on. Set MISE_VERSION to pin a build if you ever need to.
echo "==> Installing mise"
# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
as_dev '
	if [ ! -x "$HOME/.local/bin/mise" ]; then
		curl -fsSL https://mise.run | sh
	fi
'

# --- 7. mise (per-project language/toolchain versions) --------------------
# The binary is installed in section 6; the toolchains it installs live in
# ~/.local/share/mise (writable). This only seeds a writable global config. No
# `[tools]` block: nothing is installed globally, a project declares what it
# needs and mise fetches it on first use.
echo "==> Seeding the global mise config"
# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
as_dev '
	MISE="$HOME/.local/bin/mise"
	if [ -x "$MISE" ] && [ ! -f "$HOME/.config/mise/config.toml" ]; then
		mkdir -p "$HOME/.config/mise"
		cat >"$HOME/.config/mise/config.toml" <<TOML
[settings]
# Fetch a project-pinned version the first time its tool is invoked instead of
# erroring, so entering a repo that pins e.g. node@20 just works.
not_found_auto_install = true
# Honor legacy per-language version files, not only mise.toml/.tool-versions:
# .nvmrc, .node-version, .python-version, .java-version, .go-version.
idiomatic_version_file_enable_tools = ["node", "python", "java", "go"]
TOML
	fi
'

# --- 8. done ---------------------------------------------------------------
echo "==> Base image ready. provision.sh runs per sandbox at \`wbox create\`."
