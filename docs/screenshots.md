# Screenshots

Taken on a real stick in **guest view** and with **`?redact`**: opening
`http://<stick>:8040/?redact` masks IP addresses (except `192.168.x` and Tailscale's fixed
DNS addresses), shows the host name / MagicDNS name / SSID as `mystick` / `MyStick` and
replaces the tailnet peer counts with placeholders. It only changes what
the page displays; the server is unaffected. The footer shows "🙈 redacted" in that mode.
Admin-only data (IMEI, IMSI, ICCID, phone numbers, cell IDs, SMS, call log, hotspot clients)
never reaches a guest in the first place.

Light = Catppuccin Latte, dark = Catppuccin Mocha (follows the system theme, 🌗 toggles).

## Home

| Latte | Mocha |
|---|---|
| ![Home, light](screenshots/home-light.png) | ![Home, dark](screenshots/home-dark.png) |

On a phone (390 px wide):

<img src="screenshots/phone-dark.png" alt="Home on a phone, dark" width="300">

## Mobile

Connection, Wi-Fi calling (dongie-ims), cells, signal history, bands, everything ModemManager
reports. The history charts were almost empty when these were taken, because dongie had just
been restarted and keeps its history in RAM only.

| Latte | Mocha |
|---|---|
| ![Mobile, light](screenshots/mobile-light.png) | ![Mobile, dark](screenshots/mobile-dark.png) |

## Network

| Latte | Mocha |
|---|---|
| ![Network, light](screenshots/network-light.png) | ![Network, dark](screenshots/network-dark.png) |

## System

⚠ **The flash write rate shown here (≈ 7–10 MiB/min, "≈ 9–15 GB/day") is not normal
operation.** The 1 h window includes several binary deploys and SSH sessions from the
development session. Measured at idle right afterwards from `/proc/diskstats`
(45 s settle, 120 s window): **16 KiB in 120 s ≈ 8 KiB/min ≈ 11.5 MB/day**.

| Latte | Mocha |
|---|---|
| ![System, light](screenshots/system-light.png) | ![System, dark](screenshots/system-dark.png) |

---
[← back to README](../README.md) · [docs index](README.md)
