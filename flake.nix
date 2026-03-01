{
  description = "command-center - multi-agent coordination hub";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    claude-code = {
      url = "github:sadjow/claude-code-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane, claude-code }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [
          (import rust-overlay)
          claude-code.overlays.default
        ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config.allowUnfreePredicate = pkg: builtins.elem (nixpkgs.lib.getName pkg) [
            "claude-code"
          ];
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "rustfmt" "clippy" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = [ pkgs.pkg-config ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv
              pkgs.darwin.cctools
            ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      in
      {
        checks = {
          fmt = craneLib.cargoFmt { inherit src; };

          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          nextest = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            cargoNextestExtraArgs = "--no-tests=pass";
          });
        };

        packages.default = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });

        apps.e2e = let
          clat = self.packages.${system}.default;
          hooks = ./.claude/hooks;
          tests = ./tests/e2e;
          e2e = pkgs.writeShellScriptBin "e2e-tests" ''
            export PATH="${pkgs.lib.makeBinPath [
              clat pkgs.bats pkgs.jq pkgs.tmux pkgs.git pkgs.sqlite
            ]}:$PATH"
            export HOOK_DIR="${hooks}"
            exec bats "${tests}"
          '';
        in {
          type = "app";
          program = "${e2e}/bin/e2e-tests";
        };

        devShells.default = pkgs.mkShell {
          buildInputs = [
            # Rust
            rustToolchain
            pkgs.cargo-nextest

            # Build deps
            pkgs.pkg-config

            # AI agent
            pkgs.claude-code

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
          ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
            pkgs.darwin.cctools
          ];

          shellHook = ''
            export PATH="$PWD/bin:$PATH"
            echo "command-center dev shell loaded"
          '';
        };
      }
    );
}
