# Configuration

`/etc/dongie/config.toml` — every key is optional; these are the defaults
(also in `deploy/config.toml`):

| Key | Default | Meaning |
|---|---|---|
| `web_listen` | `127.0.0.1:8040` | web UI + login API (use your stick's Tailscale IP, or `tailscale:8040`). Every listen key takes `ip:port` or `tailscale:PORT`: the stick's Tailscale IPv4 **and** IPv6 address, whatever they are. dongie waits until Tailscale has one and moves the listener within 30 s when it changes (another tailnet) or goes away (logout) |
| `web_listen_extra` | `[]` | more addresses for the **web UI only** (login required as usual), e.g. `["tailscale:8040"]`, or `["192.168.4.1:8040"]` for the hotspot or the USB address, so the stick is reachable without Tailscale. Addresses that don't exist yet (hotspot off) are retried every 5 s. Browser calls still need HTTPS (Tailscale) |
| `api_listen` | `127.0.0.1:8041` | open API (use the Tailscale IP, never `0.0.0.0`) |
| `firewall` | `false` | nftables firewall: zones × services from `<state_dir>/firewall.toml` (or the defaults), edited on the Network tab's Firewall card. See [Firewall](#firewall) and [security](security.md) |
| `api_enabled` | `true` | `false` disables the open API completely |
| `api_allow_sms` | `true` | open API may read/send SMS |
| `anonymous_guest` | `true` | dashboard visible without login |
| `guest_networks` | `[]` | where `anonymous_guest` applies, as networks (CIDR), e.g. `["192.168.100.0/24", "100.64.0.0/10"]` for the bridge/hotspot and the tailnet. Other clients get the login. Empty = everyone who reaches the web UI |
| `update_check` | `true` | look for a new release once a day (GitHub API, ~5 KB); the UI shows a badge, installing is an admin's click |
| `update_repo` | `derpyspike/dongie-rs` | where releases come from |
| `users_file` | `/etc/dongie/users` | accounts |
| `state_dir` | `/var/lib/dongie` | `usage.json` |
| `qmi_device` | `/dev/wwan0qmi0` | must match the sudoers file |
| `hotspot_connection` | `hotspot` | NetworkManager connection of the AP |
| `lte_connection` | `lte` | fallback name of the modem connection; dongie uses the **active** gsm connection when there is one (one connection per SIM) |
| `poll_secs` | `10` | status refresh interval |
| `max_messages` | `500` | SMS kept in RAM |
| `services` | ModemManager, NetworkManager, tailscaled, fix-attach-apn, dongie-leds, ssh, systemd-zram-setup@zram0, dongie-ims | units shown on the System tab (dongie itself is always listed, lnsd too when Reticulum is set up) |
| `ims_socket` | `/run/dongie-ims/api.sock` | dongie-ims API socket (Wi-Fi calling); `""` turns it off. Missing socket = card hidden |
| `lnsd_unit`, `lnstatus`, `reticulum_config` | `lnsd`, `/usr/local/bin/lnstatus`, `/etc/dongie/reticulum` | Reticulum tab (admin): the daemon's unit, Leviculum's lnstatus and a client config dir with the daemon's `rpc_key`; the tab is hidden when lnstatus is missing or `lnsd_unit = ""`. See [contrib/lnsd](../contrib/lnsd/README.md) |
| `muxtastic_url`, `muxtastic_unit`, `muxtastic_link` | `http://127.0.0.1:8090`, `muxtastic`, `""` | Mesh tab: muxtastic's web/API on loopback, its unit (the tab shows when the unit is installed), and the link of the "Open muxtastic" button (`""` = the host the page was opened with, on `muxtastic_url`'s port). See [contrib/muxtastic](../contrib/muxtastic/README.md) |
| `muxtastic_config` | `/etc/muxtastic/muxtastic.toml` | muxtastic's config, edited from the Mesh tab (needs group `dongie` write access, see contrib/muxtastic) |
| `persist` | `false` | keep SMS and the call log in `state_dir` (`sms.bin`, `calls.bin`) across restarts and power cuts; off = RAM only. Contacts (`contacts.bin`) are always saved |
| `ussd` | `false` | USSD card on the Mobile tab (admin only). Off because on LTE the modem falls back to 3G for USSD: data and Tailscale stall for ~20-30 s; on the UZ801 the reply often never comes |

## Settings tab (web UI)

Admins can change most of these keys on the **⚙️ Settings** tab: everything above except `users_file`, `state_dir` and `qmi_device`, plus the notification targets. dongie runs as its own user and can't write this file, so the UI writes `<state_dir>/settings.toml`, which overrides `config.toml` key by key; **↺** next to a setting removes the override again. Saving restarts dongie (systemd starts it again within a few seconds); logins survive that (sessions are kept in `/run/dongie`, RAM only).

