# Helper scripts (`contrib/`)

Small scripts that run on the stick next to dongie (dongie shows their service state):

| File | What it does |
|---|---|
| `lte-status` | `lte-status [-w] [-p]` — one-screen modem summary in the terminal (colours, live mode, ping) |
| `fix-attach-apn` + `.service` | at boot, rewrites the modem's LTE **initial-attach APN** (QMI profile 3). The UZ801 firmware re-seeds it with `mobile.three.com.hk` on every modem boot, which makes the network refuse LTE registration. Set `APN=` for your operator. |
| `dongie-leds` + `.service` | status LEDs: blue = LTE (on / blinking while searching / off), green = Tailscale running, red untouched (power) |

Install: `/usr/local/bin/lte-status`, `/usr/local/sbin/{fix-attach-apn,dongie-leds}`,
units into `/etc/systemd/system/`, then `systemctl enable --now <unit>`.

---
[← back to README](../README.md) · [docs index](README.md)
