// SPDX-License-Identifier: EUPL-1.2
//! Firewall: zones (by interface) × services, plus custom rules, turned into
//! one nftables table (`inet dongie`).
//!
//! - `<state_dir>/firewall.toml` holds the configuration (dongie writes it, it
//!   can also be edited by hand); without it the built-in defaults apply.
//! - `dongie firewall-apply` (root, dongie-firewall.service, at boot before the
//!   network and when the UI changes something) validates it, generates the
//!   ruleset and loads it in one atomic `nft -f`.
//! - Changes from the UI are provisional: the new file is `firewall.pending`
//!   with an expiry in `firewall.try`; the helper applies it only while the
//!   expiry is ahead. Confirm within CONFIRM_SECS or dongie rolls back (also
//!   after a dongie restart or a reboot: an expired try is ignored).
//! - `firewall = true` in config.toml turns it on (default off, so existing
//!   installs don't change by themselves).
//!
//! Everything that ends up in the ruleset is validated against a strict
//! character set first, so the file can't inject nft syntax.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

pub const UNIT: &str = "dongie-firewall.service";
pub const CONFIRM_SECS: u64 = 120;
const TABLE: &str = "dongie";
const STATUS: &str = "/run/dongie-firewall/status.json";

/// The zones, in matching order (an interface belongs to the first that lists it).
pub const ZONES: &[&str] = &["bridge", "tailscale", "wireguard", "wifi", "lte"];

/// Services that can be allowed per zone.
pub const SERVICES: &[(&str, &str)] = &[
    ("ssh", "SSH (22/tcp)"),
    ("web", "dongie web UI (its ports, 443 for tailscale serve)"),
    ("api", "dongie open API (api_listen port)"),
    ("calls", "browser calls (dongie-ims WebRTC, 40100-40199/udp)"),
    ("dns", "DNS + DHCP for clients (53, 67/udp, 547/udp)"),
    ("ping", "ping (ICMP echo)"),
    ("mesh", "muxtastic (4404/tcp apps, 8090/tcp web)"),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub zone: String,
    /// tcp | udp
    pub proto: String,
    /// "8080" or "8000-8100"
    pub ports: String,
    /// optional source network (CIDR or address)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FwConfig {
    /// interfaces per zone (shell-style `*` at the end allowed: "wwan*")
    #[serde(default = "default_zones")]
    pub zones: BTreeMap<String, Vec<String>>,
    /// services allowed per zone
    #[serde(default = "default_allow")]
    pub allow: BTreeMap<String, Vec<String>>,
    /// zones whose clients may be forwarded (out to the internet via NAT)
    #[serde(default = "default_forward")]
    pub forward: Vec<String>,
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn default_zones() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([
        ("bridge".into(), s(&["br0", "usb*"])),
        ("tailscale".into(), s(&["tailscale0"])),
        ("wireguard".into(), s(&["wg*", "mainwg"])),
        ("wifi".into(), s(&["wlan*"])),
        ("lte".into(), s(&["wwan*", "rmnet*"])),
    ])
}

fn default_allow() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([
        ("bridge".into(), s(&["ssh", "web", "api", "calls", "dns", "ping", "mesh"])),
        ("tailscale".into(), s(&["ssh", "web", "api", "calls", "ping", "mesh"])),
        ("wireguard".into(), s(&["ssh", "web", "api", "calls", "ping"])),
        // SSH from the Wi-Fi the stick joins: allowed; the web UI is opt-in
        ("wifi".into(), s(&["ssh", "ping"])),
        ("lte".into(), vec![]),
    ])
}

fn default_forward() -> Vec<String> {
    s(&["bridge"])
}

impl Default for FwConfig {
    fn default() -> Self {
        FwConfig { zones: default_zones(), allow: default_allow(), forward: default_forward(), rules: vec![] }
    }
}

