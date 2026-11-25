# muxtastic on the stick

[muxtastic](https://github.com/derpyspike/muxtastic) (AGPL-3.0-or-later, a
separate program) shares one Meshtastic node between several apps and keeps a
searchable history. On an OpenStick it talks to a node **with Wi-Fi that joins
the stick's hotspot**; the stick's own USB port is the one plugged into the
computer, so a USB-serial node isn't an option.

dongie shows it on the **📻 Mesh** tab and a Home card (behind dongie's login;
guests see no messages, positions or addresses) and lists the unit in Services.

## Files

- `muxtastic.service` — `DynamicUser`, read-only system, `MemoryMax=64M`,
  logs to the journal, nothing written to flash.
- `muxtastic.toml.in` — config template: replace `@NODE_IP@` (the node's IP on
  the hotspot) and `@HOST@`. History is `storage = "memory"` with
  `retention_days = 1`; the MQTT bridge is off (every byte goes over LTE).

muxtastic listens on all interfaces: apps connect to the stick's address on
the hotspot, USB or Tailscale, port **4404**; the web UI (no login of its own)
is on port **8090**. While it runs, `muxtastic.service` drops both ports when
they arrive on the LTE side (`wwan*`, iptables/ip6tables rules added at start
and removed at stop), so they're never open to the carrier's network. Change
the ports in both files if you change them. Optional: `tailscale serve` can
put HTTPS in front of the web UI, but nothing needs it.

## Build (aarch64 musl, from Dash)

muxtastic has C dependencies (SQLite, aws-lc for MQTT TLS). With zig as the C
cross compiler and rust-lld as linker:

```bash
cat > zigcc/cc <<'SH'
#!/bin/sh
for a in "$@"; do shift; case "$a" in --target=*) ;; *) set -- "$@" "$a" ;; esac; done
exec zig cc -target aarch64-linux-musl -fno-sanitize=undefined "$@"
SH
# (zigcc/ar: exec zig ar "$@")
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static" \
CC_aarch64_unknown_linux_musl=$PWD/zigcc/cc AR_aarch64_unknown_linux_musl=$PWD/zigcc/ar \
cargo build --release --locked --target aarch64-unknown-linux-musl -p muxtastic
```

`-fno-sanitize=undefined`: zig turns on UBSan for unoptimised C, and a Rust
link has no UBSan runtime (`undefined symbol: __ubsan_handle_…`).

## Install

```bash
sudo install -m 0755 muxtastic /usr/local/bin/
sudo install -D -m 0644 muxtastic.toml /etc/muxtastic/muxtastic.toml
sudo install -m 0644 muxtastic.service /etc/systemd/system/
# let dongie's Mesh tab edit the settings (node, admin allowlist, history)
sudo chgrp dongie /etc/muxtastic /etc/muxtastic/muxtastic.toml
sudo chmod 0775 /etc/muxtastic && sudo chmod 0664 /etc/muxtastic/muxtastic.toml
sudo systemctl daemon-reload && sudo systemctl enable --now muxtastic
```

dongie's polkit rule (`deploy/50-dongie.rules`) lets it start, stop and
restart `muxtastic.service`, nothing else (no enable/disable).

## Settings from dongie

The Mesh tab (admin) edits node address and port, `allow_admin_from` (with
quick buttons for the tailnet and the hotspot network), and how many days of
history stay in RAM. dongie edits the file in place with `toml_edit`, so
comments and all other keys stay; the previous version is kept as
`muxtastic.toml.bak`; muxtastic is restarted if it runs. The ports aren't
editable there because `muxtastic.service` names them in its LTE rule.

Measured on a UZ801 (no node yet): ~2 MB RSS.
