# Configuration

`/etc/dongie/config.toml` — every key is optional; these are the defaults
(also in `deploy/config.toml`):

| Key | Default | Meaning |
|---|---|---|
| `web_listen` | `127.0.0.1:8040` | web UI + login API (use your stick's Tailscale IP) |
| `api_listen` | `127.0.0.1:8041` | open API (use the Tailscale IP, never `0.0.0.0`) |
| `api_enabled` | `true` | `false` disables the open API completely |
| `api_allow_sms` | `true` | open API may read/send SMS |
| `anonymous_guest` | `true` | dashboard visible without login |
| `users_file` | `/etc/dongie/users` | accounts |
| `state_dir` | `/var/lib/dongie` | `usage.json` |
| `qmi_device` | `/dev/wwan0qmi0` | must match the sudoers file |
| `hotspot_connection` | `hotspot` | NetworkManager connection of the AP |
| `lte_connection` | `lte` | NetworkManager connection of the modem |
| `poll_secs` | `10` | status refresh interval |
| `max_messages` | `500` | SMS kept in RAM |
| `services` | ModemManager, NetworkManager, tailscaled, fix-attach-apn, dongie-leds, ssh, systemd-zram-setup@zram0, dongie-ims | units shown on the System tab |
| `ims_socket` | `/run/dongie-ims/api.sock` | dongie-ims API socket (Wi-Fi calling); `""` turns it off. Missing socket = card hidden |

If the Tailscale IP isn't up yet at boot, dongie retries binding every 5 s.

---
[← back to README](../README.md) · [docs index](README.md)
