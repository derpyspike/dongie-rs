# Build & install

## Build

On a NixOS (or any Nix) machine:

```bash
nix develop                    # rust-overlay toolchain + aarch64-musl std, rust-lld linker
cargo test                     # unit tests run on the host
cargo build --release --target aarch64-unknown-linux-musl
# -> target/aarch64-unknown-linux-musl/release/dongie (static, stripped, ~2 MB)
```

The flake pulls the official prebuilt Rust toolchain plus the `aarch64-unknown-linux-musl`
standard library and links with `rust-lld`, so **no C cross-compiler** is needed — which
means **all dependencies must stay pure Rust**.

## Install on the dongle

```bash
mkdir /tmp/dongie-install
cp deploy/* target/aarch64-unknown-linux-musl/release/dongie /tmp/dongie-install/
scp -r /tmp/dongie-install user@mystick:/tmp/
ssh user@mystick 'sudo bash /tmp/dongie-install/install.sh'
```

`install.sh` creates the `dongie` user, installs the binary, the sudoers file (validated
with `visudo -c` first), the polkit rule, the config (only if none exists) and the systemd
unit, sets the Tailscale operator and restarts the service. Running it again upgrades.

Then create your admin account (you type the password, minimum 8 characters):

```bash
sudo dongie user add admin
```

and open `http://<tailscale-ip>:8040`.

**Upgrading just the binary:**
`sudo install -m 0755 dongie /usr/local/bin/dongie && sudo systemctl restart dongie`

## Users

```bash
sudo dongie user add <name>                 # admin
sudo dongie user add <name> --role guest    # dashboard only, like anonymous guests
sudo dongie user passwd <name>
sudo dongie user del <name>
sudo dongie user list
```

The file is re-read on every login, so changes apply without a restart. Existing
sessions of a changed or deleted user end when dongie restarts.


## HTTPS for browser calls

Browsers only give a page the microphone over HTTPS. Tailscale can provide that, reachable
on the tailnet only:

```bash
sudo tailscale serve --bg --https=443 http://<tailscale-ip>:8040   # → https://<host>.<tailnet>.ts.net
sudo tailscale serve status                                         # must say "(tailnet only)"
```

Don't use `tailscale funnel`: that would put dongie on the public internet. The host name
appears in the public Certificate Transparency logs.

---
[← back to README](../README.md) · [docs index](README.md)
