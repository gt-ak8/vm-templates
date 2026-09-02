{
  pkgs,
  herdr-pkg,
  worktrunk-pkg,
  ...
}:

# Devstation payload: zsh + starship, herdr, agent CLIs.
# Ported from the homelab devstation home.nix (shell, aliases, starship)
# and agentic-vms/home/base.nix (git identity/signing, zshenv Nix/HM
# sourcing). No ssh-agent forwarding here: each sandbox holds its own key.
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
    herdr-pkg
  ];

  # mise is deliberately NOT managed here, for the same reason as claude-code:
  # nixpkgs pins it per release channel (nixos-26.05 is stuck on 2026.5.12) and
  # the store is read-only, so `mise self-update` cannot work and a project's
  # `min_version` starts failing. bootstrap.sh installs it from mise.run into
  # ~/.local/bin instead, where it updates itself.

  # No *global* language toolchains on purpose: no rustup, no nodejs, no bun
  # default. A project declares what it needs and mise fetches it on first use.
  # A global toolchain in the image is one more thing to keep current for a VM
  # that is thrown away anyway.

  # claude-code is deliberately NOT managed here: the nix store is read-only so
  # its self-update cannot work. bootstrap.sh installs it via the official
  # installer into ~/.local/bin.

  home.sessionVariables = {
    EDITOR = "nano";
    # microsandbox secret injection intercepts TLS, so everything must trust
    # the interception CA. Runtimes that ship their own root store (Claude
    # Code's bundled node, any node a project brings in) need to be pointed at
    # the system bundle explicitly.
    SSL_CERT_FILE = "/etc/ssl/certs/ca-certificates.crt";
    NODE_EXTRA_CA_CERTS = "/etc/ssl/certs/ca-certificates.crt";
  };

  programs.home-manager.enable = true;

  # Upstream HM module (flake input): installs wt + zsh integration
  # (`wt switch` must cd the parent shell; the eval'd init replaces
  # `wt config shell install`, which cannot edit the read-only HM zshrc).
  programs.worktrunk = {
    enable = true;
    package = worktrunk-pkg; # prebuilt release binary, see prebuilt.nix
  };

  # ------------------------------------------------------------------ git ----
  programs.git = {
    enable = true;

    # SSH commit signing. Each sandbox generates its own ed25519 key and
    # registers it with GitHub as a signing key only: signing needs no
    # authentication, so an SSO-enforced org has nothing to authorize. Git talks
    # to GitHub over HTTPS with the token instead (see below). Identity +
    # signing key are NOT in this repo: provision.sh writes
    # ~/.config/git/identity.inc (and the allowed_signers file) from the
    # host-side .env. A missing include is silently ignored by git, so the flake
    # builds without it.
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
      # Everything that talks to GitHub goes over HTTPS with GH_TOKEN, which
      # is the one credential an SSO-enforced org can be made to accept without
      # a per-sandbox click: a classic token is authorized once through
      # "Configure SSO", an SSH key would need it per key and no API can do it.
      # The rewrites catch remotes that were written as ssh (`git clone
      # git@...`, a submodule URL, a .git/config copied in) so they take the
      # same path.
      url."https://github.com/".insteadOf = [
        "git@github.com:"
        "ssh://git@github.com/"
      ];
      # `gh` reads GH_TOKEN and hands it to git as the password. In the guest
      # that variable holds the $MSB_GH_TOKEN placeholder, not the token: the
      # sandbox proxy substitutes the real value on the way out to github.com,
      # so the credential is never on the guest filesystem or in its memory.
      credential."https://github.com".helper = "!gh auth git-credential";
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
    envExtra = ''
      # An ssh session has USER and SHELL from sshd. A shell started over the
      # microsandbox agent (wbox's execs, msb exec) arrives with LOGNAME and a
      # minimal environment instead. nix.sh gates its whole body on USER being
      # set, so without this it exports nothing at all there.
      export USER="''${USER:-''${LOGNAME:-$(id -un)}}"

      # SHELL is missing in the same sessions. Programs that spawn "the user's
      # shell" from it fall back to /bin/sh: plain sh, no zsh config, no
      # starship prompt.
      export SHELL="''${SHELL:-/usr/bin/zsh}"

      # bootstrap.sh installs single-user Nix, whose profile script lives under
      # ~/.nix-profile; the daemon path exists only on multi-user installs.
      # Probing the daemon path alone left ~/.nix-profile/bin off PATH
      # entirely, so every home-manager package (herdr, wt, ...) was
      # unreachable, including over `ssh vm <cmd>`.
      for _nix_profile in \
        /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh \
        "$HOME/.nix-profile/etc/profile.d/nix.sh"; do
        if [ -e "$_nix_profile" ]; then
          . "$_nix_profile"
        fi
      done
      unset _nix_profile
      if [ -e "$HOME/.nix-profile/etc/profile.d/hm-session-vars.sh" ]; then
        . "$HOME/.nix-profile/etc/profile.d/hm-session-vars.sh"
      fi

      # Sandbox secrets are injected only into agent-spawned processes, so an
      # ssh session would have no GH_TOKEN and git would prompt for a password.
      # provision.sh writes the placeholders (never the tokens) here.
      if [ -e "$HOME/.config/wbox/env" ]; then
        . "$HOME/.config/wbox/env"
      fi

      # Native-installer bins (claude, opencode, mise) and mise's shims (node
      # and friends, for whatever a project pins) in PATH for non-interactive
      # shells too, so `ssh vm claude ...` and `ssh vm npm test` work. The
      # interactive `mise activate` (initContent) does not run for these.
      export PATH="$PATH:$HOME/.local/bin:$HOME/.opencode/bin:$HOME/.local/share/mise/shims"
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
  # provision.sh, and ~/.claude is a host mount HM must never touch.
}
