# Prebuilt release binaries for herdr and worktrunk.
#
# Both upstream flakes build from source through crane, which pulls a Rust
# toolchain into the closure and dominates a cold `home-manager switch`. Both
# projects publish statically linked musl binaries for linux-aarch64 and
# linux-x86_64, so a release artifact needs no interpreter patching and behaves
# exactly like the source build.
#
# The versions here must stay in sync with the flake: herdr's must match the
# host client exactly (`herdr --remote` refuses a mismatch).
{ pkgs }:

let
  inherit (pkgs) lib;
  inherit (pkgs.stdenv.hostPlatform) system;

  herdrVersion = "0.8.2";
  worktrunkVersion = "0.72.0";

  # Upstream names its artifacts by bare CPU, not by Nix system double.
  herdrArtifacts = {
    aarch64-linux = {
      name = "herdr-linux-aarch64";
      hash = "sha256-9VYQZY4cLg0qrvcwtLKriF9/i6AChas3K/sU8uPVtA0=";
    };
    x86_64-linux = {
      name = "herdr-linux-x86_64";
      hash = "sha256-l2FQoU1JDJSyQ+ouGn6y37Z/EuNrGC25CTb2co5q7PQ=";
    };
  };

  worktrunkArtifacts = {
    aarch64-linux = {
      target = "aarch64-unknown-linux-musl";
      hash = "sha256-L2tF/QWS5LD2bKPDTLr5DHZDp+qr+KnEsOEtSCUaCGw=";
    };
    x86_64-linux = {
      target = "x86_64-unknown-linux-musl";
      hash = "sha256-6RvHzrBiOUKnlzF/VlQagl1qNuJNBVmFqCmdMDRb40Y=";
    };
  };

  pick = what: set: set.${system} or (throw "${what} has no prebuilt linux artifact for ${system}");

  herdrArtifact = pick "herdr" herdrArtifacts;
  worktrunkArtifact = pick "worktrunk" worktrunkArtifacts;
in
{
  herdr = pkgs.stdenvNoCC.mkDerivation {
    pname = "herdr";
    version = herdrVersion;

    # A bare ELF, not an archive.
    src = pkgs.fetchurl {
      url = "https://github.com/ogulcancelik/herdr/releases/download/v${herdrVersion}/${herdrArtifact.name}";
      inherit (herdrArtifact) hash;
    };
    dontUnpack = true;

    installPhase = ''
      runHook preInstall
      install -Dm755 $src $out/bin/herdr
      runHook postInstall
    '';

    meta = {
      description = "Agent multiplexer";
      homepage = "https://github.com/ogulcancelik/herdr";
      mainProgram = "herdr";
      platforms = lib.attrNames herdrArtifacts;
    };
  };

  worktrunk = pkgs.stdenvNoCC.mkDerivation {
    pname = "worktrunk";
    version = worktrunkVersion;

    src = pkgs.fetchurl {
      url = "https://github.com/max-sixty/worktrunk/releases/download/v${worktrunkVersion}/worktrunk-${worktrunkArtifact.target}.tar.xz";
      inherit (worktrunkArtifact) hash;
    };

    # The tarball also carries git-wt, a README, a CHANGELOG and a LICENSE.
    # Upstream's `packages.default` is `wt` alone, so install only that.
    installPhase = ''
      runHook preInstall
      install -Dm755 wt $out/bin/wt
      runHook postInstall
    '';

    meta = {
      description = "A CLI for Git worktree management, designed for parallel AI agent workflows";
      homepage = "https://github.com/max-sixty/worktrunk";
      license = with lib.licenses; [
        mit
        asl20
      ];
      mainProgram = "wt";
      platforms = lib.attrNames worktrunkArtifacts;
    };
  };
}
