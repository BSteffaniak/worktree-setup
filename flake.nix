{
  description = "worktree-setup - CLI tool for setting up git worktrees";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "rustfmt"
          ];
        };

        # Build dependencies
        buildInputs = with pkgs; [
          rustToolchain
          pkg-config
          libiconv
        ];

      in
      {
        devShells.default = pkgs.mkShell {
          inherit buildInputs;

          packages = with pkgs; [
            fish
          ];

          shellHook = ''
            echo "worktree-setup Development Environment"
            echo "Rust: $(rustc --version)"
            echo ""
            echo "Build with: cargo build --release"
            echo "Install with: cargo install --path packages/cli"

            # Only exec fish if we're in an interactive shell (not running a command)
            if [ -z "$IN_NIX_SHELL_FISH" ] && [ -z "$BASH_EXECUTION_STRING" ]; then
              case "$-" in
                *i*) export IN_NIX_SHELL_FISH=1; exec fish ;;
              esac
            fi
          '';
        };
      }
    );
}
