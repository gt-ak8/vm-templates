{
  description = "Lima dev sandbox VM - devstation Home Manager config (standalone, Debian 13)";

  inputs = {
    # nixos- channel (NOT -darwin): these outputs target Linux VMs.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    home-manager.url = "github:nix-community/home-manager/release-26.05";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    # worktrunk is kept as an input for its Home Manager module (shell
    # integration), not for its package: the module's `package` option is
    # overridden with a prebuilt release binary, so the source build is never
    # realised. herdr needs no input at all, only the binary.
    # See prebuilt.nix for the pinned versions and hashes.
    worktrunk.url = "github:max-sixty/worktrunk/v0.72.0";
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      home-manager,
      worktrunk,
    }:
    let
      # One home build per system. aarch64-linux is primary (Apple Silicon VMs);
      # x86_64-linux is a secondary entry for Intel hosts / cloud images.
      mkHome =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            config.allowUnfree = true; # codex, pi-coding-agent are unfree
          };
          prebuilt = import ./prebuilt.nix { inherit pkgs; };
        in
        home-manager.lib.homeManagerConfiguration {
          inherit pkgs;
          extraSpecialArgs = {
            herdr-pkg = prebuilt.herdr;
            # The release artifact is stamped by upstream CI, so it reports its
            # own tag: no VERGEN_GIT_DESCRIBE override needed.
            worktrunk-pkg = prebuilt.worktrunk;
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

      # Re-exported so bootstrap.sh can run the home-manager CLI from this
      # flake's lock. Resolving `home-manager/release-26.05` fresh instead
      # calls the GitHub commits API unauthenticated, and the whole build dies
      # with HTTP 403 whenever the host IP is over the anonymous rate limit.
      packages = nixpkgs.lib.genAttrs [ "aarch64-linux" "x86_64-linux" ] (system: {
        home-manager = home-manager.packages.${system}.home-manager;
        default = home-manager.packages.${system}.home-manager;
      });
    };
}
