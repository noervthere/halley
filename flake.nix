{
  description = "Halley: a spatial Wayland compositor built around infinite workspace navigation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    home-manager,
  }: let
    linuxSystems = nixpkgs.lib.filter (nixpkgs.lib.hasSuffix "-linux") flake-utils.lib.defaultSystems;
  in
    flake-utils.lib.eachSystem linuxSystems (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [self.overlays.default];
      };
    in {
      packages = {
        inherit (pkgs) halley;
        default = pkgs.halley;
      };

      apps = {
        default = {
          type = "app";
          program = "${pkgs.halley}/bin/halley";
        };
        halley = {
          type = "app";
          program = "${pkgs.halley}/bin/halley";
        };
        halleyctl = {
          type = "app";
          program = "${pkgs.halley}/bin/halleyctl";
        };
        halley-lift = {
          type = "app";
          program = "${pkgs.halley}/bin/halley-lift";
        };
      };

      devShells.default = pkgs.mkShell {
        name = "halley-dev";
        inputsFrom = [pkgs.halley];
        nativeBuildInputs = with pkgs; [
          cargo
          rustc
          clippy
          rustfmt
          rust-analyzer
          pkg-config
          rustPlatform.bindgenHook
        ];
        buildInputs = pkgs.halley.buildInputs;
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (
          pkgs.halley.buildInputs
          ++ [
            pkgs.libglvnd
            pkgs.wayland
            pkgs.libxkbcommon
            pkgs.libgbm
            pkgs.libdrm
            pkgs.pipewire
          ]
        );
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    })
    // {
      overlays.default = final: prev: {
        halley = final.callPackage ./nix/halley.nix {};
      };
      overlays.halley = self.overlays.default;

      nixosModules.default = { ... }: {
        imports = [ ./nix/nixos-module.nix ];
        nixpkgs.overlays = [ self.overlays.default ];
      };
      nixosModules.halley = self.nixosModules.default;

      homeManagerModules.default = { ... }: {
        imports = [ ./nix/home-manager-module.nix ];
        nixpkgs.overlays = [ self.overlays.default ];
      };
      homeManagerModules.halley = self.homeManagerModules.default;
    };
}
