{
  description = "command-center - multi-agent coordination hub";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            # Rust
            rustToolchain

            # Terminal UI / session management
            pkgs.tmux

            # Data / persistence
            pkgs.sqlite

            # Search & filtering
            pkgs.fzf
            pkgs.ripgrep
            pkgs.jq

            # Git
            pkgs.git

            # Nix
            pkgs.nixpkgs-fmt
          ];

          shellHook = ''
            echo "command-center dev shell loaded"
          '';
        };
      }
    );
}
