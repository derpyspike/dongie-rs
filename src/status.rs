// SPDX-License-Identifier: EUPL-1.2
//! Periodically collects everything shown on the dashboard.

use crate::cmd::{self, kv, num, qv, qv_in};
use crate::config::Config;
use crate::usage::{Usage, UsageView};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Default, Clone, Serialize)]
pub struct Status {
    pub updated: u64,
    pub system: System,
    pub lte: Lte,
    pub hotspot: Hotspot,
    pub tailscale: Tailscale,
    pub usage: UsageView,
    pub wifi: crate::wifi::Wifi,
    pub services: Vec<Service>,
    pub leds: Vec<Led>,
    /// Wi-Fi calling (dongie-ims); None when it isn't running
    pub ims: Option<crate::ims::ImsStatus>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct System {
    pub hostname: String,
    pub model: String,
    pub os: String,
    pub kernel: String,
    pub uptime_secs: u64,
    pub load: [f64; 3],
    pub cpus: usize,
    pub cpu_mhz: Option<u64>,
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
    pub swap_total_kb: u64,
    pub swap_used_kb: u64,
    pub disk_total: u64,
    pub disk_used: u64,
    /// bytes written to the whole eMMC since boot (/sys/block/mmcblk0/stat)
    pub flash_written_since_boot: Option<u64>,
    /// ext4 lifetime write counter of the root fs (/sys/fs/ext4/*/lifetime_write_kbytes), KiB
    pub ext4_lifetime_kib: Option<u64>,
    pub temps: Vec<(String, f64)>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Lte {
    pub present: bool,
    pub state: Option<String>,
    pub failed_reason: Option<String>,
    pub tech: Option<String>,
    pub operator_name: Option<String>,
    pub operator_code: Option<String>,
    pub registration: Option<String>,
    pub modes: Option<String>,
    pub signal_quality: Option<f64>,
    pub rsrp: Option<f64>,
    pub rsrq: Option<f64>,
    pub snr: Option<f64>,
    pub rssi: Option<f64>,
    pub wcdma_rssi: Option<f64>,
    pub wcdma_ecio: Option<f64>,
    pub band: Option<String>,
    pub channel: Option<String>,
    pub ip: Option<String>,
    pub apn: Option<String>,
    pub firmware: Option<String>,
    /// human label: "LTE B3", "3G 2100"
    pub band_label: Option<String>,
    pub cs_attached: Option<String>,
    pub ps_attached: Option<String>,
    pub roaming: Option<String>,
    pub hw_revision: Option<String>,
    pub supported_bands: Vec<String>,
    pub current_bands: Vec<String>,
    pub sim_operator: Option<String>,
    pub sim_operator_code: Option<String>,
    pub cells: Cells,
    /// every field ModemManager reports (modem, SIM, data bearer); sensitive ones admin-only
    pub details: Vec<Detail>,
    // sensitive (admin only)
    pub cell_id: Option<String>,
    pub tac: Option<String>,
    /// LTE timing advance (µs) — roughly the distance to the tower
    pub timing_advance_us: Option<f64>,
    pub own_number: Option<String>,
    pub imei: Option<String>,
    pub iccid: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Detail {
    pub group: &'static str,
    pub key: String,
    pub value: String,
    pub sensitive: bool,
}

const SENSITIVE: &[&str] = &["imei", "equipment-identifier", "device-identifier", "own-numbers", "imsi", "iccid", "eid"];

/// Turn `mmcli -K` output into a flat, human-ish list. `value[n]` entries are joined.
pub fn details_from(group: &'static str, text: &str, strip: &[&str]) -> Vec<Detail> {
    let mut out: Vec<Detail> = vec![];
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for line in text.lines() {
        let Some(i) = line.find(": ") else { continue };
        let key = line[..i].trim();
        let val = line[i + 2..].trim();
        if val.is_empty() || val == "--" || key.ends_with(".length") || key.contains("dbus-path") {
            continue;
        }
        let base = match key.rfind(".value[") {
            Some(j) => &key[..j],
            None => key,
        };
        if ["modem.generic.sim", "modem.generic.sim-slots", "modem.generic.bearers"].contains(&base) {
            continue;
        }
        if let Some(&k) = seen.get(base) {
            // newline, not ", ": some values contain commas themselves ("allowed: 3g, 4g; ...")
            out[k].value.push('\n');
            out[k].value.push_str(val);
            continue;
        }
        let mut pretty = base;
        for p in strip {
            if let Some(r) = base.strip_prefix(p) {
                pretty = r;
                break;
            }
        }
        seen.insert(base.to_string(), out.len());
        out.push(Detail {
            group,
            key: pretty.replace('.', " · ").replace('-', " "),
            value: val.to_string(),
            sensitive: SENSITIVE.iter().any(|s| base.contains(s)),
        });
    }
    out
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Cells {
    pub idle: Option<bool>,
    pub serving_pci: Option<String>,
    pub cells: Vec<Cell>,
    /// frequencies the network told us to watch (even without measured cells)
    pub freqs: Vec<Freq>,
    // sensitive (admin only)
    pub plmn: Option<String>,
    pub tac: Option<String>,
    pub global_cell_id: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Cell {
    /// lte-intra | lte-inter | wcdma | gsm
    pub kind: String,
    pub channel: Option<u32>,
    pub band: Option<String>,
    /// PCI (LTE) / PSC (WCDMA)
    pub id: Option<String>,
    pub serving: bool,
    pub rsrp: Option<f64>,
    pub rsrq: Option<f64>,
    pub rssi: Option<f64>,
    pub rscp: Option<f64>,
    pub ecno: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Freq {
    pub kind: String,
    pub channel: Option<u32>,
    pub band: Option<String>,
    pub priority: Option<String>,
}

/// LTE downlink EARFCN -> band number (bands this modem / EU networks use)
pub fn earfcn_band(n: u32) -> Option<u32> {
    const T: &[(u32, u32, u32)] = &[
        (1, 0, 599), (2, 600, 1199), (3, 1200, 1949), (4, 1950, 2399), (5, 2400, 2649),
        (7, 2750, 3449), (8, 3450, 3799), (20, 6150, 6449), (28, 9210, 9659),
        (38, 37750, 38249), (40, 38650, 39649), (41, 39650, 41589),
    ];
    T.iter().find(|(_, a, b)| (*a..=*b).contains(&n)).map(|x| x.0)
}

/// UMTS downlink UARFCN -> band number
pub fn uarfcn_band(n: u32) -> Option<u32> {
    const T: &[(u32, u32, u32)] = &[(1, 10562, 10838), (2, 9662, 9938), (5, 4357, 4458), (8, 2937, 3088)];
    T.iter().find(|(_, a, b)| (*a..=*b).contains(&n)).map(|x| x.0)
}

/// qmicli band class ("eutran-3", "wcdma-2100", "wcdma-900") -> "LTE B3" / "3G 2100"
pub fn band_label(class: &str) -> String {
    if let Some(n) = class.strip_prefix("eutran-") {
        format!("LTE B{n}")
    } else if let Some(n) = class.strip_prefix("wcdma-") {
        format!("3G {n}")
    } else {
        class.to_string()
    }
}

/// ModemManager band names ("eutran-3", "utran-8") -> "LTE B3" / "3G B8"
pub fn mm_band_label(b: &str) -> String {
    if let Some(n) = b.strip_prefix("eutran-") {
        format!("LTE B{n}")
    } else if let Some(n) = b.strip_prefix("utran-") {
        format!("3G B{n}")
    } else {
        b.to_string()
    }
}

fn quoted(s: &str) -> Option<String> {
    let a = s.find('\'')?;
    let r = &s[a + 1..];
    Some(r[..r.find('\'')?].to_string())
}

/// Parse `qmicli --nas-get-cell-location-info`. Returns (cells, timing advance µs).
pub fn parse_cells(t: &str) -> (Cells, Option<f64>) {
    let mut c = Cells::default();
    let mut ta = None;
    let mut section = String::new();
    let mut chan: Option<u32> = None;
    let mut band: Option<String> = None;
    let mut cur: Option<Cell> = None;
    let flush = |cur: &mut Option<Cell>, c: &mut Cells| {
        if let Some(x) = cur.take() {
            c.cells.push(x);
        }
    };
    for line in t.lines() {
        if line.starts_with('[') || line.trim().is_empty() {
            continue;
        }
        let depth = line.chars().take_while(|ch| *ch == '\t').count();
        let s = line.trim();
        if depth == 0 {
            flush(&mut cur, &mut c);
            section = s.trim_end_matches(':').to_string();
            chan = None;
            band = None;
            if s.starts_with("LTE Timing Advance") {
                ta = quoted(s).and_then(|v| num(&v));
            }
            continue;
        }
        let kind = if section.starts_with("Intrafrequency LTE") {
            "lte-intra"
        } else if section.starts_with("Interfrequency LTE") {
            "lte-inter"
        } else if section.contains("WCDMA") || section.starts_with("UMTS") {
            "wcdma"
        } else if section.contains("GSM") {
            "gsm"
        } else {
            "other"
        };
        if s.starts_with("Frequency [") {
            flush(&mut cur, &mut c);
            chan = None;
            band = None;
            continue;
        }
        if s.starts_with("Cell [") {
            flush(&mut cur, &mut c);
            cur = Some(Cell { kind: kind.into(), channel: chan, band: band.clone(), ..Default::default() });
            continue;
        }
        let Some((k, rest)) = s.split_once(':') else { continue };
        let v = quoted(rest);
        let vn = v.as_deref().and_then(num);
        if k.contains("Absolute RF Channel Number") {
            chan = v.as_deref().and_then(|x| x.parse().ok());
            band = chan.and_then(|n| {
                if k.starts_with("EUTRA") {
                    earfcn_band(n).map(|b| format!("LTE B{b}"))
                } else {
                    uarfcn_band(n).map(|b| format!("3G B{b}"))
                }
            });
            if kind != "lte-intra" && cur.is_none() {
                c.freqs.push(Freq { kind: kind.into(), channel: chan, band: band.clone(), priority: None });
            }
            continue;
        }
        if let Some(cell) = cur.as_mut() {
            match k {
                "Physical Cell ID" | "Primary Scrambling Code" | "PSC" | "BSIC ID" => cell.id = v,
                "RSRP" => cell.rsrp = vn,
                "RSRQ" => cell.rsrq = vn,
                "RSSI" => cell.rssi = vn,
                "CPICH RSCP" | "RSCP" => cell.rscp = vn,
                "CPICH EcNo" | "EcNo" | "ECIO" => cell.ecno = vn,
                _ => {}
            }
            continue;
        }
        match k {
            "UE In Idle" if c.idle.is_none() => c.idle = v.map(|x| x == "yes"),
            "PLMN" => c.plmn = v,
            "Tracking Area Code" => c.tac = v,
            "Global Cell ID" => c.global_cell_id = v,
            "Serving Cell ID" => c.serving_pci = v,
            "Cell Reselection Priority" => {
                if let Some(f) = c.freqs.last_mut() {
                    f.priority = v;
                }
            }
            _ => {}
        }
    }
    flush(&mut cur, &mut c);
    let sp = c.serving_pci.clone();
    for cell in c.cells.iter_mut() {
        cell.serving = cell.kind == "lte-intra" && cell.id.is_some() && cell.id == sp;
    }
    (c, ta)
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Hotspot {
    pub active: bool,
    pub ssid: Option<String>,
    pub band: Option<String>,
    pub channel: Option<String>,
    pub txpower: Option<String>,
    pub address: Option<String>,
    pub client_count: usize,
    pub clients: Vec<Client>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Client {
    pub mac: String,
    pub ip: Option<String>,
    pub hostname: Option<String>,
    pub signal_dbm: Option<f64>,
    pub connected_secs: Option<u64>,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Tailscale {
    pub state: Option<String>,
    pub ip: Option<String>,
    pub hostname: Option<String>,
    /// MagicDNS name, e.g. "mystick.example.ts.net" (for the HTTPS link needed by calls)
    pub dns_name: Option<String>,
    pub peers_total: usize,
    pub peers_online: usize,
    pub health: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Service {
    pub name: String,
    pub state: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Led {
    pub name: String,
    pub on: bool,
    pub trigger: String,
}

impl Status {
    /// What a guest / anonymous viewer may see.
    pub fn for_guest(&self) -> Status {
        let mut s = self.clone();
        s.lte.own_number = None;
        s.lte.imei = None;
        s.lte.iccid = None;
        // cell + tracking area reveal the location (cell databases), TA the distance
        s.lte.cell_id = None;
        s.lte.tac = None;
        s.lte.timing_advance_us = None;
        s.lte.cells.plmn = None;
        s.lte.cells.tac = None;
        s.lte.cells.global_cell_id = None;
        s.lte.details.retain(|d| !d.sensitive);
        s.wifi.for_guest();
        s.hotspot.clients.clear();
        if let Some(i) = s.ims.as_mut() {
            i.for_guest();
        }
        s
    }
}

pub struct Collector {
    pub cfg: Config,
    pub status: Arc<RwLock<Status>>,
    pub usage: Arc<Mutex<Usage>>,
    pub history: Arc<Mutex<crate::history::History>>,
}

fn sample(st: &Status, flash_kib_min: Option<f64>) -> crate::history::Sample {
    let l = &st.lte;
    let tmax = |pred: &dyn Fn(&str) -> bool| {
        st.system.temps.iter().filter(|t| pred(&t.0)).map(|t| t.1).fold(None, |a: Option<f64>, x| Some(a.map_or(x, |a| a.max(x))))
    };
    crate::history::Sample {
        t: st.updated,
        connected: matches!(l.state.as_deref(), Some("connected") | Some("registered") | Some("connecting")),
        tech: l.tech.clone(),
        band: l.band_label.clone(),
        rsrp: l.rsrp,
        rsrq: l.rsrq,
        snr: l.snr,
        w_rssi: l.wcdma_rssi,
        w_ecio: l.wcdma_ecio,
        neighbors: l.cells.cells.iter().filter(|c| !c.serving).count() as u32,
        temp_cpu: tmax(&|n| n.starts_with("cpu")),
        temp_modem: tmax(&|n| n == "modem"),
        flash_kib_min,
        mem_used_pct: (st.system.mem_total_kb > 0).then(|| {
            ((st.system.mem_total_kb - st.system.mem_available_kb) as f64 / st.system.mem_total_kb as f64 * 1000.0).round() / 10.0
        }),
    }
}

impl Collector {
    pub async fn run(self) {
        let mut tick = 0u64;
        let mut prev_flash: Option<(u64, u64)> = None; // (unix time, bytes)
        loop {
            let st = self.collect().await;
            let now_flash = st.system.flash_written_since_boot.map(|b| (st.updated, b));
            let rate = match (prev_flash, now_flash) {
                (Some((t0, b0)), Some((t1, b1))) if t1 > t0 && b1 >= b0 => {
                    Some(((b1 - b0) as f64 / 1024.0) / ((t1 - t0) as f64 / 60.0))
                }
                _ => None,
            };
            prev_flash = now_flash;
            self.history.lock().await.push(sample(&st, rate));
            *self.status.write().await = st;
            tick += 1;
            // usage.json: every ~10 min (flash wear); also saved on SIGTERM
            if tick % (600 / self.cfg.poll_secs.max(3)).max(1) == 0 {
                self.usage.lock().await.save_if_dirty();
            }
            tokio::time::sleep(std::time::Duration::from_secs(self.cfg.poll_secs.max(3))).await;
        }
    }

    async fn collect(&self) -> Status {
        let (system, lte, hotspot, tailscale, services, wifi) = tokio::join!(
            system(),
            self.lte(),
            self.hotspot(),
            tailscale(),
            services(&self.cfg.services),
            crate::wifi::collect(&self.cfg.hotspot_connection),
        );
        let ims = crate::ims::status(&self.cfg.ims_socket).await;
        let usage = self.usage_view().await;
        Status {
            updated: cmd::now(),
            system,
            lte,
            hotspot,
            tailscale,
            usage,
            wifi,
            services,
            leds: leds(),
            ims,
        }
    }

    async fn qmi(&self, flag: &str) -> Option<String> {
        cmd::sudo(&["/usr/bin/qmicli", "-p", "-d", &self.cfg.qmi_device, flag]).await
    }

    async fn usage_view(&self) -> UsageView {
        let stats = self.qmi("--wds-get-packet-statistics").await;
        let mut u = self.usage.lock().await;
        let now = cmd::now();
        match stats {
            Some(t) => {
                let rx = qv_plain(&t, "RX bytes OK:");
                let tx = qv_plain(&t, "TX bytes OK:");
                if let (Some(rx), Some(tx)) = (rx, tx) {
                    u.update(rx, tx, now);
                    return u.view(now, true);
                }
                u.view(now, false)
            }
            None => u.view(now, false),
        }
    }

    async fn lte(&self) -> Lte {
        let mut l = Lte::default();
        let apn_args = ["-t", "-g", "gsm.apn", "con", "show", self.cfg.lte_connection.as_str()];
        let (mm, sim, sig, band, serving, apn, ip, cells) = tokio::join!(
            cmd::run("mmcli", &["-m", "any", "-K"]),
            cmd::run("mmcli", &["-i", "any", "-K"]),
            self.qmi("--nas-get-signal-info"),
            self.qmi("--nas-get-rf-band-info"),
            self.qmi("--nas-get-serving-system"),
            cmd::run("nmcli", &apn_args),
            cmd::run("ip", &["-j", "-4", "addr", "show", "dev", "wwan0"]),
            self.qmi("--nas-get-cell-location-info"),
        );
        let mut details = vec![];
        if let Some(t) = &mm {
            details.extend(details_from("Modem", t, &["modem.generic.", "modem."]));
            if let Some(b) = kv(t).get("modem.generic.bearers.value[1]") {
                if let Some(bt) = cmd::run("mmcli", &["-b", b, "-K"]).await {
                    details.extend(details_from("Data connection", &bt, &["bearer.stats.", "bearer.status.", "bearer.properties.", "bearer."]));
                }
            }
        }
        if let Some(t) = &sim {
            details.extend(details_from("SIM", t, &["sim.properties.", "sim."]));
        }
        l.details = details;
        if let Some(t) = cells {
            let (c, ta) = parse_cells(&t);
            l.cells = c;
            l.timing_advance_us = ta;
        }
        if let Some(t) = mm {
            let m = kv(&t);
            l.present = m.contains_key("modem.generic.state");
            let g = |k: &str| m.get(k).cloned();
            l.state = g("modem.generic.state");
            l.failed_reason = g("modem.generic.state-failed-reason");
            l.tech = g("modem.generic.access-technologies.value[1]");
            l.operator_name = g("modem.3gpp.operator-name");
            l.operator_code = g("modem.3gpp.operator-code");
            l.registration = g("modem.3gpp.registration-state");
            l.modes = g("modem.generic.current-modes");
            l.signal_quality = g("modem.generic.signal-quality.value").and_then(|s| num(&s));
            l.firmware = g("modem.generic.revision");
            l.own_number = g("modem.generic.own-numbers.value[1]");
            l.imei = g("modem.3gpp.imei");
            l.hw_revision = g("modem.generic.hardware-revision");
            let list = |prefix: &str| -> Vec<String> {
                let mut v: Vec<(usize, String)> = m
                    .iter()
                    .filter_map(|(k, val)| {
                        let i = k.strip_prefix(prefix)?.trim_end_matches(']').parse().ok()?;
                        Some((i, val.clone()))
                    })
                    .collect();
                v.sort();
                v.into_iter().map(|x| mm_band_label(&x.1)).collect()
            };
            l.supported_bands = list("modem.generic.supported-bands.value[");
            l.current_bands = list("modem.generic.current-bands.value[");
        }
        if let Some(t) = sim {
            let m = kv(&t);
            l.iccid = m.get("sim.properties.iccid").cloned();
            l.sim_operator = m.get("sim.properties.operator-name").cloned();
            l.sim_operator_code = m.get("sim.properties.operator-code").cloned();
        }
        if let Some(t) = sig {
            l.rsrp = qv_in(&t, "LTE:", "RSRP").and_then(|s| num(&s));
            l.rsrq = qv_in(&t, "LTE:", "RSRQ").and_then(|s| num(&s));
            l.snr = qv_in(&t, "LTE:", "SNR").and_then(|s| num(&s));
            l.rssi = qv_in(&t, "LTE:", "RSSI").and_then(|s| num(&s));
            l.wcdma_rssi = qv_in(&t, "WCDMA:", "RSSI").and_then(|s| num(&s));
            l.wcdma_ecio = qv_in(&t, "WCDMA:", "ECIO").and_then(|s| num(&s));
        }
        if let Some(t) = band {
            l.band = qv(&t, "Active Band Class");
            l.channel = qv(&t, "Active Channel");
            l.band_label = l.band.as_deref().map(band_label);
        }
        if let Some(t) = serving {
            l.cell_id = qv(&t, "3GPP cell ID");
            l.tac = qv(&t, "LTE tracking area code");
            l.cs_attached = qv(&t, "CS:");
            l.ps_attached = qv(&t, "PS:");
            l.roaming = qv(&t, "Roaming status");
        }
        l.apn = apn.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        l.ip = ip.and_then(|j| first_ipv4(&j));
        l
    }

    async fn hotspot(&self) -> Hotspot {
        let mut h = Hotspot::default();
        let nm_args = [
            "-t",
            "-f",
            "GENERAL.STATE,802-11-wireless.ssid,802-11-wireless.band,ipv4.addresses",
            "con",
            "show",
            self.cfg.hotspot_connection.as_str(),
        ];
        let (nm, info, dump, leases) = tokio::join!(
            cmd::run("nmcli", &nm_args),
            cmd::run("/usr/sbin/iw", &["dev", "wlan0", "info"]),
            cmd::run("/usr/sbin/iw", &["dev", "wlan0", "station", "dump"]),
            cmd::sudo(&["/usr/bin/cat", "/var/lib/NetworkManager/dnsmasq-wlan0.leases"]),
        );
        if let Some(t) = nm {
            for line in t.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let v = v.trim();
                    match k {
                        "GENERAL.STATE" => h.active = v == "activated",
                        "802-11-wireless.ssid" if !v.is_empty() => h.ssid = Some(v.into()),
                        "802-11-wireless.band" if !v.is_empty() => {
                            h.band = Some(if v == "bg" { "2.4 GHz".into() } else { v.into() })
                        }
                        "ipv4.addresses" if !v.is_empty() => h.address = Some(v.into()),
                        _ => {}
                    }
                }
            }
        }
        if h.active {
            if let Some(t) = info {
                for line in t.lines() {
                    let line = line.trim();
                    if let Some(r) = line.strip_prefix("channel ") {
                        h.channel = Some(r.split(',').next().unwrap_or(r).to_string());
                    } else if let Some(r) = line.strip_prefix("txpower ") {
                        h.txpower = Some(r.to_string());
                    }
                }
            }
            let mut clients = parse_stations(dump.as_deref().unwrap_or(""));
            if let Some(l) = leases {
                for line in l.lines() {
                    let f: Vec<&str> = line.split_whitespace().collect();
                    if f.len() >= 4 {
                        if let Some(c) = clients.iter_mut().find(|c| c.mac.eq_ignore_ascii_case(f[1])) {
                            c.ip = Some(f[2].to_string());
                            if f[3] != "*" {
                                c.hostname = Some(f[3].to_string());
                            }
                        }
                    }
                }
            }
            h.client_count = clients.len();
            h.clients = clients;
        }
        h
    }
}

fn qv_plain(t: &str, label: &str) -> Option<u64> {
    t.lines()
        .find(|l| l.contains(label))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

pub fn first_ipv4(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let a = v.get(0)?.get("addr_info")?.get(0)?;
    Some(format!(
        "{}/{}",
        a.get("local")?.as_str()?,
        a.get("prefixlen")?.as_u64()?
    ))
}

fn parse_stations(t: &str) -> Vec<Client> {
    let mut out: Vec<Client> = vec![];
    for line in t.lines() {
        if let Some(r) = line.strip_prefix("Station ") {
            out.push(Client {
                mac: r.split_whitespace().next().unwrap_or("").to_string(),
                ..Default::default()
            });
            continue;
        }
        let Some(c) = out.last_mut() else { continue };
        let line = line.trim();
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            match k {
                "rx bytes" => c.rx_bytes = v.parse().ok(),
                "tx bytes" => c.tx_bytes = v.parse().ok(),
                "signal" => c.signal_dbm = num(v),
                "connected time" => c.connected_secs = num(v).map(|x| x as u64),
                _ => {}
            }
        }
    }
    out
}

async fn tailscale() -> Tailscale {
    let mut ts = Tailscale::default();
    let Some(j) = cmd::run("tailscale", &["status", "--json"]).await else {
        ts.state = Some("unavailable".into());
        return ts;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&j) else {
        return ts;
    };
    ts.state = v["BackendState"].as_str().map(String::from);
    ts.ip = v["Self"]["TailscaleIPs"][0].as_str().map(String::from);
    ts.hostname = v["Self"]["HostName"].as_str().map(String::from);
    ts.dns_name = v["Self"]["DNSName"].as_str().map(|s| s.trim_end_matches('.').to_string()).filter(|s| !s.is_empty());
    if let Some(p) = v["Peer"].as_object() {
        ts.peers_total = p.len();
        ts.peers_online = p.values().filter(|x| x["Online"].as_bool() == Some(true)).count();
    }
    if let Some(h) = v["Health"].as_array() {
        ts.health = h.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    ts
}

async fn services(names: &[String]) -> Vec<Service> {
    let mut args = vec!["is-active"];
    args.extend(names.iter().map(|s| s.as_str()));
    let out = cmd::run_any("systemctl", &args).await.map(|o| o.1).unwrap_or_default();
    let states: Vec<&str> = out.lines().collect();
    names
        .iter()
        .enumerate()
        .map(|(i, n)| Service {
            name: n.clone(),
            state: states.get(i).copied().unwrap_or("unknown").to_string(),
        })
        .collect()
}

fn read(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string())
}

fn leds() -> Vec<Led> {
    ["blue:wan", "green:wlan", "red:power"]
        .iter()
        .map(|n| {
            let base = format!("/sys/class/leds/{n}");
            let on = read(&format!("{base}/brightness")).map(|b| b != "0").unwrap_or(false);
            let trig = read(&format!("{base}/trigger"))
                .and_then(|t| {
                    let a = t.find('[')?;
                    let b = t[a..].find(']')?;
                    Some(t[a + 1..a + b].to_string())
                })
                .unwrap_or_default();
            Led { name: n.to_string(), on, trigger: trig }
        })
        .collect()
}

async fn system() -> System {
    let mut s = System {
        hostname: read("/proc/sys/kernel/hostname").unwrap_or_default(),
        model: read("/proc/device-tree/model").unwrap_or_default(),
        kernel: read("/proc/sys/kernel/osrelease").unwrap_or_default(),
        cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        ..Default::default()
    };
    if let Some(os) = read("/etc/os-release") {
        s.os = os
            .lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_default();
    }
    s.uptime_secs = read("/proc/uptime").and_then(|u| num(&u)).unwrap_or(0.0) as u64;
    if let Some(l) = read("/proc/loadavg") {
        let f: Vec<f64> = l.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
        if f.len() == 3 {
            s.load = [f[0], f[1], f[2]];
        }
    }
    s.cpu_mhz = read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")
        .and_then(|v| v.parse::<u64>().ok())
        .map(|khz| khz / 1000);
    if let Some(m) = read("/proc/meminfo") {
        let get = |k: &str| {
            m.lines()
                .find(|l| l.starts_with(k))
                .and_then(|l| num(&l[k.len()..]))
                .unwrap_or(0.0) as u64
        };
        s.mem_total_kb = get("MemTotal:");
        s.mem_available_kb = get("MemAvailable:");
        s.swap_total_kb = get("SwapTotal:");
        s.swap_used_kb = s.swap_total_kb.saturating_sub(get("SwapFree:"));
    }
    s.flash_written_since_boot = read("/sys/block/mmcblk0/stat")
        .and_then(|t| t.split_whitespace().nth(6).and_then(|x| x.parse::<u64>().ok()))
        .map(|sectors| sectors * 512);
    if let Ok(rd) = std::fs::read_dir("/sys/fs/ext4") {
        s.ext4_lifetime_kib = rd
            .flatten()
            .find_map(|e| read(&e.path().join("lifetime_write_kbytes").to_string_lossy()).and_then(|v| v.parse().ok()));
    }
    if let Some(df) = cmd::run("df", &["-P", "-B1", "/"]).await {
        if let Some(line) = df.lines().nth(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() >= 4 {
                s.disk_total = f[1].parse().unwrap_or(0);
                s.disk_used = f[2].parse().unwrap_or(0);
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir("/sys/class/thermal") {
        let mut temps: Vec<(String, f64)> = rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("thermal_zone"))
            .filter_map(|e| {
                let p = e.path();
                let ty = read(&p.join("type").to_string_lossy())?;
                let t = read(&p.join("temp").to_string_lossy())?.parse::<f64>().ok()? / 1000.0;
                Some((ty.trim_end_matches("-thermal").to_string(), t))
            })
            .collect();
        temps.sort_by(|a, b| a.0.cmp(&b.0));
        s.temps = temps;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_view_hides_sensitive() {
        let mut s = Status::default();
        s.lte.cell_id = Some("12345678".into());
        s.lte.tac = Some("4321".into());
        s.lte.imei = Some("350000000000001".into());
        s.lte.own_number = Some("+34".into());
        s.lte.iccid = Some("8934".into());
        s.lte.band = Some("eutran-3".into());
        s.hotspot.clients.push(Client { mac: "aa".into(), ..Default::default() });
        s.hotspot.client_count = 1;
        let g = s.for_guest();
        assert!(g.lte.cell_id.is_none() && g.lte.tac.is_none() && g.lte.imei.is_none());
        assert!(g.lte.own_number.is_none() && g.lte.iccid.is_none());
        assert!(g.hotspot.clients.is_empty());
        assert_eq!(g.hotspot.client_count, 1);
        assert_eq!(g.lte.band.as_deref(), Some("eutran-3"));
    }

    #[test]
    fn cells_connected_and_idle() {
        let t = "[/dev/wwan0qmi0] Successfully got cell location info\nIntrafrequency LTE Info\n\tUE In Idle: 'no'\n\tPLMN: '21403'\n\tTracking Area Code: '4321'\n\tGlobal Cell ID: '12345678'\n\tEUTRA Absolute RF Channel Number: '1849' (E-UTRA band 3: 1800+)\n\tServing Cell ID: '122'\n\tCell [0]:\n\t\tPhysical Cell ID: '122'\n\t\tRSRQ: '-19.4' dB\n\t\tRSRP: '-103.6' dBm\n\t\tRSSI: '-63.7' dBm\n\tCell [1]:\n\t\tPhysical Cell ID: '77'\n\t\tRSRQ: '-20.0' dB\n\t\tRSRP: '-109.1' dBm\n\t\tRSSI: '-75.9' dBm\nInterfrequency LTE Info\n\tUE In Idle: 'yes'\n\tFrequency [0]:\n\t\tEUTRA Absolute RF Channel Number: '201' (E-UTRA band 1: 2100)\n\t\tCell Reselection Priority: '6'\nLTE Info Neighboring WCDMA\n\tUE In Idle: 'yes'\n\tFrequency [0]:\n\t\tUTRA Absolute RF Channel Number: '2959'\n\t\tCell Reselection Priority: '1'\nLTE Timing Advance: '5' us\n";
        let (c, ta) = parse_cells(t);
        assert_eq!(ta, Some(5.0));
        assert_eq!(c.idle, Some(false));
        assert_eq!(c.global_cell_id.as_deref(), Some("12345678"));
        assert_eq!(c.cells.len(), 2);
        assert!(c.cells[0].serving && !c.cells[1].serving);
        assert_eq!(c.cells[1].id.as_deref(), Some("77"));
        assert_eq!(c.cells[1].rsrp, Some(-109.1));
        assert_eq!(c.cells[0].band.as_deref(), Some("LTE B3"));
        assert_eq!(c.freqs.len(), 2);
        assert_eq!(c.freqs[0].band.as_deref(), Some("LTE B1"));
        assert_eq!(c.freqs[0].priority.as_deref(), Some("6"));
        assert_eq!(c.freqs[1].band.as_deref(), Some("3G B8"));
        assert_eq!(band_label("eutran-3"), "LTE B3");
        assert_eq!(band_label("wcdma-2100"), "3G 2100");
        assert_eq!(mm_band_label("utran-8"), "3G B8");
        assert_eq!(mm_band_label("eutran-20"), "LTE B20");
    }

    #[test]
    fn details() {
        let t = "modem.dbus-path : /x\nmodem.generic.drivers.length : 2\nmodem.generic.drivers.value[1] : a\nmodem.generic.drivers.value[2] : b\nmodem.3gpp.imei : 8600\nmodem.generic.bearers.value[1] : /b\nmodem.generic.revision : --\n";
        let d = details_from("Modem", t, &["modem.generic.", "modem."]);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].key, "drivers");
        assert_eq!(d[0].value, "a\nb");
        assert_eq!(d[1].key, "3gpp · imei");
        assert!(d[1].sensitive && !d[0].sensitive);
    }

    #[test]
    fn stations() {
        let t = "Station 3c:22:fb:aa:bb:cc (on wlan0)\n\tinactive time:\t120 ms\n\trx bytes:\t12345\n\ttx bytes:\t678\n\tsignal:  \t-45 [-45] dBm\n\tconnected time:\t321 seconds\n";
        let c = parse_stations(t);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].mac, "3c:22:fb:aa:bb:cc");
        assert_eq!(c[0].rx_bytes, Some(12345));
        assert_eq!(c[0].signal_dbm, Some(-45.0));
        assert_eq!(c[0].connected_secs, Some(321));
    }

    #[test]
    fn ipv4_json() {
        let j = r#"[{"ifindex":5,"ifname":"wwan0","addr_info":[{"family":"inet","local":"10.38.118.144","prefixlen":27}]}]"#;
        assert_eq!(first_ipv4(j).as_deref(), Some("10.38.118.144/27"));
    }

    #[test]
    fn packet_stats() {
        let t = "\tTX bytes OK: 1534000\n\tRX bytes OK: 1899431\n";
        assert_eq!(qv_plain(t, "RX bytes OK:"), Some(1899431));
    }
}
