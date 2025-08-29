{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    naersk.url = "github:nix-community/naersk";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, naersk, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = import nixpkgs { inherit system; };
      naerskLib = pkgs.callPackage naersk {};
      rustSrc = pkgs.rust.packages.stable.rustPlatform.rustLibSrc;

      ambiway = naerskLib.buildPackage {
        src = ./.;
        buildInputs = with pkgs; [
          opencv xorg.libX11 xorg.libXrandr
        ];
        nativeBuildInputs = [ pkgs.pkg-config pkgs.clang ];
        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
      };
    in {
      packages.default = ambiway;

      defaultPackage = ambiway;

      devShells.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          fish
          cargo rustc rustfmt clippy rust-analyzer
          opencv xorg.libX11 xorg.libXrandr
        ];
        nativeBuildInputs = [ pkgs.pkg-config pkgs.clang ];

        shellHook = ''
          if [ -z "$FISH_VERSION" ] && [ -z "$NO_AUTO_FISH" ]; then
            exec ${pkgs.fish}/bin/fish
          fi
        '';

        env.RUST_SRC_PATH = "${rustSrc}";
        env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
      };
    });
}
