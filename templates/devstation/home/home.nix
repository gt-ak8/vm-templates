{
  pkgs,
  herdr-pkg,
  worktrunk-pkg,
  ...
}:

# Devstation payload: zsh + starship, herdr, agent CLIs.
# Ported from homelab/templates/devstation/home.nix (shell, aliases, starship)
# and agentic-vms/home/base.nix (git identity/signing, SSH agent socket
# stabilizer, zshenv Nix/HM sourcing).
#
# HM only manages what it generates (packages, shell, git). Editable config
# files (herdr) are seeded as writable copies by bootstrap.sh; shared agent
# files (AGENTS.md, CLAUDE.md) live in the host mounts. The repo seeds the VM
# at creation, then the VM owns its config: change = recreate.

{
  home.username = "dev";
  home.homeDirectory = "/home/dev";
  home.stateVersion = "26.05";

  home.packages = with pkgs; [
    ripgrep
    fd
    fzf
    jq
    just
    gh
    lazygit
    codex
    pi-coding-agent
    herdr-pkg
    # Rust: rustup, not the nixpkgs rustc/cargo. Toolchains live in ~/.rustup
    # (writable), so a project's rust-toolchain.toml is honored and
    # `rustup update` works. bootstrap.sh seeds a stable toolchain.
    rustup
    # Node: the `nodejs` attr tracks the current LTS in nixpkgs (24.x on
    # nixos-26.05). Ships npm. Global installs go to ~/.npm-global (see
    # NPM_CONFIG_PREFIX below), the store being read-only. This is the fallback
    # `node` outside a project; mise overrides it inside a repo that pins a
    # version (see below).
    nodejs
    # mise: per-project language/toolchain versions. Same pattern as rustup -
    # the binary is read-only from the store, but the toolchains it installs
    # live in ~/.local/share/mise (writable). Reads a repo's mise.toml /
    # .tool-versions / .nvmrc / .python-version / ... and, with
    # not_found_auto_install, fetches the pinned version on first use. Shell
    # hook is wired in initContent; global config seeded by bootstrap.sh.
    mise
  ];

  # claude-code is deliberately NOT managed here: the nix store is read-only so
  # its self-update cannot work. bootstrap.sh installs it via the official
  # installer into ~/.local/bin.

  home.sessionVariables = {
    EDITOR = "nano";
    # `npm i -g` would target the read-only store prefix. Redirect it to a
    # writable dir; its bin/ is added to PATH in zshenv below.
    NPM_CONFIG_PREFIX = "/home/dev/.npm-global";
  };

  programs.home-manager.enable = true;

  # Upstream HM module (flake input): installs wt + zsh integration
  # (`wt switch` must cd the parent shell; the eval'd init replaces
  # `wt config shell install`, which cannot edit the read-only HM zshrc).
  programs.worktrunk = {
    enable = true;
    package = worktrunk-pkg; # tag-stamped build from flake.nix
  };

  # ------------------------------------------------------------------ git ----
  programs.git = {
    enable = true;

    # SSH commit signing. On a VM the private key never lives on disk: the
    # forwarded ssh-agent signs each commit. Identity + signing key are NOT in
    # this repo: bootstrap.sh writes ~/.config/git/identity.inc (and the
    # allowed_signers file) from the host-side .env. A missing include is
    # silently ignored by git, so the flake builds without it.
    signing = {
      format = "ssh";
      signByDefault = true;
    };

    includes = [ { path = "/home/dev/.config/git/identity.inc"; } ];

    settings = {
      init.defaultBranch = "main";
      core.autocrlf = "input";
      tag.gpgSign = true;
      gpg.ssh.allowedSignersFile = "/home/dev/.config/git/allowed_signers";
      url."git@github.com:".insteadOf = "https://github.com/";
      credential.helper = "cache --timeout=86400";
      diff.colorMoved = "default";
      merge.conflictstyle = "zdiff3";
    };
  };

  # ----------------------------------------------------------------- shell ----
  programs.zsh = {
    enable = true;
    autosuggestion.enable = true;
    syntaxHighlighting.enable = true;

    # ~/.zshenv: read by zsh for ALL invocations (interactive and not).
    # Nix + HM env sourced here so `ssh vm <cmd>` works.
    # SSH agent socket stabilizer also lives here: a non-interactive
    # `ssh lima-<name> <cmd>` must get the stable socket path too.
    envExtra = ''
      if [ -e /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]; then
        . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
      fi
      if [ -e "$HOME/.nix-profile/etc/profile.d/hm-session-vars.sh" ]; then
        . "$HOME/.nix-profile/etc/profile.d/hm-session-vars.sh"
      fi

      # SSH agent socket stabilizer. Everything in the VM reads the stable
      # ~/.ssh/agent.sock instead of a per-session SSH_AUTH_SOCK that dies with
      # the ssh connection that created it (long-lived agents outlive logins).
      # Normally that path is a real socket, remote-forwarded by the host
      # (host/lima-agent-forward.sh under launchd). Leave it alone in that case.
      # Only when it is absent or dangling do we fall back to symlinking the
      # session socket.
      if ! { [ -S "$HOME/.ssh/agent.sock" ] && [ ! -L "$HOME/.ssh/agent.sock" ]; } \
         && [ -n "$SSH_AUTH_SOCK" ] && [ -S "$SSH_AUTH_SOCK" ] \
         && [ "$SSH_AUTH_SOCK" != "$HOME/.ssh/agent.sock" ]; then
        mkdir -p "$HOME/.ssh" && ln -sf "$SSH_AUTH_SOCK" "$HOME/.ssh/agent.sock"
      fi
      [ -e "$HOME/.ssh/agent.sock" ] && export SSH_AUTH_SOCK="$HOME/.ssh/agent.sock"

      # Native-installer bins (claude, opencode), rustup's shims (cargo,
      # rustc, ...), npm's global bin, and mise's shims (bun, node, ... for
      # tools mise manages) in PATH for non-interactive shells too, so
      # `ssh vm claude ...`, `ssh vm cargo test` and `ssh vm bun install` work.
      # The interactive `mise activate` (initContent) does not run for these.
      export PATH="$PATH:$HOME/.local/bin:$HOME/.opencode/bin:$HOME/.cargo/bin:$HOME/.npm-global/bin:$HOME/.local/share/mise/shims"
    '';

    initContent = ''
      bindkey '^f' autosuggest-accept
      [ -n "$TERM" ] && ! infocmp "$TERM" >/dev/null 2>&1 && export TERM=xterm-256color

      # mise: cd-hooks that swap language versions per directory. Interactive
      # only; non-interactive shells rely on the shims dir on PATH (envExtra).
      command -v mise >/dev/null 2>&1 && eval "$(mise activate zsh)"
    '';

    shellAliases = {
      ".." = "cd ..";
      add = "git add .";
      push = "git push";
      pull = "git pull";
      m = "git switch main";
      cc = "claude --dangerously-skip-permissions";
      co = "codex --full-auto";
    };
  };

  programs.starship = {
    enable = true;
    settings = {
      add_newline = false;
      format = "$directory$git_branch$git_status$cmd_duration$line_break$character";
      character = {
        success_symbol = "[❯](purple)";
        error_symbol = "[❯](red)";
      };
      cmd_duration.format = "[$duration]($style) ";
    };
  };

  # No home.file entries: everything user-specific (git identity.inc,
  # allowed_signers, herdr config, AGENTS.md symlinks) is seeded writable by
  # bootstrap.sh, and ~/.claude is a host mount HM must never touch.
}
