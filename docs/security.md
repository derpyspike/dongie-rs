# Security model

- **Network exposure:** both ports listen **only on the Tailscale IP**. Nothing is reachable
  from the hotspot, USB or LTE side. There is no TLS — the tailnet (WireGuard) encrypts.
- **Accounts:** `/etc/dongie/users`, one `name:role:argon2id-hash` per line
  (`root:dongie`, mode 0640), managed with the CLI. Roles: `admin`, `guest`.
- **Guest view** (`anonymous_guest = true`, the default): everything except SMS, actions and
  sensitive fields. The server filters them; they are never sent to guests.
- **Sessions** live in RAM (7 days), cookie `HttpOnly; SameSite=Strict`.
- **CSRF:** every state-changing request must carry `X-Dongie: 1`.
- **Brute force:** logins are serialized; a failed login waits 2 s.
- **Privileges:** dongie runs as system user `dongie`. Extra rights, nothing more:
  - `deploy/sudoers-dongie` — exact, read-only commands: five `qmicli` queries,
    `wg show all dump` (private keys are dropped before anything leaves dongie) and the
    hotspot DHCP leases. `!pam_session` keeps sudo from writing two log lines per call.
  - `deploy/50-dongie.rules` (polkit) — ModemManager messaging, NetworkManager network
    control + Wi-Fi sharing + system settings + Wi-Fi scan, logind reboot.
  - Tailscale `--operator=dongie` (set by `install.sh`) so it can read Tailscale prefs.
  - Wi-Fi calling: dongie-ims runs as **root** in its own process (GPL-3.0, needs the
    modem). dongie only talks to it over `/run/dongie-ims/api.sock` (`root:dongie`, 0660,
    no TCP). Guests see only "registered / since"; number, IPs and the call log are admin only.
- **Deliberately not offered:** switching the Tailscale **exit node** or **accept-routes**.
  Changing them remotely can cut the stick off from the tailnet; they are shown read-only.
- **Hotspot / USB access** (`web_listen_extra`): only the web UI (8040) is offered there,
  never the open API. With `anonymous_guest = true` everyone on the hotspot sees the guest
  view (signal, data usage, no SMS / numbers / IMEI); set it to `false` if that's too much,
  or limit it with `guest_networks` (e.g. the bridge and the tailnet, not the upstream Wi-Fi
  when the stick is a client). The client address comes from the TCP connection; a client-sent
  `X-Dongie-Peer` header is dropped.
- **Port 8041 has no login.** With `api_allow_sms = true` (default) **anyone on the
  tailnet can read and send SMS** from the SIM (and read the Wi-Fi calling call log). Set it to `false` if that is too much.
- **Known gap:** Wi-Fi passwords are passed to `nmcli` as command-line arguments, so other
  local users on the stick could see them in the process list for a moment. WireGuard
  configs go through a short-lived 0600 temp file instead. Neither is ever logged.

---
[← back to README](../README.md) · [docs index](README.md)
