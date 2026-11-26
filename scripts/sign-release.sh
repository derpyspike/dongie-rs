#!/bin/sh
# SPDX-License-Identifier: EUPL-1.2
# Sign a release's SHA256SUMS so sticks can install it with the Update button.
#   scripts/sign-release.sh v0.2.2
# Downloads SHA256SUMS of that GitHub release, shows it, signs it with the
# release key, checks the signature the way the stick will (gpgv + the key in
# deploy/release-key.gpg) and leaves SHA256SUMS.asc in the current directory:
# upload it to the release (GitHub: Edit release -> attach files).
set -eu
tag="$1"
repo="${REPO:-derpyspike/dongie-rs}"
key=6882C1DBD9D70697E99A27A0B1FEEB2BF58465DC
here=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -sSfL --proto =https -o "$work/SHA256SUMS" "https://github.com/$repo/releases/download/$tag/SHA256SUMS"
echo "SHA256SUMS of $repo $tag:"
cat "$work/SHA256SUMS"
grep -q " \./dongie-$tag-aarch64-linux-musl\$" "$work/SHA256SUMS" || { echo "no dongie-$tag-aarch64-linux-musl in it"; exit 1; }
gpg --local-user "$key" --armor --detach-sign --output "$work/SHA256SUMS.asc" "$work/SHA256SUMS"
mkdir -m 700 "$work/gnupg"
gpgv --homedir "$work/gnupg" --keyring "$here/deploy/release-key.gpg" "$work/SHA256SUMS.asc" "$work/SHA256SUMS"
cp "$work/SHA256SUMS.asc" ./SHA256SUMS.asc
echo "OK: ./SHA256SUMS.asc — attach it to https://github.com/$repo/releases/tag/$tag"
