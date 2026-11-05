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
          just
          cargo
          rustc
          rustfmt
          clippy
          ripgrep
          python3
          pkg-config
          clang
          nodejs_22
          pnpm
        ];

        buildInputs = with pkgs; [
          openssl
          ffmpeg.dev
          glibc.dev
          nasm

          # GStreamer — replaces FFmpeg for streaming encode + webrtcbin transport
          gst_all_1.gstreamer
          gst_all_1.gst-plugins-base
          gst_all_1.gst-plugins-good
          gst_all_1.gst-plugins-bad
          gst_all_1.gst-plugins-ugly
          gst_all_1.gst-plugins-rs
          libnice
        ];

        shellHook = ''
          export LIBCLANG_PATH="${pkgs.clang.cc.lib}/lib"
          export LD_LIBRARY_PATH="${pkgs.clang.cc.lib}/lib:${pkgs.llvmPackages.libllvm.lib}/lib"
          export C_INCLUDE_PATH="${pkgs.glibc.dev}/include"
          # GStreamer: ensure libnice plugin is discoverable
          export GST_PLUGIN_SYSTEM_PATH="${pkgs.gst_all_1.gstreamer}/lib/gstreamer-1.0:${pkgs.gst_all_1.gst-plugins-base}/lib/gstreamer-1.0:${pkgs.gst_all_1.gst-plugins-good}/lib/gstreamer-1.0:${pkgs.gst_all_1.gst-plugins-bad}/lib/gstreamer-1.0:${pkgs.gst_all_1.gst-plugins-ugly}/lib/gstreamer-1.0:${pkgs.gst_all_1.gst-plugins-rs}/lib/gstreamer-1.0:${pkgs.libnice}/lib/gstreamer-1.0"
        '';
      };
    });
}
