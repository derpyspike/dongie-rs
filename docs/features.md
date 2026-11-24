# Features

The UI has five tabs, in Catppuccin Latte/Mocha (follows the system theme; 🌗 toggles
auto/light/dark). Without logging in you get a **guest view**; sensitive data and all
actions need an **admin** login.

| Tab | What you get | Admin only |
|---|---|---|
| **Home** | compact cards: mobile (tech, signal tiles, band, IP), data, Wi-Fi, Tailscale, system (load, temp, RAM/zram/disk %), unread SMS | message preview |
| **Mobile** | **Wi-Fi calling** card when **dongie-ims** (separate GPL-3.0 service) runs (registered since, re-registrations) · connection (operator, SIM, registration, roaming, CS/PS attach, APN, modes) · neighbour cells table (PCI, band, RSRP/RSRQ) and watched frequencies · signal history charts (RSRP, RSRQ/SNR, 3G RSSI/Ec-Io; 1 h / 6 h / 24 h) · band-over-time strip · time per band + switch count · **everything ModemManager reports** about modem, SIM and data bearer | IMEI, IMSI, ICCID, own number, device ID, global cell ID / TAC / PLMN, timing advance (≈ distance to the tower); LTE reconnect; IMS number, inner IP, P-CSCF, last error, **call log** (answered / missed / rejected, offered codecs) with reject for ringing calls |
| **Network** | Wi-Fi (hotspot or client mode) · Tailscale (IP, peers online, health) · WireGuard tunnels (state, public key, handshake, traffic) · interfaces · routes & DNS · data usage (today / month / session, 31-day chart) | hotspot settings (SSID, password, channel), scan & join networks, saved networks, WireGuard import/start/stop/autostart/delete, connection test, Tailscale peer list, hotspot client details |
| **System** | uptime, load, CPU clock, RAM, zram, disk · temperature tiles + history · RAM history · **flash write rate** (since boot, ext4 lifetime, per-day estimate, chart) · services · LEDs | reboot |
| **Messages** | — | SMS conversations, send (with delivery reports ✓ / ✓✓ / ✗), unread badges, GSM/Unicode character counter |

**Calls** (admin, needs dongie-ims + HTTPS): a call bar at the bottom of every tab rings for
incoming calls (Answer / Reject); in a call it shows timer, codec, mute, a DTMF keypad and
hang up. The dialer is in the Wi-Fi Calling card; outgoing calls are charged to the SIM.
Audio runs browser ⇄ WebRTC (G.722) ⇄ dongie-ims ⇄ AMR-WB/AMR ⇄ network. Browsers only allow
the microphone over HTTPS, so open dongie via `tailscale serve` (see [install](install.md)).

**SMS** are kept **in RAM only** (last 500). Incoming messages are copied into RAM and
then deleted from the SIM/modem, so the SIM never fills up. A restart clears them.
While dongie-ims is registered, the network may deliver SMS over IMS instead of through
the modem. dongie pulls those from dongie-ims and marks them "via Wi-Fi calling".

**Screenshot mode:** append `?redact` to the URL (`http://<stick>:8040/?redact#home`) to
mask IP addresses and tailnet peer counts in the page before taking screenshots
([examples](screenshots.md)). It is display-only.

**Data usage** comes from the modem's own QMI counters, because the kernel's `wwan0`
counters stay at 0 on this hardware. Daily totals are kept in `/var/lib/dongie/usage.json`.

**History** (signal, band, temperatures, RAM, flash writes) is a 24 h ring buffer in RAM,
one sample per poll (10 s), and is gone after a restart.

---
[← back to README](../README.md) · [docs index](README.md)
