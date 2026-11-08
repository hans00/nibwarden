{
  # nibwarden for Nix (phase2.md P2-M6): `nix build`, `nix run . -- status`.
  #
  # On NixOS, the systemd user unit comes with `systemd.packages`:
  #   environment.systemPackages = [ nibwarden ];
  #   systemd.packages = [ nibwarden ];
  description = "Bitwarden-compatible password manager for the Linux desktop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forSystems (pkgs: {
        default = pkgs.callPackage ./packaging/nix/package.nix { };
      });

      apps = forSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.default}/bin/nibwarden";
        };
      });
    };
}
