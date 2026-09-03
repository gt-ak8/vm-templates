{
  description = "Lima dev sandbox VM - devstation Home Manager config (standalone, Debian 13)";

  inputs = {
    # nixos- channel (NOT -darwin): these outputs target Linux VMs.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    # Unstable, for the few packages the stable channel freezes too far back.
    # Only what is wired through extraSpecialArgs below comes from here; the
    # rest of the closure stays on stable.
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    home-manager.url = "github:nix-community/home-manager/release-26.05";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    # herdr: agent multiplexer; upstream flake builds from source.
    # Must match the host client version exactly: `herdr --remote` refuses a mismatch.
    herdr.url = "github:ogulcancelik/herdr/v0.8.2";
    # worktrunk: upstream flake pinned to a release tag (nixpkgs lags releases).
    # Ships its own HM module (package + shell integration); builds from source.
    worktrunk.url = "github:max-sixty/worktrunk/v0.72.0";
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      nixpkgs-unstable,
      home-manager,
      herdr,
      worktrunk,
    }:
    let
      # One home build per system. aarch64-linux is primary (Apple Silicon VMs);
      # x86_64-linux is a secondary entry for Intel hosts / cloud images.
      mkHome =
        system:
        home-manager.lib.homeManagerConfiguration {
          pkgs = import nixpkgs {
            inherit system;
            config.allowUnfree = true; # codex, pi-coding-agent are unfree
          };
          extraSpecialArgs = {
            herdr-pkg = herdr.packages.${system}.default;
            # mise from unstable: projects gate on a min mise version
            # (`min_version` in mise.toml) and stable 26.05 is frozen at
            # 2026.5.12, too old for repos asking 2026.8.x.
            mise-pkg =
              (import nixpkgs-unstable {
                inherit system;
              }).mise;
            # Flake inputs carry no .git, so upstream's build would stamp the
            # bare commit sha into `wt --version`. Stamp the tag instead.
            # Keep in sync with the worktrunk input tag above.
            worktrunk-pkg = worktrunk.packages.${system}.default.overrideAttrs {
              VERGEN_GIT_DESCRIBE = "v0.72.0";
            };
          };
          modules = [
            worktrunk.homeModules.default
            ./home.nix
          ];
        };
    in
    {
      homeConfigurations = {
        devstation = mkHome "aarch64-linux";
        "devstation-x86_64" = mkHome "x86_64-linux";
      };
    };
}
