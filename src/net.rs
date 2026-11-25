// SPDX-License-Identifier: EUPL-1.2
//! Network tab extras: interfaces, routes, DNS, WireGuard, Tailscale exit nodes,
//! connectivity test.

use crate::cmd;
use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone, Serialize)]
pub struct Net {
    pub interfaces: Vec<Iface>,
    pub routes: Vec<Route>,
    pub dns: Vec<String>,
    pub wireguard: Vec<Wg>,
    // admin only
    pub exit_nodes: Vec<ExitNode>,
    pub exit_node: Option<String>,
    pub accept_routes: Option<bool>,
    pub peers: Vec<Peer>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Iface {
    pub name: String,
    pub state: String,
    pub mac: Option<String>,
    pub addrs: Vec<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// counters are known to stay at 0 on this interface (wwan0 / bam-dmux)
    pub counters_broken: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Route {
    pub dst: String,
    pub gateway: Option<String>,
    pub dev: Option<String>,
    pub metric: Option<u64>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Wg {
    pub name: String,
    pub active: bool,
    pub autoconnect: bool,
    pub public_key: Option<String>,
    pub listen_port: Option<u64>,
    pub peers: Vec<WgPeer>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct WgPeer {
    /// dongie's label (connection user data), "" if none
    pub name: String,
    pub public_key: String,
    pub endpoint: Option<String>,
    pub allowed_ips: String,
    pub latest_handshake: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ExitNode {
    pub name: String,
    pub ip: String,
    pub online: bool,
    pub active: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Peer {
    pub name: String,
    pub ip: Option<String>,
    pub os: Option<String>,
    pub online: bool,
    pub relay: Option<String>,
    pub direct: Option<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub exit_node_option: bool,
}

impl Net {
    pub fn for_guest(&mut self) {
        self.exit_nodes.clear();
        self.peers.clear();
        self.exit_node = None;
        for w in self.wireguard.iter_mut() {
            w.peers.iter_mut().for_each(|p| p.endpoint = None);
        }
        for i in self.interfaces.iter_mut() {
            i.mac = None;
        }
    }
}

fn read_u64(p: &str) -> u64 {
    std::fs::read_to_string(p).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

async fn interfaces() -> Vec<Iface> {
    let Some(j) = cmd::run("ip", &["-j", "addr"]).await else { return vec![] };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&j) else { return vec![] };
    let mut out = vec![];
    for i in v.as_array().into_iter().flatten() {
        let name = i["ifname"].as_str().unwrap_or("").to_string();
        let state = i["operstate"].as_str().unwrap_or("?").to_string();
        // the modem exposes wwan1..7 mux devices that are never used
        if name == "lo" || (name.starts_with("wwan") && name != "wwan0" && state == "DOWN") {
            continue;
        }
        let addrs = i["addr_info"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["scope"].as_str() != Some("link"))
            .filter_map(|a| Some(format!("{}/{}", a["local"].as_str()?, a["prefixlen"].as_u64()?)))
            .collect();
        let base = format!("/sys/class/net/{name}/statistics");
        out.push(Iface {
            mac: i["address"].as_str().filter(|m| m.len() == 17 && *m != "00:00:00:00:00:00").map(String::from),
            addrs,
            rx_bytes: read_u64(&format!("{base}/rx_bytes")),
            tx_bytes: read_u64(&format!("{base}/tx_bytes")),
            counters_broken: name == "wwan0",
            name,
            state,
        });
    }
    out
}

async fn routes() -> Vec<Route> {
    let Some(j) = cmd::run("ip", &["-j", "route"]).await else { return vec![] };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&j) else { return vec![] };
    v.as_array()
        .into_iter()
        .flatten()
        .map(|r| Route {
            dst: r["dst"].as_str().unwrap_or("?").to_string(),
            gateway: r["gateway"].as_str().map(String::from),
            dev: r["dev"].as_str().map(String::from),
            metric: r["metric"].as_u64(),
        })
        .collect()
}

fn dns() -> Vec<String> {
    std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("nameserver").map(|x| x.trim().to_string()))
        .collect()
}

/// NetworkManager WireGuard profiles + live state from `wg show all dump` (private keys dropped).
async fn wireguard() -> Vec<Wg> {
    let (list, dump) = tokio::join!(
        cmd::run("nmcli", &["-t", "-f", "NAME,TYPE,ACTIVE,AUTOCONNECT", "con", "show"]),
        cmd::sudo(&["/usr/bin/wg", "show", "all", "dump"]),
    );
    let mut out: Vec<Wg> = vec![];
    for line in list.unwrap_or_default().lines() {
        let f = crate::wifi::split_t(line);
        if f.len() >= 4 && f[1] == "wireguard" {
            out.push(Wg {
                name: f[0].clone(),
                active: f[2] == "yes",
                autoconnect: f[3] == "yes",
                ..Default::default()
            });
        }
    }
    for line in dump.unwrap_or_default().lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let Some(w) = out.iter_mut().find(|w| f.first() == Some(&w.name.as_str())) else { continue };
        match f.len() {
            // interface: name, private-key, public-key, listen-port, fwmark
            5 => {
                w.public_key = Some(f[2].to_string());
                w.listen_port = f[3].parse().ok();
            }
            // peer: name, public-key, preshared-key, endpoint, allowed-ips, handshake, rx, tx, keepalive
            9 => w.peers.push(WgPeer {
                name: String::new(),
                public_key: f[1].to_string(),
                endpoint: (f[3] != "(none)").then(|| f[3].to_string()),
                allowed_ips: f[4].to_string(),
                latest_handshake: f[5].parse().ok().filter(|x| *x > 0),
                rx_bytes: f[6].parse().unwrap_or(0),
                tx_bytes: f[7].parse().unwrap_or(0),
            }),
            _ => {}
        }
    }
    for w in out.iter_mut() {
        let names = crate::wgedit::peer_names(&w.name).await;
        for p in w.peers.iter_mut() {
            if let Some(n) = names.get(&p.public_key) {
                p.name = n.clone();
            }
        }
    }
    out
}

/// `collect()` shared by all open tabs for a few seconds (it runs a dozen
/// commands); `fresh` skips the cache, e.g. right after a change.
pub async fn collect_cached(fresh: bool) -> Net {
    static CACHE: tokio::sync::Mutex<Option<(std::time::Instant, Net)>> = tokio::sync::Mutex::const_new(None);
    let mut c = CACHE.lock().await; // also makes concurrent tabs wait for one collect instead of running their own
    if !fresh {
        if let Some((at, n)) = c.as_ref() {
            if at.elapsed() < std::time::Duration::from_secs(8) {
                return n.clone();
            }
        }
    }
    let n = collect().await;
    *c = Some((std::time::Instant::now(), n.clone()));
    n
}

pub async fn collect() -> Net {
    let (interfaces, routes, wireguard, ts, prefs) = tokio::join!(
        interfaces(),
        routes(),
        wireguard(),
        cmd::tailscale("status", &["status", "--json"]),
        cmd::tailscale("prefs", &["debug", "prefs"]),
    );
    let mut n = Net { interfaces, routes, wireguard, dns: dns(), ..Default::default() };
    if let Some(v) = ts.and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok()) {
        for p in v["Peer"].as_object().into_iter().flat_map(|m| m.values()) {
            let name = p["HostName"].as_str().unwrap_or("?").to_string();
            let ip = p["TailscaleIPs"][0].as_str().map(String::from);
            let online = p["Online"].as_bool() == Some(true);
            if p["ExitNodeOption"].as_bool() == Some(true) {
                n.exit_nodes.push(ExitNode {
                    name: name.clone(),
                    ip: ip.clone().unwrap_or_default(),
                    online,
                    active: p["ExitNode"].as_bool() == Some(true),
                });
            }
            if p["ExitNode"].as_bool() == Some(true) {
                n.exit_node = Some(name.clone());
            }
            n.peers.push(Peer {
                name,
                ip,
                os: p["OS"].as_str().map(String::from),
                online,
                relay: p["Relay"].as_str().filter(|s| !s.is_empty()).map(String::from),
                direct: p["CurAddr"].as_str().filter(|s| !s.is_empty()).map(String::from),
                rx_bytes: p["RxBytes"].as_u64().unwrap_or(0),
                tx_bytes: p["TxBytes"].as_u64().unwrap_or(0),
                exit_node_option: p["ExitNodeOption"].as_bool() == Some(true),
            });
        }
        n.exit_nodes.sort_by(|a, b| b.online.cmp(&a.online).then(a.name.cmp(&b.name)));
        n.peers.sort_by(|a, b| b.online.cmp(&a.online).then(a.name.cmp(&b.name)));
    }
    if let Some(v) = prefs.and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok()) {
        n.accept_routes = v["RouteAll"].as_bool();
    }
    n
}

// ---------------- actions (admin) ----------------

async fn run_checked(prog: &str, args: &[&str], secs: u64) -> Result<String, String> {
    match cmd::run_any_t(prog, args, secs).await {
        Some((true, out, _)) => Ok(out),
        Some((false, out, err)) => Err(format!("{}{}", err.trim(), out.trim()).trim().to_string()),
        None => Err(format!("{prog} timed out")),
    }
}

pub fn valid_wg_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 15
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// Import a wg-quick style config as a NetworkManager connection.
/// Defaults are the safe ones: no default route through the tunnel, no autostart.
pub async fn wg_import(name: &str, config: &str, route_all: bool, autoconnect: bool) -> Result<String, String> {
    if !valid_wg_name(name) {
        return Err("name: 1-15 chars, a-z 0-9 - _, starting with a letter".into());
    }
    if config.len() > 16_000
        || !config.contains("[Interface]")
        || !config.contains("PrivateKey")
        || !config.contains("[Peer]")
    {
        return Err("doesn't look like a WireGuard config ([Interface] with PrivateKey + [Peer])".into());
    }
    if wireguard().await.iter().any(|w| w.name == name) {
        return Err(format!("a WireGuard connection named {name} already exists"));
    }
    // nmcli names the connection + interface after the file name
    let mut rnd = [0u8; 8];
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    OsRng.fill_bytes(&mut rnd);
    let dir =
        std::env::temp_dir().join(format!("dongie-wg-{}", rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    let path = dir.join(format!("{name}.conf"));
    let res = async {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| e.to_string())?;
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| e.to_string())?;
            f.write_all(config.as_bytes()).map_err(|e| e.to_string())?;
        }
        let p = path.to_string_lossy().to_string();
        run_checked("nmcli", &["connection", "import", "type", "wireguard", "file", &p], 30).await
    }
    .await;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    res?;
    let nd = if route_all { "no" } else { "yes" };
    let ac = if autoconnect { "yes" } else { "no" };
    run_checked(
        "nmcli",
        &["con", "modify", name, "ipv4.never-default", nd, "ipv6.never-default", nd, "connection.autoconnect", ac],
        20,
    )
    .await?;
    // import activates it immediately; respect the autoconnect choice
    if !autoconnect {
        let _ = run_checked("nmcli", &["con", "down", name], 20).await;
    }
    Ok(format!(
        "imported {name} ({}, {})",
        if route_all { "routes all traffic" } else { "only its AllowedIPs" },
        if autoconnect { "starts at boot" } else { "manual start" }
    ))
}

