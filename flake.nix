{
  description = "BMC Aggregator — development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs { inherit system; };
    in {
      devShells.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs; [
          pkg-config
          clang
        ];

        buildInputs = with pkgs; [
          ffmpeg.dev
          glibc.dev
          nasm
        ];

        shellHook = ''
          export LIBCLANG_PATH="${pkgs.clang.cc.lib}/lib"
          export LD_LIBRARY_PATH="${pkgs.clang.cc.lib}/lib:${pkgs.llvmPackages.libllvm.lib}/lib"
          export C_INCLUDE_PATH="${pkgs.glibc.dev}/include"
        '';
      };
    });
}
