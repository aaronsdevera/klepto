{
  description = "Klepto — local-first Rust harness around oh-my-pi (omp)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
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
        pkgs = import nixpkgs { inherit system overlays; };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rustfmt" "clippy" ];
        };
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
        klepto = rustPlatform.buildRustPackage {
          pname = "klepto";
          version = "0.5.8";
          src = ./klepto;
          cargoLock = {
            lockFile = ./klepto/Cargo.lock;
          };
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ openssl ];
          meta = with pkgs.lib; {
            description = "Local-first Rust harness around oh-my-pi (omp)";
            license = licenses.mit;
            mainProgram = "klepto";
          };
        };
      in {
        packages.default = klepto;
        packages.klepto = klepto;

        apps.default = {
          type = "app";
          program = "${klepto}/bin/klepto";
        };
        apps.klepto = self.apps.${system}.default;

        devShells.default = pkgs.mkShell {
          packages = [
            klepto
            rustToolchain
            pkgs.pkg-config
            pkgs.openssl
            pkgs.tmux
            pkgs.ripgrep
            pkgs.cargo-watch
          ];
          shellHook = ''
            echo "Klepto nix shell — run: klepto serve"
            echo "omp is not packaged here; install with: curl -fsSL https://omp.sh/install | sh"
          '';
        };
      });
}