async fn require_wg(name: &str) -> Result<(), String> {
    if wireguard().await.iter().any(|w| w.name == name) {
        Ok(())
    } else {
        Err("no such WireGuard connection".into())
    }
}

pub async fn wg_action(name: &str, action: &str) -> Result<String, String> {
    require_wg(name).await?;
    match action {
        "up" => run_checked("nmcli", &["--wait", "20", "con", "up", name], 30).await.map(|_| format!("{name} up")),
        "down" => run_checked("nmcli", &["con", "down", name], 20).await.map(|_| format!("{name} down")),
        "delete" => run_checked("nmcli", &["con", "delete", name], 20).await.map(|_| format!("{name} deleted")),
        "autoconnect-on" | "autoconnect-off" => {
            let v = if action == "autoconnect-on" { "yes" } else { "no" };
            run_checked("nmcli", &["con", "modify", name, "connection.autoconnect", v], 20)
                .await
                .map(|_| format!("{name}: start at boot {v}"))
        }
        _ => Err("unknown action".into()),
    }
}

// ---------------- connectivity test ----------------

#[derive(Debug, Default, Serialize)]
pub struct TestResult {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub ms: Option<f64>,
}

async fn ping(target: &str) -> TestResult {
    let mut r = TestResult { name: format!("ping {target}"), ..Default::default() };
    match cmd::run_any_t("ping", &["-c", "4", "-i", "0.3", "-W", "2", "-q", target], 15).await {
        Some((_, out, err)) => {
            let loss = out.lines().find(|l| l.contains("packet loss")).unwrap_or("").to_string();
            let rtt = out.lines().find(|l| l.contains("min/avg/max"));
            if let Some(l) = rtt {
                let nums: Vec<f64> = l.split('=').nth(1).unwrap_or("").split('/').filter_map(cmd::num).collect();
                if nums.len() >= 3 {
                    r.ms = Some(nums[1]);
                    r.detail =
                        format!("{} · min {:.0} / avg {:.0} / max {:.0} ms", loss.trim(), nums[0], nums[1], nums[2]);
                }
            }
            r.ok = rtt.is_some();
            if r.detail.is_empty() {
                r.detail = format!("{} {}", loss.trim(), err.trim()).trim().to_string();
            }
        }
        None => r.detail = "timed out".into(),
    }
    r
}