fn valid_iface(i: &str) -> bool {
    let base = i.strip_suffix('*').unwrap_or(i);
    !base.is_empty() && base.len() <= 15 && base.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn valid_ports(p: &str) -> Option<(u16, u16)> {
    let (a, b) = p.split_once('-').unwrap_or((p, p));
    let (a, b) = (a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?);
    (a >= 1 && a <= b).then_some((a, b))
}

/// "10.0.0.0/8", "fd00::/8" or a plain address; returns (is_v6, normalized text).
fn valid_net(n: &str) -> Option<(bool, String)> {
    let (addr, len) = match n.split_once('/') {
        Some((a, l)) => (a, Some(l.parse::<u8>().ok()?)),
        None => (n, None),
    };
    let ip: IpAddr = addr.trim().parse().ok()?;
    let max = if ip.is_ipv6() { 128 } else { 32 };
    let len = len.unwrap_or(max);
    (len <= max).then(|| (ip.is_ipv6(), format!("{ip}/{len}")))
}

fn valid_comment(c: &str) -> bool {
    c.chars().count() <= 60 && c.chars().all(|c| c.is_alphanumeric() || " .,:_-/()".contains(c))
}

impl FwConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (z, ifs) in &self.zones {
            if !ZONES.contains(&z.as_str()) {
                return Err(format!("unknown zone {z:?}"));
            }
            if let Some(i) = ifs.iter().find(|i| !valid_iface(i)) {
                return Err(format!("zone {z}: bad interface name {i:?}"));
            }
        }
        for (z, svcs) in &self.allow {
            if !ZONES.contains(&z.as_str()) {
                return Err(format!("unknown zone {z:?} in allow"));
            }
            if let Some(x) = svcs.iter().find(|x| !SERVICES.iter().any(|(n, _)| n == x)) {
                return Err(format!("zone {z}: unknown service {x:?}"));
            }
        }
        if let Some(z) = self.forward.iter().find(|z| !ZONES.contains(&z.as_str())) {
            return Err(format!("unknown zone {z:?} in forward"));
        }
        for (n, r) in self.rules.iter().enumerate() {
            let n = n + 1;
            if !ZONES.contains(&r.zone.as_str()) {
                return Err(format!("rule {n}: unknown zone {:?}", r.zone));
            }
            if r.proto != "tcp" && r.proto != "udp" {
                return Err(format!("rule {n}: protocol must be tcp or udp"));
            }
            if valid_ports(&r.ports).is_none() {
                return Err(format!("rule {n}: ports {:?} (e.g. 8080 or 8000-8100)", r.ports));
            }
            if !r.from.is_empty() && valid_net(&r.from).is_none() {
                return Err(format!("rule {n}: source {:?} is not an address or network", r.from));
            }
            if !valid_comment(&r.comment) {
                return Err(format!("rule {n}: comment: up to 60 letters, digits, spaces and .,:_-/()"));
            }
        }
        // lockout guard: dongie's web UI must stay reachable from the bridge or Tailscale
        let web = |z: &str| {
            self.allow.get(z).is_some_and(|v| v.iter().any(|x| x == "web"))
                && self.zones.get(z).is_some_and(|i| !i.is_empty())
        };
        if !web("bridge") && !web("tailscale") {
            return Err(
                "the web UI must stay allowed on the bridge or on Tailscale (or you'd lock yourself out)".into()
            );
        }
        Ok(())
    }
}

/// Ports of the running setup that services refer to.
#[derive(Debug, Default, Clone)]
pub struct Ports {
    pub web: Vec<u16>,
    pub api: Option<u16>,
    /// UDP ports to allow everywhere: Tailscale, WireGuard listen ports
    pub tunnels: Vec<u16>,
}

fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
}

impl Ports {
    pub fn from_config(cfg: &crate::config::Config) -> Ports {
        let mut web: Vec<u16> =
            std::iter::once(&cfg.web_listen).chain(cfg.web_listen_extra.iter()).filter_map(|a| port_of(a)).collect();
        web.push(443); // tailscale serve (HTTPS) ends in tailscaled on tailscale0
        web.sort();
        web.dedup();
        Ports { web, api: cfg.api_enabled.then(|| port_of(&cfg.api_listen)).flatten(), tunnels: vec![41641] }
    }
}

