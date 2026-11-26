# Helper scripts (`contrib/`)

Small scripts that run on the stick next to dongie (dongie shows their service state):

| File | What it does |
|---|---|
| `lte-status` | `lte-status [-w] [-p]` — one-screen modem summary in the terminal (colours, live mode, ping) |
| `fix-attach-apn` + `.service` | at boot, rewrites the modem's LTE **initial-attach APN** (QMI profile 3). The UZ801 firmware re-seeds it with `mobile.three.com.hk` on every modem boot, which makes the network refuse LTE registration. Picks the APN from the **SIM's MCC+MNC** (built in: Orange/Simyo, Vodafone, Movistar ES; Telekom, O2 DE; unknown SIMs are left alone) and brings up the matching NetworkManager connection. |
| `dongie-leds` + `.service` | status LEDs: WAN LED = LTE (on / blinking while searching / off), WLAN LED = Tailscale running (or, without Tailscale, the Wi-Fi interface up: hotspot or client), power LED untouched. Polls every 5 s with one `busctl` property read plus sysfs (no `mmcli`/`nmcli`/`tailscale` runs). Finds the LEDs by function: UZ801 v3 and JZ01-45-V33 (`blue:wan`, `green:wlan`), MF800 (`green:wan-online` + `red:wan` for "modem down", `green:wlan`), HMUF02 v5 (labels only: green = wan, blue = wlan). `DRY_RUN=1 dongie-leds` shows what it found; override in `/etc/default/dongie-leds` |
| `lnsd/` | Reticulum on the stick (Leviculum `lnsd`, storage in RAM only with hard caps) for LXMF notifications; see [contrib/lnsd/README.md](../contrib/lnsd/README.md) |

Install: `/usr/local/bin/lte-status`, `/usr/local/sbin/{fix-attach-apn,dongie-leds}`,
units into `/etc/systemd/system/`, then `systemctl enable --now <unit>`.


### Several SIMs: one NetworkManager connection per SIM

Bind each connection to its SIM's MCC+MNC, so NetworkManager picks the right one
automatically when you swap SIMs (the PIN is tried with whichever connection
activates first, so it helps if all SIMs share a PIN):

```bash
sudo nmcli con modify lte gsm.sim-operator-id 21403                    # Orange / Simyo
sudo nmcli con clone lte lte-vodafone                                  # (clone drops the PIN)
sudo nmcli con clone lte lte-movistar
PIN="$(sudo nmcli -s -g gsm.pin con show lte)"
# set PIN and password in ONE modify: nmcli doesn't load existing secrets, so a
# later modify that touches only one secret silently drops the other
sudo nmcli con modify lte-vodafone gsm.sim-operator-id 21401 gsm.apn airtelwap.es \
     gsm.username wap@wap gsm.pin-flags 0 gsm.password-flags 0 \
     gsm.pin "$PIN" gsm.password wap125                                # public defaults
sudo nmcli con modify lte-movistar gsm.sim-operator-id 21407 gsm.apn telefonica.es \
     gsm.username telefonica gsm.pin-flags 0 gsm.password-flags 0 \
     gsm.pin "$PIN" gsm.password telefonica                            # public defaults
unset PIN
```

Tested 2026-11-24 with a Vodafone ES SIM: data on `airtelwap.es` and VoWiFi (dongie-ims)
registered within seconds. On the first boot NetworkManager may first try another
connection while the SIM is still locked (the operator code is unknown before the PIN);
the network rejects it ("service option not subscribed") and the matching one follows.

---
[← back to README](../README.md) · [docs index](README.md)
