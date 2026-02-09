{
  description = "Othala orchestrator development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;
        allowUnfree = pkgs.config.allowUnfree or false;
        graphitePkg =
          if !allowUnfree then
            null
          else if builtins.hasAttr "graphite-cli" pkgs then
            pkgs.graphite-cli
          else if
            builtins.hasAttr "nodePackages" pkgs && builtins.hasAttr "graphite-cli" pkgs.nodePackages
          then
            pkgs.nodePackages.graphite-cli
          else
            null;
        othalaPackage = pkgs.rustPlatform.buildRustPackage {
          pname = "othala";
          version = "0.1.0";
          src = lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [
            "-p"
            "orchd"
            "--bin"
            "othala"
          ];
          doCheck = false;
          meta = {
            mainProgram = "othala";
          };
        };
      in
      {
        formatter = pkgs.nixfmt;

        packages = {
          othala = othalaPackage;
          default = othalaPackage;
        };

        apps = {
          othala = flake-utils.lib.mkApp { drv = othalaPackage; };
          default = flake-utils.lib.mkApp { drv = othalaPackage; };
        };

        devShells.default = pkgs.mkShell {
          packages =
            with pkgs;
            [
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
              git
              gh
              jq
              ripgrep
              fd
              tree
              just
              process-compose
              sqlite
              sops
              age
              cargo-nextest
              watchexec
              pkg-config
              openssl
              nil
              nixfmt
            ]
            ++ lib.optionals stdenv.isLinux [
              inotify-tools
            ]
            ++ lib.optionals stdenv.isDarwin [
              libiconv
            ]
            ++ lib.optionals (graphitePkg != null) [
              graphitePkg
            ];

          env = {
            RUST_BACKTRACE = "1";
            SQLX_OFFLINE = "true";
          };

          shellHook = ''
            echo "Othala dev shell ready (${system})"
            echo "Rust: $(rustc --version)"
            echo "Cargo: $(cargo --version)"
            echo "Process Compose: $(process-compose version | sed -n '2p')"
            if command -v gt >/dev/null 2>&1; then
              echo "Graphite (gt): available"
            else
              echo "Graphite (gt): not found in this shell (install manually or use NIXPKGS_ALLOW_UNFREE=1 nix develop --impure)"
            fi
            for cli in claude codex gemini; do
              if command -v "$cli" >/dev/null 2>&1; then
                echo "$cli: available"
              else
                echo "$cli: not found"
              fi
            done
          '';
        };
      }
    );
}