async fn timed<F: std::future::Future<Output = Result<String, String>>>(name: &str, f: F) -> TestResult {
    let t = Instant::now();
    match tokio::time::timeout(Duration::from_secs(10), f).await {
        Ok(Ok(d)) => {
            TestResult { name: name.into(), ok: true, detail: d, ms: Some(t.elapsed().as_secs_f64() * 1000.0) }
        }
        Ok(Err(e)) => TestResult { name: name.into(), ok: false, detail: e, ms: None },
        Err(_) => TestResult { name: name.into(), ok: false, detail: "timed out after 10 s".into(), ms: None },
    }
}

pub async fn connectivity_test() -> Vec<TestResult> {
    let (p1, p2, d, t) = tokio::join!(
        ping("1.1.1.1"),
        ping("8.8.8.8"),
        timed("DNS lookup example.com", async {
            let a: Vec<_> = tokio::net::lookup_host("example.com:443").await.map_err(|e| e.to_string())?.collect();
            Ok(a.first().map(|x| x.ip().to_string()).unwrap_or_default())
        }),
        timed("TCP connect 1.1.1.1:443", async {
            tokio::net::TcpStream::connect("1.1.1.1:443")
                .await
                .map(|_| "connected".to_string())
                .map_err(|e| e.to_string())
        }),
    );
    vec![p1, p2, d, t]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wg_names() {
        assert!(valid_wg_name("wg0") && valid_wg_name("home-vpn"));
        assert!(
            !valid_wg_name("") && !valid_wg_name("0wg") && !valid_wg_name("Wg") && !valid_wg_name("a-very-long-name-x")
        );
        assert!(!valid_wg_name("wg0;rm"));
    }
}