fn iface_set(ifs: &[String]) -> String {
    // nft: iifname { "br0", "usb*" }
    let v: Vec<String> = ifs.iter().map(|i| format!("\"{i}\"")).collect();
    format!("{{ {} }}", v.join(", "))
}

fn port_set(p: &[u16]) -> String {
    let v: Vec<String> = p.iter().map(|x| x.to_string()).collect();
    format!("{{ {} }}", v.join(", "))
}

/// The nftables ruleset (one atomic transaction: create, flush and refill the table).
pub fn ruleset(c: &FwConfig, ports: &Ports) -> String {
    let mut o = String::new();
    o += &format!("table inet {TABLE}\ndelete table inet {TABLE}\ntable inet {TABLE} {{\n");
    // zone chains
    for z in ZONES {
        o += &format!("  chain in_{z} {{\n");
        for svc in c.allow.get(*z).map(|v| v.as_slice()).unwrap_or(&[]) {
            match svc.as_str() {
                "ssh" => o += "    tcp dport 22 accept\n",
                "web" if !ports.web.is_empty() => o += &format!("    tcp dport {} accept\n", port_set(&ports.web)),
                "api" => {
                    if let Some(p) = ports.api {
                        o += &format!("    tcp dport {p} accept\n");
                    }
                }
                "calls" => o += "    udp dport 40100-40199 accept\n",
                "dns" => {
                    o += "    udp dport { 53, 67, 547 } accept\n";
                    o += "    tcp dport 53 accept\n";
                }
                "ping" => {
                    o += "    icmp type echo-request accept\n";
                    o += "    icmpv6 type echo-request accept\n";
                }
                "mesh" => o += "    tcp dport { 4404, 8090 } accept\n",
                _ => {}
            }
        }
        for r in c.rules.iter().filter(|r| r.zone == *z) {
            let (a, b) = valid_ports(&r.ports).unwrap_or((0, 0));
            let dport = if a == b { a.to_string() } else { format!("{a}-{b}") };
            let src = match valid_net(&r.from) {
                Some((true, n)) => format!("ip6 saddr {n} "),
                Some((false, n)) => format!("ip saddr {n} "),
                None => String::new(),
            };
            let comment = if r.comment.is_empty() { String::new() } else { format!(" comment \"{}\"", r.comment) };
            o += &format!("    {src}{} dport {dport} accept{comment}\n", r.proto);
        }
        o += "  }\n";
    }
    // input
    o += "  chain input {\n    type filter hook input priority filter; policy drop;\n";
    o += "    ct state established,related accept\n    ct state invalid drop\n    iif lo accept\n";
    o += "    # needed for IPv6 and path MTU, everywhere\n";
    o += "    icmpv6 type { destination-unreachable, packet-too-big, time-exceeded, parameter-problem, nd-router-solicit, nd-router-advert, nd-neighbor-solicit, nd-neighbor-advert, mld-listener-query, mld-listener-report, mld2-listener-report } accept\n";
    o += "    icmp type { destination-unreachable, time-exceeded, parameter-problem } accept\n";
    o += "    # DHCP / DHCPv6 replies to the stick's own client (Wi-Fi, LTE)\n";
    o += "    udp sport 67 udp dport 68 accept\n    udp sport 547 udp dport 546 accept\n";
    if !ports.tunnels.is_empty() {
        o += &format!("    # Tailscale and WireGuard tunnels\n    udp dport {} accept\n", port_set(&ports.tunnels));
    }
    for z in ZONES {
        if let Some(ifs) = c.zones.get(*z).filter(|v| !v.is_empty()) {
            o += &format!("    iifname {} jump in_{z}\n", iface_set(ifs));
        }
    }
    o += "  }\n";
    // forward
    o += "  chain forward {\n    type filter hook forward priority filter; policy drop;\n";
    o += "    ct state established,related accept\n    ct state invalid drop\n";
    for z in &c.forward {
        if let Some(ifs) = c.zones.get(z).filter(|v| !v.is_empty()) {
            o += &format!("    iifname {} accept\n", iface_set(ifs));
        }
    }
    o += "  }\n}\n";
    o
}

