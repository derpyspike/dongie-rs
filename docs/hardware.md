# Hardware notes & known limitations

Measured or verified on one stick (UZ801 v3.0, MSM8916, 317 MiB usable RAM, 3.6 GiB eMMC):

- **Wi-Fi radio does one mode at a time** (`iw`: interface combinations not supported):
  hotspot *or* client. Joining a network turns the hotspot off; if joining fails, dongie
  brings the hotspot back. The preferred client network gets autoconnect priority 110
  (hotspot 100).
- **No scanning while it's a hotspot** (`iw scan ap-force` → *Operation not supported*),
  so a scan pauses the hotspot for ~15 s and always restarts it.
- **2.4 GHz only**; regulatory domain stays `00` (cfg80211 is built in and loads before the
  root fs, so `regulatory.db` isn't available; `00` is stricter than ES anyway).
- **Bluetooth** shows up as `hci0` but its radio never sends or receives on this board.
- **GNSS** exists in the modem but reports no usable satellites (no antenna).
- **VoLTE** isn't available with this modem firmware/kernel (no IMS service, no sound card).
- **Phones with private MACs** (e.g. GrapheneOS) send no DHCP hostname, so hotspot clients
  show only MAC/IP.
- **Kernel lacks connmark** — Tailscale warns about it; harmless unless the stick were an
  exit node / subnet router.
- **Flash:** the eMMC doesn't report wear. The System tab tracks the write rate; logs and
  mount options were tuned on that stick (journald capped, NM/MM at WARN, `noatime,commit=30`,
  `tailscaled --no-logs-no-support`).

---
[← back to README](../README.md) · [docs index](README.md)
