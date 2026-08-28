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
# chsh on non-NixOS); gnupg for key ops. build-essential + python3 are the
# node-gyp toolchain: npm packages with native addons compile on first run and
# fail with "not found: make" without them. sudo so the dev user can install
# apt packages later without a root shell.
echo "==> Installing apt prerequisites"
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y \
	curl ca-certificates git xz-utils zsh gnupg build-essential python3 sudo

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

# --- 3. Nix (single-user, no daemon) --------------------------------------
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

# --- 4. home-manager switch ------------------------------------------------
# `path:` prefix: nix reads all files in the dir without requiring git tracking
# (the flake was copied in by `wbox build`, not cloned as a git repo).
# `-b hm-bak` backs up pre-existing target files instead of aborting.
echo "==> Applying home-manager config (#$ATTR)"
as_dev "
	if command -v home-manager >/dev/null 2>&1; then
		home-manager switch -b hm-bak --flake 'path:${FLAKE_DIR}#${ATTR}'
	else
		nix --extra-experimental-features 'nix-command flakes' \
			run home-manager/release-26.05 -- \
			switch -b hm-bak --flake 'path:${FLAKE_DIR}#${ATTR}'
	fi
"

# --- 5. claude code (official installer, not nix) -------------------------
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
# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
as_dev '
	if [ ! -x "$HOME/.opencode/bin/opencode" ]; then
		curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path
	fi
'

# --- 6. rust toolchain -----------------------------------------------------
# rustup itself comes from Home Manager (read-only store); the toolchains it
# manages live in ~/.rustup, so this only has to seed a default one. minimal
# profile + rustfmt/clippy: skips rust-docs (~100MB) nobody reads in a VM.
echo "==> Seeding the stable Rust toolchain"
# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
as_dev '
	RUSTUP="$HOME/.nix-profile/bin/rustup"
	if [ -x "$RUSTUP" ] && ! "$RUSTUP" toolchain list 2>/dev/null | grep -q "^stable"; then
		"$RUSTUP" toolchain install stable \
			--profile minimal --component rustfmt --component clippy
		"$RUSTUP" default stable
	fi
'

# --- 7. mise (per-project language/toolchain versions) --------------------
# mise itself comes from Home Manager (read-only store); the toolchains it
# installs live in ~/.local/share/mise (writable), same split as rustup. This
# only seeds a writable global config: settings + a default bun (bun is not in
# nixpkgs, unlike Node).
echo "==> Seeding the global mise config + bun default"
# shellcheck disable=SC2016 # $HOME etc. expand in the dev shell, not here
as_dev '
	MISE="$HOME/.nix-profile/bin/mise"
	if [ -x "$MISE" ] && [ ! -f "$HOME/.config/mise/config.toml" ]; then
		mkdir -p "$HOME/.config/mise"
		cat >"$HOME/.config/mise/config.toml" <<TOML
[settings]
# Fetch a project-pinned version the first time its tool is invoked instead of
# erroring, so entering a repo that pins e.g. node@20 just works.
not_found_auto_install = true
# Honor legacy per-language version files, not only mise.toml/.tool-versions:
# .nvmrc, .node-version, .python-version, .java-version, .go-version. Rust is
# left out on purpose: rustup owns rust-toolchain.toml.
idiomatic_version_file_enable_tools = ["node", "python", "java", "go"]

[tools]
# Global default so \`bun\` works in any project. A repo pinning a specific bun
# (mise.toml / .tool-versions) overrides this.
bun = "latest"
TOML
		"$MISE" install
		"$MISE" reshim
	fi
'

# --- 8. done ---------------------------------------------------------------
echo "==> Base image ready. provision.sh runs per sandbox at \`wbox create\`."