// ---------------- files ----------------

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

pub fn load(dir: &Path) -> Result<FwConfig, String> {
    load_file(&path(dir, "firewall.toml"))
}

fn load_file(p: &Path) -> Result<FwConfig, String> {
    match std::fs::read_to_string(p) {
        Ok(t) => {
            let c: FwConfig = toml::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))?;
            c.validate()?;
            Ok(c)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FwConfig::default()),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn write_atomic(p: &Path, body: &str) -> Result<(), String> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, p)).map_err(|e| e.to_string())
}

/// Seconds left to confirm a provisional change, if one is pending.
pub fn pending_left(dir: &Path) -> Option<u64> {
    let until: u64 = std::fs::read_to_string(path(dir, "firewall.try")).ok()?.trim().parse().ok()?;
    let now = crate::cmd::now();
    (until > now && path(dir, "firewall.pending").exists()).then(|| until - now)
}

// ---------------- root helper ----------------

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Status {
    pub ok: bool,
    pub enabled: bool,
    pub at: u64,
    /// which file the active rules came from: firewall.toml | pending | defaults | off
    pub source: String,
    pub error: Option<String>,
    pub tunnel_ports: Vec<u16>,
}

fn write_status(st: &Status) {
    let _ = std::fs::create_dir_all("/run/dongie-firewall");
    if let Ok(j) = serde_json::to_string(st) {
        let _ = std::fs::write(STATUS, j);
    }
}

pub fn read_status() -> Option<Status> {
    serde_json::from_str(&std::fs::read_to_string(STATUS).ok()?).ok()
}

fn nft(input: &str, check: bool) -> Result<(), String> {
    use std::io::Write;
    let mut cmd = std::process::Command::new("nft");
    if check {
        cmd.arg("-c");
    }
    let mut child = cmd
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("nft: {e}"))?;
    child.stdin.take().unwrap().write_all(input.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().take(4).collect::<Vec<_>>().join(" | "))
    }
}

/// UDP listen ports of the WireGuard interfaces that are up (`wg show all listen-port`).
fn wireguard_ports() -> Vec<u16> {
    let out = std::process::Command::new("wg").args(["show", "all", "listen-port"]).output();
    let Ok(out) = out else { return vec![] };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .collect()
}

/// `dongie firewall-apply`: apply the pending config while its try window is
/// open, else firewall.toml, else the defaults; or remove the table when off.
pub fn helper(cfg: &crate::config::Config) -> Result<(), String> {
    let dir = &cfg.state_dir;
    let mut st = Status { at: crate::cmd::now(), enabled: cfg.firewall, ..Default::default() };
    if !cfg.firewall {
        let _ = nft(&format!("table inet {TABLE}\ndelete table inet {TABLE}\n"), false);
        st.ok = true;
        st.source = "off".into();
        write_status(&st);
        println!("firewall: off (firewall = false in config.toml), table removed");
        return Ok(());
    }
    let (conf, source) = if pending_left(dir).is_some() {
        (load_file(&path(dir, "firewall.pending")), "pending")
    } else if path(dir, "firewall.toml").exists() {
        (load(dir), "firewall.toml")
    } else {
        (Ok(FwConfig::default()), "defaults")
    };
    let mut ports = Ports::from_config(cfg);
    let wg = wireguard_ports();
    ports.tunnels.extend(&wg);
    ports.tunnels.sort();
    ports.tunnels.dedup();
    st.tunnel_ports = ports.tunnels.clone();
    let result = conf.and_then(|c| {
        let rs = ruleset(&c, &ports);
        nft(&rs, true).map_err(|e| format!("nft check: {e}"))?;
        nft(&rs, false).map_err(|e| format!("nft: {e}"))
    });
    st.source = source.into();
    match result {
        Ok(()) => {
            st.ok = true;
            write_status(&st);
            println!("firewall: rules from {source} loaded (tunnel ports {:?})", ports.tunnels);
            Ok(())
        }
        Err(e) => {
            // nft -f is atomic: a rejected pending change leaves the previous
            // rules in place. A broken firewall.toml (e.g. edited by hand) at
            // boot would mean no firewall at all: load the defaults instead.
            if source == "firewall.toml" && nft(&ruleset(&FwConfig::default(), &ports), false).is_ok() {
                eprintln!("firewall: firewall.toml unusable, defaults loaded instead");
                st.source = "defaults (firewall.toml unusable)".into();
            }
            st.ok = false;
            st.error = Some(e.clone());
            write_status(&st);
            Err(e)
        }
    }
}

