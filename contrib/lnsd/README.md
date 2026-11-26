# Reticulum on the stick: Leviculum `lnsd` (RAM only) + LXMF notifications

dongie can send its notifications as LXMF messages (`[[notify]] kind = "lxmf"`).
That needs a Reticulum daemon on the stick; these files run
[Leviculum](https://codeberg.org/Lew_Palm/leviculum)'s `lnsd` (Rust, AGPL-3.0,
a separate program) as a plain client of your transport node, with its storage
in RAM only.

Measured on a UZ801 (aarch64): `lnsd` 5.5-6 MB RSS. For comparison, Python
`rnsd` needs ~46 MB and Python isn't even installed on the stick.

| file | what |
| --- | --- |
| `config.in` → `/etc/lnsd/config` | client only (`enable_transport = No`), compact tables (max 2000 destinations, paths expire after a day), no interface discovery, log level 2, one `TCPClientInterface` to your transport node (set host/port; `interface_enabled = yes` keeps the file valid for Python rnsd too) |
| `run-lnsd.mount.in` | `/run/lnsd`: tmpfs, **12 MB / 4096 inodes max**, owned by `lnsd` (fill in `@UID@`/`@GID@`) |
| `run-lnmsg.mount.in` | `/run/lnmsg`: tmpfs, **2 MB max**, owned by `dongie`: lnmsg's state (dongie's `lnmsg_home`) |
| `lnsd.service` | `lnsd --storage /run/lnsd`, `MemoryHigh=24M`, `MemoryMax=40M`, hardened, journal rate-limited |
| `lnsd-prune.service` + `.timer` | every 5 min: delete ratchet files older than 10 min |

Why the pruning: lnsd keeps every announced ratchet in memory *and* writes it
as a 61-byte file (= one 4 KB tmpfs page). Against a real transport node that
was ~70 new files a minute. Deleting old files loses nothing while lnsd runs;
without it the tmpfs would fill up within ~40 min and lnsd would log a warning
for every ratchet it can't write.

After a reboot `lnsd` starts empty and relearns paths from the network. The
LXMF *sender identity* is the one thing kept on flash: dongie keeps it in
`<state_dir>/lxmf/identity` (directory 0700) and links `/run/lnmsg/identity`
to it.

## Building lnsd and lnmsg for the stick (aarch64, static)

Leviculum's C parts (mimalloc, SQLite in lnmsg) need a C cross compiler; `zig
cc` works and is in the Nix cache (nixpkgs' aarch64-musl gcc is not). cc-rs
passes `--target=aarch64-unknown-linux-musl`, which zig can't parse, so wrap it:

```sh
#!/bin/sh
# zigcc: drop cc-rs' --target=..., give zig its own triple
for a in "$@"; do shift; case "$a" in --target=*) ;; *) set -- "$@" "$a" ;; esac; done
exec zig cc -target aarch64-linux-musl "$@"
```

```sh
# in the leviculum checkout, with a Rust toolchain that has aarch64-unknown-linux-musl
export CC_aarch64_unknown_linux_musl=/path/to/zigcc AR_aarch64_unknown_linux_musl="zig ar"
cargo build --release --locked --target aarch64-unknown-linux-musl -p leviculum-cli \
  --no-default-features --features alloc-mimalloc,compression,serial --bin lnsd   # no BLE
cargo build --release --locked --target aarch64-unknown-linux-musl -p lnmsg --bin lnmsg
```

Stripped: lnsd 6.9 MB, lnmsg 10.1 MB, lnstatus 6.7 MB. The binaries on the first test stick (2026-11-25) were built from Leviculum 395edd42 + the patches below (e80c3d7b). The patches are now rebased onto upstream d476a0ace (2026-11-22): branch **`dongie-master`** (cf7cb559) in [github.com/derpyspike/leviculum](https://github.com/derpyspike/leviculum), next to an unchanged copy of upstream `master`. Build the stick binaries from `dongie-master`. The Reticulum tab shows each tool's version and commit.

## Local patches to Leviculum (`patches/`)

Apply with `git am patches/*.patch` on Leviculum d476a0ace (upstream master, 2026-11-22) before building
`lnmsg`. They change Leviculum code, so they are **AGPL-3.0** like Leviculum,
not EUPL like the rest of this repository.

1. `0001`: ignore bytes after the delivery announce's msgpack array, as Python
   does. A MeshChatX nightly announces 18 extra bytes there; Leviculum rejected
   the whole announce, lost the recipient's stamp cost and sent unstamped
   messages, which the recipient (stamp cost 8) acknowledged and then dropped:
   `lnmsg` said "Delivered", nothing showed up.
2. `0002`: if a delivery announce can't be decoded at all, assume stamp cost 10
   (also satisfies lower requirements) instead of none, so the next format
   change doesn't silently swallow messages again. On the UZ801 a cost-10
   stamp plus the send took 761 ms.

## Install

```sh
sudo useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin lnsd
sudo install -m 0755 lnsd lnmsg /usr/local/bin/
sudo install -d -m 0750 -o root -g lnsd /etc/lnsd
sudo install -m 0640 -o root -g lnsd config /etc/lnsd/config      # from config.in, host/port set
sed "s/@UID@/$(id -u lnsd)/; s/@GID@/$(id -g lnsd)/" run-lnsd.mount.in | sudo tee /etc/systemd/system/run-lnsd.mount
sed "s/@UID@/$(id -u dongie)/; s/@GID@/$(id -g dongie)/" run-lnmsg.mount.in | sudo tee /etc/systemd/system/run-lnmsg.mount
sudo install -m 0644 lnsd.service lnsd-prune.service lnsd-prune.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now run-lnsd.mount run-lnmsg.mount lnsd.service lnsd-prune.timer
```

For dongie's **Reticulum tab** also install `lnstatus` (built like lnsd,
`--bin lnstatus`, 6.7 MB stripped) to `/usr/local/bin`, and give dongie a client
config with the same `rpc_key` as `/etc/lnsd/config`:

```sh
sudo install -d -m 0750 -o root -g dongie /etc/dongie/reticulum
printf '[reticulum]\n  share_instance = Yes\n  rpc_key = %s\n' "$KEY" | sudo tee /etc/dongie/reticulum/config
sudo chown root:dongie /etc/dongie/reticulum/config && sudo chmod 0640 /etc/dongie/reticulum/config
```

dongie's polkit rule (`deploy/50-dongie.rules`) lets it restart `lnsd.service`
and nothing else (not stop, not other units).

Then add an `lxmf` target on dongie's Settings tab (or in config.toml) and press **Send test**.
