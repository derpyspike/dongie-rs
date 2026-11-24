# Development

```
src/main.rs      CLI (serve, user …), startup
src/web.rs       HTTP routes, auth, CSRF, actions, both listeners
src/status.rs    collector: modem, cells, hotspot, Tailscale, system, flash, services, LEDs
src/history.rs   24 h ring buffer, bucketed queries, band histogram
src/sms.rs       ModemManager D-Bus: ingest, de-dupe, send, delivery reports
src/wifi.rs      hotspot/client modes, scan, join, saved networks
src/net.rs       interfaces, routes, DNS, WireGuard, Tailscale peers, connection test
src/usage.rs     data usage from QMI counters
src/users.rs     users file, argon2id, sessions
src/cmd.rs       process + parsing helpers
web/index.html   the whole UI (embedded at build time)
deploy/          systemd unit, sudoers, polkit rule, default config, install.sh
contrib/         helper scripts for the stick
```

Parsers are unit-tested against real output captured on a UZ801 stick (`cargo test`). For UI work,
build, copy the binary, restart the service; the page needs no separate build step.

---
[← back to README](../README.md) · [docs index](README.md)