// ---------------- dongie side (unprivileged) ----------------

pub async fn restart_helper() -> Result<(), String> {
    match crate::cmd::run_any_t("systemctl", &["restart", UNIT], 30).await {
        Some((true, _, _)) => Ok(()),
        Some((false, _, e)) => Err(format!("systemctl restart {UNIT}: {}", e.trim())),
        None => Err("systemctl timed out".into()),
    }
}

/// Save a new config provisionally and apply it; it must be confirmed.
pub async fn try_config(dir: &Path, c: &FwConfig) -> Result<u64, String> {
    c.validate()?;
    let body = toml::to_string_pretty(c).map_err(|e| e.to_string())?;
    write_atomic(&path(dir, "firewall.pending"), &format!("# pending, written by dongie's UI\n{body}"))?;
    write_atomic(&path(dir, "firewall.try"), &(crate::cmd::now() + CONFIRM_SECS).to_string())?;
    if let Err(e) = restart_helper().await {
        rollback(dir).await.ok();
        return Err(e);
    }
    if let Some(st) = read_status().filter(|s| !s.ok) {
        rollback(dir).await.ok();
        return Err(st.error.unwrap_or_else(|| "rules rejected".into()));
    }
    Ok(CONFIRM_SECS)
}

pub async fn confirm(dir: &Path) -> Result<(), String> {
    if pending_left(dir).is_none() {
        return Err("nothing to confirm (or too late: the previous rules are back)".into());
    }
    let body = std::fs::read_to_string(path(dir, "firewall.pending")).map_err(|e| e.to_string())?;
    let body =
        body.replacen("# pending, written by dongie's UI\n", "# written by dongie's UI; edit by hand or there\n", 1);
    write_atomic(&path(dir, "firewall.toml"), &body)?;
    let _ = std::fs::remove_file(path(dir, "firewall.pending"));
    let _ = std::fs::remove_file(path(dir, "firewall.try"));
    // same rules, now from firewall.toml (so the status says so)
    restart_helper().await
}

pub async fn rollback(dir: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(path(dir, "firewall.try"));
    let _ = std::fs::remove_file(path(dir, "firewall.pending"));
    restart_helper().await
}