- Tokens are write-only in the UI: it only shows "set ✓", never the value.
- Changing a listen address (`web_listen`, `web_listen_extra`, `api_listen`, `api_enabled`) can lock you out, so it is provisional: confirm it from the new address within 2 minutes ("Keep the new addresses"), otherwise the previous `settings.toml` comes back and dongie restarts with it.
- `api_listen` on `0.0.0.0` is refused (the API has no login).
- A broken `settings.toml` is ignored (logged); `config.toml` alone then applies.

## Notifications

Incoming SMS, incoming calls (ringing) and missed calls can be sent to any number of targets. HTTP goes through `curl` (installed on Debian images); the URL, token and body are passed to curl on stdin, never on the command line. Test all targets with `sudo dongie notify test` or the **Send test** button on the System tab.

```toml
[[notify]]
kind = "ntfy"                         # ntfy.sh or self-hosted
url = "https://ntfy.sh/my-secret-topic"
token = ""                            # optional access token
events = ["sms", "missed_call"]       # any of sms, call, missed_call (default: all three)
include_text = true                   # false: only "SMS from +34… (42 characters)"

[[notify]]
kind = "webhook"                      # POST {"event","from","text","via","title","at"} as JSON
url = "https://example.org/hook"
token = "secret"                      # sent as Authorization: Bearer

[[notify]]
kind = "meshtastic"                   # text message over the Meshtastic TCP API
host = "192.168.1.50:4404"            # muxtastic (4404) or a node with Wi-Fi/Ethernet (4403)
channel = 0                           # channel index 0-7
to = ""                               # "" = broadcast on the channel, "!a1b2c3d4" = direct message
include_text = false                  # it's a radio broadcast: think about who can read it

[[notify]]
kind = "lxmf"                         # LXMF over Reticulum, sent with Leviculum's lnmsg
to = "0123456789abcdef0123456789abcdef"  # recipient's LXMF address (Sideband, Columba, MeshChat…)
via = "auto"                          # auto | direct | propagated
pn = ""                               # optional: propagation node your app syncs from (32 hex)
```

LXMF needs a Reticulum daemon on the stick (Leviculum `lnsd`, or Python `rnsd`) and the `lnmsg` binary: keys `lnmsg` (default `/usr/local/bin/lnmsg`), `lnmsg_home` (default `/run/lnmsg`, best a small tmpfs: lnmsg's paths and ratchets stay in RAM) and `lxmf_name` (sender name, default the hostname). The sender identity is kept on flash in `<state_dir>/lxmf/identity` (directory 0700); `lnmsg_home/identity` is a symlink to it, created by dongie.

With `via = "auto"`, lnmsg tries a direct link first and leaves the message at a propagation node when none comes up; the recipient's app only gets it if it syncs from that node, so set `pn` to yours. **Send test** says which happened: "delivered (direct)", "sent …, delivery not confirmed yet", or "left at a propagation node". dongie reads that from lnmsg's event log (`LEVICULUM_EVENT_LOG`, a file in `lnmsg_home`, removed after each send).

Meshtastic messages are cut to 200 bytes. The few protobuf fields needed are encoded by hand from the public wire format; no Meshtastic code is included.

If the Tailscale IP isn't up yet at boot, dongie retries binding every 5 s.

---
[← back to README](../README.md) · [docs index](README.md)

## Firewall

With `firewall = true`, `dongie-firewall.service` loads the rules at boot. Without a
`<state_dir>/firewall.toml` the defaults apply; the Firewall card writes the file, and it
can be edited by hand (then `sudo systemctl restart dongie-firewall`):

```toml
[zones]                     # interfaces per zone, first match wins; "*" at the end
bridge = ["br0", "usb*"]
tailscale = ["tailscale0"]
wireguard = ["wg*", "mainwg"]
wifi = ["wlan*"]
lte = ["wwan*", "rmnet*"]

[allow]                     # ssh web api calls dns ping mesh
bridge = ["ssh", "web", "api", "calls", "dns", "ping", "mesh"]
tailscale = ["ssh", "web", "api", "calls", "ping", "mesh"]
wireguard = ["ssh", "web", "api", "calls", "ping"]
wifi = ["ssh", "ping"]
lte = []

forward = ["bridge"]        # zones whose clients the stick routes (NAT)

[[rule]]                    # own rules: zone, tcp|udp, port or range, optional source
zone = "wifi"
proto = "tcp"
ports = "8000-8100"
from = "192.168.1.0/24"
comment = "home lab"
```

`dongie firewall-show` prints the nftables rules the current file would load.

