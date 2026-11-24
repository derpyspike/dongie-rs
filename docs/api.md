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
`/api/messages` (+ `/read`, `/{id}`), `/api/calls` (+ `/{id}/reject`, `/{id}/answer` `{"sdp"}`, `/{id}/hangup`, `/{id}/dtmf` `{"digits"}`, `/dial` `{"to","sdp"}`), `/api/action/{hotspot-on|hotspot-off|lte-reconnect|reboot}`,
`/api/wifi/{hotspot,scan,connect,activate,forget}`, `/api/wireguard/{import,action}`,
`/api/nettest`. The guest/admin split is described under [Features](features.md).

---
[← back to README](../README.md) · [docs index](README.md)