/// Background: roll back an unconfirmed change when its time is up (also one
/// left over from before a dongie restart). At start, re-run the helper when
/// the `firewall` switch and what is loaded disagree (switched in Settings).
pub async fn watchdog(dir: PathBuf, enabled: bool) {
    if read_status().is_none_or(|st| st.enabled != enabled) {
        eprintln!("firewall: switch is {}, applying", if enabled { "on" } else { "off" });
        if let Err(e) = restart_helper().await {
            eprintln!("firewall: {e}");
        }
    }
    loop {
        if path(&dir, "firewall.try").exists() && pending_left(&dir).is_none() {
            eprintln!("firewall: change not confirmed in time, previous rules back");
            if let Err(e) = rollback(&dir).await {
                eprintln!("firewall: rollback: {e}");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_generate() {
        let c = FwConfig::default();
        c.validate().unwrap();
        let p = Ports { web: vec![443, 8040], api: Some(8041), tunnels: vec![41641] };
        let r = ruleset(&c, &p);
        assert!(r.starts_with("table inet dongie\ndelete table inet dongie\n"));
        assert!(r.contains("policy drop"));
        assert!(r.contains("iifname { \"br0\", \"usb*\" } jump in_bridge"));
        assert!(r.contains("tcp dport { 443, 8040 } accept"));
        assert!(r.contains("udp dport { 41641 } accept"));
        // wifi: ssh + ping only, lte: nothing
        let wifi = r.split("chain in_wifi {").nth(1).unwrap().split("  }").next().unwrap();
        assert!(wifi.contains("tcp dport 22") && !wifi.contains("8040"));
        let lte = r.split("chain in_lte {").nth(1).unwrap().split("  }").next().unwrap();
        assert!(lte.trim().is_empty(), "lte: {lte:?}");
        // forward: only the bridge
        let fwd = r.split("chain forward {").nth(1).unwrap();
        assert!(fwd.contains("iifname { \"br0\", \"usb*\" } accept") && !fwd.contains("wlan"));
    }

    #[test]
    fn custom_rules() {
        let mut c = FwConfig::default();
        c.rules.push(Rule {
            zone: "wifi".into(),
            proto: "tcp".into(),
            ports: "8000-8100".into(),
            from: "192.168.1.0/24".into(),
            comment: "home lab".into(),
        });
        c.rules.push(Rule {
            zone: "lte".into(),
            proto: "udp".into(),
            ports: "51820".into(),
            from: String::new(),
            comment: String::new(),
        });
        c.validate().unwrap();
        let r = ruleset(&c, &Ports::default());
        assert!(r.contains("ip saddr 192.168.1.0/24 tcp dport 8000-8100 accept comment \"home lab\""));
        assert!(r.contains("udp dport 51820 accept"));
    }

    #[test]
    fn rejects_injection_and_lockout() {
        let bad = |f: &dyn Fn(&mut FwConfig)| {
            let mut c = FwConfig::default();
            f(&mut c);
            c.validate().is_err()
        };
        assert!(bad(&|c| {
            c.zones.get_mut("wifi").unwrap().push("wlan0\" accept; #".into());
        }));
        assert!(bad(&|c| {
            c.zones.insert("dmz".into(), vec![]);
        }));
        assert!(bad(&|c| {
            c.allow.get_mut("lte").unwrap().push("everything".into());
        }));
        assert!(bad(&|c| c.rules.push(Rule {
            zone: "wifi".into(),
            proto: "tcp".into(),
            ports: "22; flush ruleset".into(),
            from: String::new(),
            comment: String::new()
        })));
        assert!(bad(&|c| c.rules.push(Rule {
            zone: "wifi".into(),
            proto: "icmp".into(),
            ports: "1".into(),
            from: String::new(),
            comment: String::new()
        })));
        assert!(bad(&|c| c.rules.push(Rule {
            zone: "wifi".into(),
            proto: "tcp".into(),
            ports: "80".into(),
            from: "1.2.3.4/33".into(),
            comment: String::new()
        })));
        assert!(bad(&|c| c.rules.push(Rule {
            zone: "wifi".into(),
            proto: "tcp".into(),
            ports: "80".into(),
            from: String::new(),
            comment: "\"; drop".into()
        })));
        // web off on both bridge and tailscale -> refused
        assert!(bad(&|c| {
            c.allow.get_mut("bridge").unwrap().retain(|x| x != "web");
            c.allow.get_mut("tailscale").unwrap().retain(|x| x != "web");
        }));
    }

    #[test]
    fn toml_roundtrip_and_partial_file() {
        let c = FwConfig::default();
        let t = toml::to_string_pretty(&c).unwrap();
        assert_eq!(toml::from_str::<FwConfig>(&t).unwrap(), c);
        // a file with only [allow] keeps the default zones
        let p: FwConfig = toml::from_str("[allow]\nbridge = [\"web\"]\n").unwrap();
        assert_eq!(p.zones, default_zones());
        assert!(toml::from_str::<FwConfig>("unknown = 1").is_err());
    }
}
