// SPDX-License-Identifier: EUPL-1.2
//! Wi-Fi: hotspot (AP) or client (station) mode — status + admin actions.
//!
//! The WCN36xx radio does ONE mode at a time ("interface combinations are not
//! supported"), so joining a network turns the hotspot off and vice versa.
//! Which one wins at boot is decided by NetworkManager autoconnect priorities:
//! hotspot = 100, the preferred client network = 110, other clients = 0.
//! If a client connection fails we bring the hotspot back, so the dongle never
//! ends up without Wi-Fi.

use crate::cmd;
use serde::Serialize;

const IFACE: &str = "wlan0";
const PRIO_PREFERRED_CLIENT: &str = "110";
const PRIO_OTHER_CLIENT: &str = "0";

#[derive(Debug, Default, Clone, Serialize)]
pub struct Wifi {
    /// "hotspot" | "client" | "off"
    pub mode: String,
    pub connection: Option<String>,
    pub station: Option<Station>,
    /// configured hotspot SSID / channel (also when the hotspot is off)
    pub hotspot_ssid: Option<String>,
    pub hotspot_channel: Option<String>,
    // admin only
    pub saved: Vec<Saved>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Station {
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub signal_dbm: Option<f64>,
    /// NetworkManager's 0-100 % (always there; dBm needs `iw`)
    pub signal_pct: Option<u8>,
    pub freq_mhz: Option<f64>,
    pub rx_bitrate: Option<String>,
    pub tx_bitrate: Option<String>,
    pub ip: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Saved {
    pub name: String,
    pub ssid: Option<String>,
    /// "ap" | "infrastructure"
    pub mode: Option<String>,
    pub autoconnect: bool,
    pub priority: i32,
    pub active: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ScanResult {
    pub in_use: bool,
    pub ssid: String,
    pub bssid: String,
    pub channel: Option<u32>,
    pub freq_mhz: Option<u32>,
    pub signal: Option<u32>,
    pub security: String,
    /// number of access points seen for this SSID
    pub ap_count: u32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Scan {
    pub networks: Vec<ScanResult>,
    /// (channel, number of APs, strongest signal %) for 2.4 GHz channels 1-13
    pub channels: Vec<(u32, u32, u32)>,
    pub hotspot_paused: bool,
    pub took_ms: u64,
}

impl Wifi {
    pub fn for_guest(&mut self) {
        self.saved.clear();
        if let Some(s) = self.station.as_mut() {
            s.bssid = None; // BSSIDs can be looked up in wardriving databases
        }
    }
}

/// Split one line of `nmcli -t` output (fields separated by ':', literal ':' and '\' escaped).
pub fn split_t(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut esc = false;
    for c in line.chars() {
        if esc {
            out.last_mut().unwrap().push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == ':' {
            out.push(String::new());
        } else {
            out.last_mut().unwrap().push(c);
        }
    }
    out
}

fn parse_link(t: &str) -> Option<Station> {
    if !t.starts_with("Connected to ") {
        return None;
    }
    let mut s = Station { bssid: t.split_whitespace().nth(2).map(String::from), ..Default::default() };
    for line in t.lines().skip(1) {
        if let Some((k, v)) = line.trim().split_once(':') {
            let v = v.trim();
            match k {
                "SSID" => s.ssid = Some(v.to_string()),
                "freq" => s.freq_mhz = cmd::num(v),
                "signal" => s.signal_dbm = cmd::num(v),
                "rx bitrate" => s.rx_bitrate = Some(v.to_string()),
                "tx bitrate" => s.tx_bitrate = Some(v.to_string()),
                _ => {}
            }
        }
    }
    Some(s)
}

/// `iw` values win (they are more exact); NetworkManager fills the gaps.
fn merge_station(iw: Option<Station>, nm: Option<crate::nm::WifiLink>, conn_ssid: Option<String>) -> Station {
    let mut st = iw.unwrap_or_default();
    if let Some(l) = nm {
        st.ssid = st.ssid.or(l.ssid);
        st.bssid = st.bssid.or(l.bssid);
        st.freq_mhz = st.freq_mhz.or(l.freq_mhz.map(f64::from));
        st.signal_pct = l.strength;
        if st.tx_bitrate.is_none() {
            st.tx_bitrate = l.bitrate_kbps.map(|k| format!("{:.1} MBit/s", k as f64 / 1000.0));
        }
    }
    st.ssid = st.ssid.or(conn_ssid);
    st
}

async fn saved_wifi() -> Vec<Saved> {
    crate::nm::shared().await.map(|s| saved_from(&s)).unwrap_or_default()
}

fn saved_from(s: &crate::nm::Snapshot) -> Vec<Saved> {
    s.conns
        .iter()
        .filter(|c| c.kind == "802-11-wireless")
        .map(|c| Saved {
            name: c.id.clone(),
            ssid: c.ssid(),
            mode: c.str("802-11-wireless", "mode").filter(|m| !m.is_empty()),
            autoconnect: c.autoconnect(),
            priority: c.i32("connection", "autoconnect-priority").unwrap_or(0),
            active: c.activated(),
        })
        .collect()
}

pub async fn collect(hotspot_con: &str) -> Wifi {
    let mut w = Wifi { mode: "off".into(), ..Default::default() };
    let (nm, link) = tokio::join!(crate::nm::shared(), cmd::run("/usr/sbin/iw", &["dev", IFACE, "link"]));
    let saved = nm.as_ref().map(saved_from).unwrap_or_default();
    w.connection = nm.as_ref().and_then(|s| s.on_device(IFACE)).map(|c| c.id.clone());
    if let Some(c) = &w.connection {
        let m = saved.iter().find(|s| &s.name == c).and_then(|s| s.mode.clone());
        w.mode = match m.as_deref() {
            Some("ap") => "hotspot",
            Some(_) => "client",
            None => "off",
        }
        .into();
    }
    if w.mode == "client" {
        // `iw` (dBm, rx/tx rates) when installed, NetworkManager for the rest:
        // client mode always gets a station, so the UI can show the network
        let (nl, ip) =
            tokio::join!(crate::nm::wifi_link(IFACE), cmd::run("ip", &["-j", "-4", "addr", "show", "dev", IFACE]));
        let conn_ssid = nm.as_ref().and_then(|s| s.on_device(IFACE)).and_then(|c| c.ssid());
        let mut st = merge_station(link.as_deref().and_then(parse_link), nl, conn_ssid);
        st.ip = ip.and_then(|j| crate::status::first_ipv4(&j));
        w.station = Some(st);
    }
    w.hotspot_ssid = saved.iter().find(|s| s.name == hotspot_con).and_then(|s| s.ssid.clone());
    // nmcli printed 0 for "automatic" too
    w.hotspot_channel = nm
        .as_ref()
        .and_then(|s| s.by_id(hotspot_con))
        .map(|c| c.u32("802-11-wireless", "channel").unwrap_or(0).to_string());
    w.saved = saved;
    w
}

// ---------------- actions (admin) ----------------

fn valid_ssid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && !s.chars().any(|c| c.is_control())
}

fn valid_psk(p: &str) -> bool {
    (8..=63).contains(&p.len()) && p.chars().all(|c| (' '..='~').contains(&c))
}

async fn nmcli(args: &[&str], secs: u64) -> Result<String, String> {
    match cmd::run_any_t("nmcli", args, secs).await {
        Some((true, out, _)) => Ok(out),
        Some((false, out, err)) => Err(format!("{}{}", err.trim(), out.trim()).trim().to_string()),
        None => Err("nmcli timed out".into()),
    }
}

async fn active_connection() -> Option<String> {
    cmd::run("nmcli", &["-t", "-f", "GENERAL.CONNECTION", "dev", "show", IFACE])
        .await
        .and_then(|t| t.lines().next().and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string()))
        .filter(|v| !v.is_empty() && v != "--")
}

/// Change hotspot SSID / password / channel. Re-activates it if it is running.
pub async fn set_hotspot(
    con: &str,
    ssid: Option<String>,
    password: Option<String>,
    channel: Option<u32>,
) -> Result<String, String> {
    let mut args: Vec<String> = vec!["con".into(), "modify".into(), con.into()];
    if let Some(s) = ssid.filter(|s| !s.is_empty()) {
        if !valid_ssid(&s) {
            return Err("SSID must be 1-32 bytes".into());
        }
        args.extend(["802-11-wireless.ssid".into(), s]);
    }
    if let Some(p) = password.filter(|p| !p.is_empty()) {
        if !valid_psk(&p) {
            return Err("password must be 8-63 printable ASCII characters".into());
        }
        args.extend(["wifi-sec.key-mgmt".into(), "wpa-psk".into(), "wifi-sec.psk".into(), p]);
    }
    if let Some(c) = channel {
        if c > 13 {
            return Err("channel must be 0 (auto) or 1-13".into());
        }
        // nmcli rejects "0"; an empty value resets the channel to automatic
        let ch = if c == 0 { String::new() } else { c.to_string() };
        args.extend(["802-11-wireless.band".into(), "bg".into(), "802-11-wireless.channel".into(), ch]);
    }
    if args.len() == 3 {
        return Err("nothing to change".into());
    }
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    nmcli(&a, 20).await?;
    if active_connection().await.as_deref() == Some(con) {
        nmcli(&["--wait", "40", "con", "up", con], 50).await?;
        return Ok("saved, hotspot restarted".into());
    }
    Ok("saved".into())
}

async fn scan_raw() -> Result<Vec<ScanResult>, String> {
    let t = nmcli(
        &[
            "-t",
            "-f",
            "IN-USE,SSID,BSSID,CHAN,FREQ,SIGNAL,SECURITY",
            "dev",
            "wifi",
            "list",
            "ifname",
            IFACE,
            "--rescan",
            "yes",
        ],
        40,
    )
    .await?;
    Ok(t.lines()
        .filter_map(|l| {
            let f = split_t(l);
            (f.len() >= 7).then(|| ScanResult {
                in_use: f[0] == "*",
                ssid: f[1].clone(),
                bssid: f[2].clone(),
                channel: f[3].parse().ok(),
                freq_mhz: cmd::num(&f[4]).map(|x| x as u32),
                signal: f[5].parse().ok(),
                security: f[6].clone(),
                ap_count: 1,
            })
        })
        .collect())
}

/// Aggregate raw results: one entry per SSID (strongest AP), plus channel usage.
pub fn summarize(raw: Vec<ScanResult>) -> (Vec<ScanResult>, Vec<(u32, u32, u32)>) {
    let mut ch: Vec<(u32, u32, u32)> = (1..=13).map(|c| (c, 0, 0)).collect();
    for r in &raw {
        if let Some(c) = r.channel.filter(|c| (1..=13).contains(c)) {
            let e = &mut ch[(c - 1) as usize];
            e.1 += 1;
            e.2 = e.2.max(r.signal.unwrap_or(0));
        }
    }
    let mut by: Vec<ScanResult> = vec![];
    for r in raw.into_iter().filter(|r| !r.ssid.is_empty()) {
        match by.iter_mut().find(|x| x.ssid == r.ssid) {
            Some(x) => {
                x.ap_count += 1;
                x.in_use |= r.in_use;
                if r.signal > x.signal {
                    let (n, u) = (x.ap_count, x.in_use);
                    *x = r;
                    x.ap_count = n;
                    x.in_use = u;
                }
            }
            None => by.push(r),
        }
    }
    by.sort_by_key(|a| std::cmp::Reverse(a.signal));
    (by, ch)
}

/// Scan. The radio can't scan while it is an AP ("Operation not supported"), so with
/// `pause_hotspot` the hotspot goes down for the scan and ALWAYS comes back afterwards.
pub async fn scan(hotspot_con: &str, pause_hotspot: bool) -> Result<Scan, String> {
    let t0 = std::time::Instant::now();
    let paused = pause_hotspot && active_connection().await.as_deref() == Some(hotspot_con);
    if paused {
        nmcli(&["con", "down", hotspot_con], 20).await?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let mut raw = scan_raw().await;
    if paused && raw.as_ref().map(|v| v.is_empty()).unwrap_or(true) {
        // first scan after the mode switch is often empty: give the radio a moment
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        raw = scan_raw().await;
    }
    if paused {
        if let Err(e) = nmcli(&["--wait", "40", "con", "up", hotspot_con], 50).await {
            return Err(format!("scan done but the hotspot did not come back: {e}"));
        }
    }
    let (networks, channels) = summarize(raw?);
    Ok(Scan { networks, channels, hotspot_paused: paused, took_ms: t0.elapsed().as_millis() as u64 })
}

async fn set_priorities(preferred: Option<&str>, hotspot_con: &str) {
    for s in saved_wifi().await {
        if s.name == hotspot_con || s.mode.as_deref() == Some("ap") {
            continue;
        }
        let p = if Some(s.name.as_str()) == preferred { PRIO_PREFERRED_CLIENT } else { PRIO_OTHER_CLIENT };
        if s.priority.to_string() != p {
            let _ = nmcli(&["con", "modify", &s.name, "connection.autoconnect-priority", p], 20).await;
        }
    }
}

/// Activate a saved connection (hotspot or client). Client failure => hotspot back.
pub async fn activate(name: &str, hotspot_con: &str) -> Result<String, String> {
    let saved = saved_wifi().await;
    let Some(s) = saved.iter().find(|s| s.name == name) else {
        return Err("no such Wi-Fi connection".into());
    };
    if s.name == hotspot_con || s.mode.as_deref() == Some("ap") {
        set_priorities(None, hotspot_con).await;
        nmcli(&["--wait", "40", "con", "up", name], 50).await?;
        return Ok("hotspot on (preferred at boot)".into());
    }
    set_priorities(Some(name), hotspot_con).await;
    match nmcli(&["--wait", "45", "con", "up", name], 55).await {
        Ok(_) => Ok(format!("connected to {} (preferred at boot)", s.ssid.as_deref().unwrap_or(name))),
        Err(e) => {
            set_priorities(None, hotspot_con).await;
            let back = nmcli(&["--wait", "40", "con", "up", hotspot_con], 50).await;
            Err(format!(
                "connecting failed: {e} — {}",
                if back.is_ok() { "hotspot restored" } else { "and the hotspot did not come back!" }
            ))
        }
    }
}

/// Save a new client network (or update its password) and connect to it.
pub async fn connect_new(ssid: &str, password: Option<&str>, hotspot_con: &str) -> Result<String, String> {
    if !valid_ssid(ssid) {
        return Err("SSID must be 1-32 bytes".into());
    }
    let pw = password.filter(|p| !p.is_empty());
    if let Some(p) = pw {
        if !valid_psk(p) {
            return Err("password must be 8-63 printable ASCII characters".into());
        }
    }
    let safe: String =
        ssid.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let name = format!("wifi-{safe}");
    if name == hotspot_con {
        return Err("name clash with the hotspot connection".into());
    }
    let exists = saved_wifi().await.iter().any(|s| s.name == name);
    let mut args: Vec<&str> = if exists {
        vec!["con", "modify", &name, "802-11-wireless.ssid", ssid]
    } else {
        vec![
            "con",
            "add",
            "type",
            "wifi",
            "ifname",
            IFACE,
            "con-name",
            &name,
            "ssid",
            ssid,
            "connection.autoconnect",
            "yes",
        ]
    };
    if let Some(p) = pw {
        args.extend(["wifi-sec.key-mgmt", "wpa-psk", "wifi-sec.psk", p]);
    }
    nmcli(&args, 20).await?;
    activate(&name, hotspot_con).await
}

pub async fn forget(name: &str, hotspot_con: &str) -> Result<String, String> {
    let saved = saved_wifi().await;
    let Some(s) = saved.iter().find(|s| s.name == name) else {
        return Err("no such Wi-Fi connection".into());
    };
    if s.name == hotspot_con || s.mode.as_deref() == Some("ap") {
        return Err("the hotspot can't be deleted here".into());
    }
    let was_active = s.active;
    nmcli(&["con", "delete", name], 20).await?;
    if was_active {
        let _ = nmcli(&["--wait", "40", "con", "up", hotspot_con], 50).await;
        return Ok("forgotten, hotspot back on".into());
    }
    Ok("forgotten".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn station_without_iw_comes_from_networkmanager() {
        let nm = crate::nm::WifiLink {
            ssid: Some("HomeNet_5A2B".into()),
            bssid: Some("D2:6E:0E:36:2A:84".into()),
            freq_mhz: Some(2462),
            strength: Some(83),
            bitrate_kbps: Some(65000),
        };
        let st = super::merge_station(None, Some(nm.clone()), None);
        assert_eq!(st.ssid.as_deref(), Some("HomeNet_5A2B"));
        assert_eq!(st.freq_mhz, Some(2462.0));
        assert_eq!(st.signal_pct, Some(83));
        assert_eq!(st.signal_dbm, None);
        assert_eq!(st.tx_bitrate.as_deref(), Some("65.0 MBit/s"));
        // iw wins where it has values
        let iw =
            super::Station { signal_dbm: Some(-42.0), tx_bitrate: Some("13.0 MBit/s".into()), ..Default::default() };
        let st = super::merge_station(Some(iw), Some(nm), None);
        assert_eq!(st.signal_dbm, Some(-42.0));
        assert_eq!(st.tx_bitrate.as_deref(), Some("13.0 MBit/s"));
        // nothing from NM either: the profile's SSID at least
        let st = super::merge_station(None, None, Some("x".into()));
        assert_eq!(st.ssid.as_deref(), Some("x"));
    }

    use super::*;

    #[test]
    fn nmcli_terse_escapes() {
        assert_eq!(
            split_t(r"*:Home\:Net:AA\:BB\:CC\:DD\:EE\:FF:6:72:WPA2"),
            vec!["*", "Home:Net", "AA:BB:CC:DD:EE:FF", "6", "72", "WPA2"]
        );
        assert_eq!(split_t("hotspot:802-11-wireless:yes:100:yes").len(), 5);
    }

    #[test]
    fn iw_link() {
        assert!(parse_link("Not connected.\n").is_none());
        let s = parse_link("Connected to 3c:22:fb:11:22:33 (on wlan0)\n\tSSID: Gondor\n\tfreq: 2437\n\tsignal: -48 dBm\n\trx bitrate: 65.0 MBit/s MCS 7\n\ttx bitrate: 72.2 MBit/s MCS 7 short GI\n").unwrap();
        assert_eq!(s.ssid.as_deref(), Some("Gondor"));
        assert_eq!(s.bssid.as_deref(), Some("3c:22:fb:11:22:33"));
        assert_eq!(s.signal_dbm, Some(-48.0));
        assert_eq!(s.freq_mhz, Some(2437.0));
    }

    #[test]
    fn scan_summary() {
        let mk = |ssid: &str, ch: u32, sig: u32| ScanResult {
            ssid: ssid.into(),
            channel: Some(ch),
            signal: Some(sig),
            ap_count: 1,
            ..Default::default()
        };
        let (n, c) = summarize(vec![mk("A", 1, 40), mk("A", 6, 70), mk("B", 6, 50), mk("", 11, 20)]);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].ssid, "A");
        assert_eq!(n[0].ap_count, 2);
        assert_eq!(n[0].signal, Some(70));
        assert_eq!(n[0].channel, Some(6));
        assert_eq!(c[5], (6, 2, 70));
        assert_eq!(c[10], (11, 1, 20));
    }

    #[test]
    fn validation() {
        assert!(valid_psk("12345678") && !valid_psk("short") && !valid_psk("ümlaut-pass"));
        assert!(valid_ssid("MyStick") && !valid_ssid("") && !valid_ssid(&"x".repeat(33)));
    }
}
