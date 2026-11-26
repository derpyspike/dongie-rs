# API

All responses are JSON. Errors look like `{"error": "…"}`.

**Open port (8041, no login):**

| Method + path | |
|---|---|
| `GET /` | endpoint list + version |
| `GET /api/status` | full status, guest view |
| `GET /api/history?range=SECS&points=N` | bucketed history (range 60–86400 s, 10–600 points) + band times |
| `GET /api/messages` | SMS in RAM, newest first (`api_allow_sms`) |
| `POST /api/messages` `{"to":"+34…","text":"…"}` | send an SMS, returns `202 {"id":…}`; state follows in `GET` (`api_allow_sms`) |
| `GET /api/calls` | Wi-Fi calling call log from dongie-ims, newest first (`api_allow_sms`, contains numbers) |

```bash
curl -s http://mystick:8041/api/status | jq .lte.rsrp
curl -s -X POST http://mystick:8041/api/messages -H 'Content-Type: application/json' \
     -d '{"to":"+34600000000","text":"Hola desde dongie"}'
```

**Web port (8040, session cookie; POST/DELETE need `X-Dongie: 1`):**
`/api/me`, `/api/login`, `/api/logout`, `/api/status`, `/api/history`, `/api/net`,
`/api/messages` (+ `/read`, `/{id}`), `/api/network` (+ `/select` `{"network": "21401" | ""}`), `/api/calls` (+ `/{id}/reject`, `/{id}/answer` `{"sdp"}`, `/{id}/hangup`, `/{id}/dtmf` `{"digits"}`, `/dial` `{"to","sdp"}`), `/api/action/{hotspot-on|hotspot-off|lte-reconnect|reboot}`,
`/api/wifi/{hotspot,scan,connect,activate,forget}`, `/api/wireguard/{import,action}`, `/api/wireguard/config/{name}` (GET: interface + peers, keys only as "set"; POST `{listen_port, mtu, addresses[], private_key?: null|"generate"|key, peers[{public_key, endpoint, allowed_ips[], keepalive, psk?: null|""|key}]}`, re-activates an active tunnel), `/api/wireguard/create` (`{name, …same}`, own key pair unless given); peers take an optional `name`, stored in the connection's NetworkManager user data (`dongie.peer-name.<hex key>`),
`/api/mesh` (muxtastic's node, apps, heard nodes; messages, positions and addresses admin-only), `/api/mesh/config` (admin; GET, POST `{node_host, node_port, allow_admin_from[], retention_days}` → muxtastic.toml, restarts muxtastic), `/api/mesh/service` (admin; `{"action": "start"|"stop"|"restart"}`), `/api/update` (installed/latest version; guests only `{current, latest, newer}`), `/api/update/check` (admin, POST: ask GitHub now), `/api/update/apply` (admin, POST: download, verify, install through `dongie-update.service`), `/api/nettest`, `/api/settings` (GET; POST `{"changes":{key: value | null}}`, admin; restarts dongie), `/api/settings/confirm`, `/api/contacts` (GET; POST `{"number","name"}`, empty name deletes), `/api/notify` + `/api/notify/test[?index=N]`, `/api/ussd/{initiate|respond|cancel}` (`{"code":"*138#"}` / `{"text":"1"}`, admin only; returns `{"reply","state"}`). The guest/admin split is described under [Features](features.md).

On the web port, JSON and the page are sent gzip-compressed when the browser asks for it, and API answers carry a weak `ETag` (`Cache-Control: private, no-cache`), so a poll whose answer hasn't changed gets `304 Not Modified` with no body. `/api/net` is shared by all clients for 8 s (it reads NetworkManager, WireGuard and tailscaled); admins can add `?fresh=1` to skip that, as the page does after a change. The page stops polling while its tab is in the background, except for a slow check of calls (10 s) and SMS (30 s).

---
[← back to README](../README.md) · [docs index](README.md)
