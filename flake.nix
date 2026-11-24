{
  description = "dongie-rs — web dashboard + API for OpenStick MSM8916 4G dongles";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; };
      # Official prebuilt toolchain + std for the dongle target; linked with rust-lld,
      # so no C cross-compiler is needed (all crates must stay pure Rust).
      rust = pkgs.rust-bin.stable.latest.default.override {
        targets = [ "aarch64-unknown-linux-musl" ];
        extensions = [ "rust-src" "clippy" "rustfmt" ];
      };
    in {
      devShells.${system}.default = pkgs.mkShell {
        packages = [ rust pkgs.file ];
      };
    };
}
