# How it works

```
                 ┌──────────── dongie (user "dongie") ─────────────┐
 browser ──8040──▶ web UI + login API ─┐                           │
 scripts ──8041──▶ open API ───────────┤  status collector (10 s) ─┼─▶ mmcli, nmcli, iw, ip,
                 │                     │  history ring (24 h RAM)  │    tailscale, systemctl,
                 │                     │  data usage (QMI counters)│    sudo qmicli (read-only),
                 │                     └─ SMS store (RAM) ◀────────┼─▶ ModemManager over D-Bus
                 └─────────────────────────────────────────────────┘
```

- **Status** is gathered by the tools already on the stick (`mmcli -K`, `qmicli`,
  `nmcli -t`, `iw`, `ip -j`, `tailscale status --json`, `/proc`, `/sys`).
- **SMS** go through ModemManager's D-Bus API (`zbus`): new messages are picked up,
  de-duplicated (the modem often stores one SMS on the SIM *and* in its memory),
  delivery reports are matched by message reference.
- **Wi-Fi / WireGuard / hotspot** changes go through NetworkManager (`nmcli`), allowed by a
  polkit rule. WireGuard uses NetworkManager's importer, **not wg-quick** — wg-quick's
  `PostUp` lines would run arbitrary commands as root.
- The web page is a single file (`web/index.html`) embedded into the binary; no external
  scripts, fonts or CDNs.

---
[← back to README](../README.md) · [docs index](README.md)
