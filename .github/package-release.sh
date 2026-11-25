#!/bin/sh
# Release files for tag $1: the binary, complete source with vendored Rust
# dependencies (builds offline), SHA256SUMS. Used by .github/workflows/ci.yml.
set -eu
tag="$1"; bin="$2"
src="dongie-rs-$tag"
work=$(mktemp -d)
mkdir -p dist "$work/$src"
cp "$bin" "dist/dongie-$tag-aarch64-linux-musl"
git archive HEAD | tar -x -C "$work/$src"
cargo vendor -q --locked --manifest-path "$work/$src/Cargo.toml" "$work/$src/vendor" > /dev/null
printf '\n[source.crates-io]\nreplace-with = "vendored-sources"\n\n[source.vendored-sources]\ndirectory = "vendor"\n' \
  >> "$work/$src/.cargo/config.toml"
tar -C "$work" --sort=name --owner=0 --group=0 --numeric-owner -cJf "dist/$src.tar.xz" "$src"
rm -rf "$work"
(cd dist && sha256sum ./* > SHA256SUMS && cat SHA256SUMS)
