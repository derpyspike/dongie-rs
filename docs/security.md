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
  - `deploy/50-dongie.rules` (polkit) — ModemManager messaging + device control (SIM PIN), NetworkManager network
    control + Wi-Fi sharing + system settings + Wi-Fi scan, logind reboot.
  - Tailscale `--operator=dongie` (set by `install.sh`) so it can read Tailscale prefs and
    switch `tailscale serve` (HTTPS on the tailnet) from the Network tab. dongie only ever
    runs `tailscale serve --https=443 …` / `… off`; it never runs `tailscale funnel`, and
    shows a warning if Funnel is on.
  - VoWiFi switch: polkit start/stop for `dongie-ims.service` only. Off writes
    `<state_dir>/vowifi-off`, and the drop-in `deploy/dongie-ims.service.d/dongie.conf`
    (`ConditionPathExists=!/var/lib/dongie/vowifi-off`) keeps systemd from starting it.
  - Wi-Fi calling: dongie-ims runs as **root** in its own process (GPL-3.0, needs the
    modem). dongie only talks to it over `/run/dongie-ims/api.sock` (`root:dongie`, 0660,
    no TCP). Guests see only "registered / since"; number, IPs and the call log are admin only.
- **Updates:** dongie (unprivileged) downloads a release only when an admin clicks Update,
  then starts `dongie-update.service` (polkit: exactly that unit, verb start). That root helper
  copies the staged files into `/run/dongie-update` (root only), and installs only if
  `SHA256SUMS.asc` is a valid signature by the release key built into the installed dongie
  (`gpgv`), the binary matches `SHA256SUMS`, it says the expected version and that version is
  higher than the installed one (no downgrade to an old signed release). GitHub holds no
  signing key. It keeps `dongie.bak` and restores it if the new version doesn't answer within
  60 s.
- **SIM PIN:** an admin can unlock a PIN- or PUK-locked SIM on the Mobile tab. With
  "Remember the PIN" ticked, the PIN is stored **in plain text** in `<state_dir>/sim-pin.json`
  (mode 0600, user `dongie`), keyed by the SIM's ICCID, so it is only ever sent to that SIM.
  Anyone with root on the stick (or its flash) can read it; it never leaves the stick through
  the API and is never logged. The automatic unlock only tries while 2+ PIN attempts are left
  and deletes a PIN the SIM rejects, so a stale saved PIN costs at most one attempt. When
  only one attempt is left the UI asks again before sending.
- **Firewall** (`firewall = true`, off by default; on in OpenStick-Builder images): one
  nftables table `inet dongie`, loaded at boot before the network by
  `dongie-firewall.service` (`dongie firewall-apply`, root). Incoming traffic is dropped
  unless a zone allows the service: zones are interfaces (bridge `br0`/`usb*`, Tailscale,
  WireGuard `wg*`, Wi-Fi client `wlan*`, LTE `wwan*`), services are SSH, web UI, API,
  browser calls, DNS/DHCP, ping, muxtastic, plus own rules (port, protocol, optional
  source network). Defaults: everything on the bridge/Tailscale/WireGuard, SSH + ping
  from the Wi-Fi the stick joins, nothing from LTE; only the bridge may forward out.
  Always open: replies to the stick's own connections, IPv6 neighbour/router messages,
  DHCP replies, Tailscale/WireGuard UDP. The **Firewall** card (Network tab) edits
  `<state_dir>/firewall.toml`; a change is provisional for 2 minutes (keep it from the
  page or it rolls back, also after a dongie restart or reboot), and a config that would
  leave the web UI unreachable from both the bridge and Tailscale is refused. Interface
  names, ports, networks and comments are validated before anything reaches `nft`.
  `dongie firewall-show` prints the ruleset.
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
