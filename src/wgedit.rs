// SPDX-License-Identifier: EUPL-1.2
//! Editing WireGuard connections (NetworkManager) from the web UI: interface
//! settings and peers (add, change, remove), and creating new tunnels.
//!
//! nmcli can't touch WireGuard peers (`wireguard.peers` isn't a property it
//! knows), so this goes through NetworkManager's D-Bus API: GetSettings +
//! GetSecrets, change, Update2 (to disk). Update2 replaces the whole connection,
//! so the existing secrets (private key, preshared keys) are merged in first and
//! carried over unless the edit replaces them. Secrets never leave the stick:
//! the UI only learns whether a key is set.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use zbus::proxy::{Builder, CacheProperties};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

type Section = HashMap<String, OwnedValue>;
type Settings = HashMap<String, Section>;

const NM: &str = "org.freedesktop.NetworkManager";
const IF_CONN: &str = "org.freedesktop.NetworkManager.Settings.Connection";

#[derive(Debug, Default, Clone, Serialize)]
pub struct PeerView {
    /// dongie's label for the peer (kept in the connection's user data)
    pub name: String,
    pub public_key: String,
    pub endpoint: String,
    pub allowed_ips: Vec<String>,
    pub keepalive: u32,
    /// a preshared key is stored (its value is never sent to the browser)
    pub psk_set: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct WgView {
    pub name: String,
    pub private_key_set: bool,
    /// derived from the private key (`wg pubkey`), for the other side's config
    pub public_key: Option<String>,
    pub listen_port: u32,
    pub mtu: u32,
    pub addresses: Vec<String>,
    pub peers: Vec<PeerView>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct PeerEdit {
    pub name: String,
    pub public_key: String,
    pub endpoint: String,
    pub allowed_ips: Vec<String>,
    pub keepalive: u32,
    /// None: keep the stored preshared key (if any); "": remove it; else: set it
    pub psk: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct WgEdit {
    pub listen_port: u32,
    pub mtu: u32,
    pub addresses: Vec<String>,
    /// None: keep; "generate": new key pair; else: this private key
    pub private_key: Option<String>,
    pub peers: Vec<PeerEdit>,
}

// ---------------- validation ----------------

/// A WireGuard key: 32 bytes, standard base64 (44 characters, '=' at the end).
pub fn valid_key(k: &str) -> bool {
    k.len() == 44 && k.ends_with('=') && k[..43].chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

/// "10.0.0.2/24", "fd00::2/64", "0.0.0.0/0"
pub fn parse_cidr(s: &str) -> Option<(IpAddr, u32)> {
    let (ip, p) = s.trim().split_once('/')?;
    let ip: IpAddr = ip.parse().ok()?;
    let p: u32 = p.parse().ok()?;
    (p <= if ip.is_ipv4() { 32 } else { 128 }).then_some((ip, p))
}

/// "vpn.example.org:51820", "1.2.3.4:51820", "[2001:db8::1]:51820"
pub fn valid_endpoint(e: &str) -> bool {
    let Some((host, port)) = e.rsplit_once(':') else { return false };
    if port.parse::<u16>().map(|p| p == 0).unwrap_or(true) {
        return false;
    }
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse::<std::net::Ipv6Addr>().is_ok();
    }
    !host.is_empty() && host.len() <= 253 && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

pub fn validate(e: &WgEdit) -> Result<(), String> {
    if e.listen_port > 65535 {
        return Err("listen port: 0-65535 (0 = random)".into());
    }
    if e.mtu != 0 && !(576..=9000).contains(&e.mtu) {
        return Err("MTU: 0 (automatic) or 576-9000".into());
    }
    if let Some(a) = e.addresses.iter().find(|a| parse_cidr(a).is_none()) {
        return Err(format!("address {a:?}: expected ip/prefix, e.g. 10.0.0.2/24"));
    }
    if let Some(k) = &e.private_key {
        if k != "generate" && !valid_key(k) {
            return Err("private key: 44 characters of base64, or generate a new one".into());
        }
    }
    let mut seen = std::collections::HashSet::new();
    for (i, p) in e.peers.iter().enumerate() {
        let n = i + 1;
        if !valid_key(&p.public_key) {
            return Err(format!("peer {n}: public key must be 44 characters of base64"));
        }
        if !valid_peer_name(&p.name) {
            return Err(format!("peer {n}: name up to 40 characters"));
        }
        if !seen.insert(p.public_key.clone()) {
            return Err(format!("peer {n}: same public key as another peer"));
        }
        if !p.endpoint.is_empty() && !valid_endpoint(&p.endpoint) {
            return Err(format!("peer {n}: endpoint must be host:port"));
        }
        if p.allowed_ips.is_empty() {
            return Err(format!("peer {n}: at least one allowed IP range (e.g. 10.0.0.0/24)"));
        }
        if let Some(a) = p.allowed_ips.iter().find(|a| parse_cidr(a).is_none()) {
            return Err(format!("peer {n}: allowed IP {a:?} must be ip/prefix"));
        }
        if p.keepalive > 65535 {
            return Err(format!("peer {n}: keepalive 0-65535 s"));
        }
        if let Some(k) = &p.psk {
            if !k.is_empty() && !valid_key(k) {
                return Err(format!("peer {n}: preshared key must be 44 characters of base64"));
            }
        }
    }
    Ok(())
}

// ---------------- peer names ----------------

/// NetworkManager user-data key for a peer's name. Keys may only use
/// [A-Za-z0-9._-], so the base64 public key goes in as hex.
fn name_key(public_key: &str) -> String {
    let hex: String = public_key.bytes().map(|b| format!("{b:02x}")).collect();
    format!("{NAME_PREFIX}{hex}")
}

const NAME_PREFIX: &str = "dongie.peer-name.";

/// The connection's user data with our peer names replaced: entries of other
/// programs stay, names of removed peers and cleared names go.
fn merge_names(existing: HashMap<String, String>, peers: &[PeerEdit]) -> HashMap<String, String> {
    let mut data: HashMap<String, String> = existing.into_iter().filter(|(k, _)| !k.starts_with(NAME_PREFIX)).collect();
    for pe in peers {
        let n = pe.name.trim();
        if !n.is_empty() {
            data.insert(name_key(&pe.public_key), n.to_string());
        }
    }
    data
}

pub fn valid_peer_name(n: &str) -> bool {
    n.chars().count() <= 40 && !n.chars().any(char::is_control)
}

fn user_data(s: &Settings) -> HashMap<String, String> {
    s.get("user")
        .and_then(|u| u.get("data"))
        .and_then(|v| HashMap::<String, String>::try_from(v.clone()).ok())
        .unwrap_or_default()
}

/// public key -> name, for one connection (no secrets needed).
pub async fn peer_names(name: &str) -> HashMap<String, String> {
    async fn inner(name: &str) -> Result<HashMap<String, String>, String> {
        let conn = Connection::system().await.map_err(|e| e.to_string())?;
        let p = proxy(&conn, &conn_path(name).await?).await?;
        let s: Settings = p.call("GetSettings", &()).await.map_err(|e| e.to_string())?;
        let data = user_data(&s);
        let peers = s.get("wireguard").map(|w| get_dicts(w, "peers")).unwrap_or_default();
        Ok(peers
            .iter()
            .filter_map(|pe| {
                let pk = get_str(pe, "public-key")?;
                let n = data.get(&name_key(&pk))?.clone();
                Some((pk, n))
            })
            .collect())
    }
    inner(name).await.unwrap_or_default()
}

// ---------------- D-Bus plumbing ----------------

fn ov(v: Value<'_>) -> OwnedValue {
    // only fails for file descriptors, which we never build
    OwnedValue::try_from(v).expect("plain value")
}

fn get_str(s: &Section, k: &str) -> Option<String> {
    s.get(k).and_then(|v| String::try_from(v.clone()).ok())
}

fn get_u32(s: &Section, k: &str) -> Option<u32> {
    s.get(k).and_then(|v| u32::try_from(v.clone()).ok())
}

fn get_dicts(s: &Section, k: &str) -> Vec<Section> {
    s.get(k).and_then(|v| Vec::<Section>::try_from(v.clone()).ok()).unwrap_or_default()
}

fn get_strs(s: &Section, k: &str) -> Vec<String> {
    s.get(k).and_then(|v| Vec::<String>::try_from(v.clone()).ok()).unwrap_or_default()
}

/// D-Bus settings path of a connection, by its UUID (GENERAL.CON-PATH only
/// exists while a connection is active).
async fn conn_path(name: &str) -> Result<String, String> {
    let known = crate::nm::shared().await.and_then(|s| s.by_id(name).map(|c| c.uuid.clone()));
    let uuid = match known {
        Some(u) => u,
        // just created and not in the snapshot yet
        None => match crate::cmd::run_any("nmcli", &["-g", "connection.uuid", "connection", "show", name]).await {
            Some((true, out, _)) if !out.trim().is_empty() => out.trim().to_string(),
            _ => return Err(format!("no connection {name:?}")),
        },
    };
    let conn = Connection::system().await.map_err(|e| e.to_string())?;
    let settings = Builder::<Proxy>::new(&conn)
        .destination(NM)
        .and_then(|b| b.path("/org/freedesktop/NetworkManager/Settings"))
        .and_then(|b| b.interface("org.freedesktop.NetworkManager.Settings"))
        .map(|b| b.cache_properties(CacheProperties::No))
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;
    let path: zbus::zvariant::OwnedObjectPath = settings
        .call("GetConnectionByUuid", &(uuid.as_str(),))
        .await
        .map_err(|e| format!("GetConnectionByUuid: {e}"))?;
    Ok(path.to_string())
}

async fn proxy<'a>(conn: &'a Connection, path: &str) -> Result<Proxy<'a>, String> {
    let p = ObjectPath::try_from(path.to_string()).map_err(|e| e.to_string())?;
    Builder::<Proxy>::new(conn)
        .destination(NM)
        .and_then(|b| b.path(p))
        .and_then(|b| b.interface(IF_CONN))
        .map(|b| b.cache_properties(CacheProperties::No))
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())
}

/// Settings with the wireguard secrets merged in (what Update2 needs back).
async fn load(p: &Proxy<'_>) -> Result<Settings, String> {
    let mut settings: Settings = p.call("GetSettings", &()).await.map_err(|e| format!("GetSettings: {e}"))?;
    // a connection that has no secrets yet (just created) answers NoSecrets
    let secrets: Settings = match p.call("GetSecrets", &("wireguard",)).await {
        Ok(s) => s,
        Err(e) if e.to_string().contains("NoSecrets") => Settings::new(),
        Err(e) => return Err(format!("GetSecrets: {e}")),
    };
    if let Some(sec) = secrets.get("wireguard") {
        let wg = settings.entry("wireguard".into()).or_default();
        if let Some(k) = sec.get("private-key") {
            wg.insert("private-key".into(), k.clone());
        }
        let secret_peers = get_dicts(sec, "peers");
        let mut peers = get_dicts(wg, "peers");
        for p in peers.iter_mut() {
            let pk = get_str(p, "public-key").unwrap_or_default();
            if let Some(sp) = secret_peers.iter().find(|sp| get_str(sp, "public-key").as_deref() == Some(&pk)) {
                if let Some(psk) = sp.get("preshared-key") {
                    p.insert("preshared-key".into(), psk.clone());
                }
            }
        }
        wg.insert("peers".into(), peers_value(peers));
    }
    Ok(settings)
}

fn peers_value(peers: Vec<Section>) -> OwnedValue {
    let v: Vec<HashMap<String, Value<'static>>> =
        peers.into_iter().map(|p| p.into_iter().map(|(k, v)| (k, Value::from(v))).collect()).collect();
    ov(Value::from(v))
}

async fn wg_tool(args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("wg")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("wg: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        if let Some(s) = stdin {
            si.write_all(format!("{s}\n").as_bytes()).await.map_err(|e| format!("wg: {e}"))?;
        }
    }
    let out = child.wait_with_output().await.map_err(|e| format!("wg: {e}"))?;
    if !out.status.success() {
        return Err(format!("wg {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn addresses_of(s: Option<&Section>) -> Vec<String> {
    let Some(s) = s else { return vec![] };
    get_dicts(s, "address-data")
        .iter()
        .filter_map(|a| Some(format!("{}/{}", get_str(a, "address")?, get_u32(a, "prefix")?)))
        .collect()
}

pub async fn view(name: &str) -> Result<WgView, String> {
    let conn = Connection::system().await.map_err(|e| e.to_string())?;
    let p = proxy(&conn, &conn_path(name).await?).await?;
    let s = load(&p).await?;
    let wg = s.get("wireguard").cloned().unwrap_or_default();
    let private = get_str(&wg, "private-key").filter(|k| !k.is_empty());
    let public_key = match &private {
        Some(k) => wg_tool(&["pubkey"], Some(k)).await.ok(),
        None => None,
    };
    let names = user_data(&s);
    let mut addresses = addresses_of(s.get("ipv4"));
    addresses.extend(addresses_of(s.get("ipv6")));
    let peers = get_dicts(&wg, "peers")
        .iter()
        .map(|p| PeerView {
            name: names.get(&name_key(&get_str(p, "public-key").unwrap_or_default())).cloned().unwrap_or_default(),
            public_key: get_str(p, "public-key").unwrap_or_default(),
            endpoint: get_str(p, "endpoint").unwrap_or_default(),
            allowed_ips: get_strs(p, "allowed-ips"),
            keepalive: get_u32(p, "persistent-keepalive").unwrap_or(0),
            psk_set: get_str(p, "preshared-key").is_some_and(|k| !k.is_empty()),
        })
        .collect();
    Ok(WgView {
        name: name.to_string(),
        private_key_set: private.is_some(),
        public_key,
        listen_port: get_u32(&wg, "listen-port").unwrap_or(0),
        mtu: get_u32(&wg, "mtu").unwrap_or(0),
        addresses,
        peers,
    })
}

fn address_section(sec: &mut Section, addrs: &[(IpAddr, u32)]) {
    // `addresses` is the deprecated form; if it's sent, NM ignores address-data
    sec.remove("addresses");
    let data: Vec<HashMap<String, Value<'static>>> = addrs
        .iter()
        .map(|(ip, p)| {
            HashMap::from([
                ("address".to_string(), Value::from(ip.to_string())),
                ("prefix".to_string(), Value::from(*p)),
            ])
        })
        .collect();
    let has = !data.is_empty();
    sec.insert("address-data".into(), ov(Value::from(data)));
    let method = get_str(sec, "method").unwrap_or_default();
    if has && method != "manual" {
        sec.insert("method".into(), ov(Value::from("manual")));
    } else if !has && method == "manual" {
        sec.insert("method".into(), ov(Value::from("disabled")));
    }
}

/// Apply an edit; returns the new view. Re-activates the tunnel if it was up.
pub async fn apply(name: &str, e: &WgEdit) -> Result<WgView, String> {
    validate(e)?;
    let conn = Connection::system().await.map_err(|e| e.to_string())?;
    let p = proxy(&conn, &conn_path(name).await?).await?;
    let mut s = load(&p).await?;

    let wg = s.entry("wireguard".into()).or_default();
    let old_peers = get_dicts(wg, "peers");
    wg.insert("listen-port".into(), ov(Value::from(e.listen_port)));
    wg.insert("mtu".into(), ov(Value::from(e.mtu)));
    match e.private_key.as_deref() {
        Some("generate") => {
            let k = wg_tool(&["genkey"], None).await?;
            wg.insert("private-key".into(), ov(Value::from(k)));
            wg.insert("private-key-flags".into(), ov(Value::from(0u32)));
        }
        Some(k) => {
            wg.insert("private-key".into(), ov(Value::from(k.to_string())));
            wg.insert("private-key-flags".into(), ov(Value::from(0u32)));
        }
        None => {}
    }
    let peers: Vec<Section> = e
        .peers
        .iter()
        .map(|pe| {
            let mut d = Section::new();
            d.insert("public-key".into(), ov(Value::from(pe.public_key.clone())));
            if !pe.endpoint.is_empty() {
                d.insert("endpoint".into(), ov(Value::from(pe.endpoint.clone())));
            }
            d.insert("allowed-ips".into(), ov(Value::from(pe.allowed_ips.clone())));
            if pe.keepalive > 0 {
                d.insert("persistent-keepalive".into(), ov(Value::from(pe.keepalive)));
            }
            let old_psk = old_peers
                .iter()
                .find(|o| get_str(o, "public-key").as_deref() == Some(pe.public_key.as_str()))
                .and_then(|o| o.get("preshared-key").cloned());
            let psk = match pe.psk.as_deref() {
                None => old_psk,
                Some("") => None,
                Some(k) => Some(ov(Value::from(k.to_string()))),
            };
            if let Some(k) = psk {
                d.insert("preshared-key".into(), k);
                d.insert("preshared-key-flags".into(), ov(Value::from(0u32)));
            }
            d
        })
        .collect();
    wg.insert("peers".into(), peers_value(peers));

    let data = merge_names(user_data(&s), &e.peers);
    let user = s.entry("user".into()).or_default();
    user.insert("data".into(), ov(Value::from(data)));

    let parsed: Vec<(IpAddr, u32)> = e.addresses.iter().filter_map(|a| parse_cidr(a)).collect();
    let v4: Vec<_> = parsed.iter().copied().filter(|(ip, _)| ip.is_ipv4()).collect();
    let v6: Vec<_> = parsed.iter().copied().filter(|(ip, _)| ip.is_ipv6()).collect();
    address_section(s.entry("ipv4".into()).or_default(), &v4);
    address_section(s.entry("ipv6".into()).or_default(), &v6);

    // 0x1 = NM_SETTINGS_UPDATE2_FLAG_TO_DISK
    let _: HashMap<String, OwnedValue> =
        p.call("Update2", &(s, 1u32, HashMap::<String, Value>::new())).await.map_err(|e| format!("Update2: {e}"))?;

    let active = crate::cmd::run("nmcli", &["-g", "GENERAL.STATE", "connection", "show", name])
        .await
        .is_some_and(|s| s.contains("activated"));
    if active {
        crate::cmd::run_any_t("nmcli", &["--wait", "20", "connection", "up", name], 30).await;
    }
    view(name).await
}

/// A new, empty WireGuard connection (not started, not at boot); fill it with apply().
pub async fn create(name: &str) -> Result<(), String> {
    if !crate::net::valid_wg_name(name) {
        return Err("name: 1-15 characters, letters, digits, - and _".into());
    }
    if conn_path(name).await.is_ok() {
        return Err(format!("a connection called {name:?} exists already"));
    }
    match crate::cmd::run_any_t(
        "nmcli",
        &[
            "connection",
            "add",
            "type",
            "wireguard",
            "con-name",
            name,
            "ifname",
            name,
            "connection.autoconnect",
            "no",
            "ipv4.method",
            "disabled",
            "ipv6.method",
            "disabled",
        ],
        20,
    )
    .await
    {
        Some((true, _, _)) => Ok(()),
        Some((false, _, e)) => Err(e.trim().chars().take(200).collect()),
        None => Err("nmcli timed out".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=";
    const K2: &str = "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=";

    fn edit() -> WgEdit {
        WgEdit {
            listen_port: 51820,
            mtu: 0,
            addresses: vec!["10.0.0.2/24".into(), "fd00::2/64".into()],
            private_key: None,
            peers: vec![PeerEdit {
                name: "Home router".into(),
                public_key: K1.into(),
                endpoint: "vpn.example.org:51820".into(),
                allowed_ips: vec!["10.0.0.0/24".into(), "fd00::/64".into()],
                keepalive: 25,
                psk: None,
            }],
        }
    }

    #[test]
    fn names_merge_keeps_foreign_user_data() {
        let existing = HashMap::from([
            ("other.app.key".to_string(), "keepme".to_string()),
            (name_key(K1), "old name".to_string()),
            (name_key(K2), "removed peer".to_string()),
        ]);
        let peers = vec![
            PeerEdit { name: " Router ".into(), public_key: K1.into(), ..Default::default() },
            PeerEdit { name: "".into(), public_key: "zzzz".into(), ..Default::default() },
        ];
        let m = merge_names(existing, &peers);
        assert_eq!(m.get("other.app.key").map(String::as_str), Some("keepme"));
        assert_eq!(m.get(&name_key(K1)).map(String::as_str), Some("Router"));
        assert!(!m.contains_key(&name_key(K2)));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn peer_name_keys() {
        let k = name_key(K1);
        assert!(k.starts_with("dongie.peer-name.") && k.len() == NAME_PREFIX.len() + 88);
        assert!(k.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)));
        assert!(valid_peer_name("Phone 📱") && !valid_peer_name("a\nb") && !valid_peer_name(&"x".repeat(41)));
    }

    #[test]
    fn validation() {
        assert!(validate(&edit()).is_ok());
        assert!(valid_key(K1) && !valid_key("short=") && !valid_key(&K1.replace('+', "!")));
        assert!(valid_endpoint("1.2.3.4:51820") && valid_endpoint("[2001:db8::1]:51820"));
        assert!(!valid_endpoint("host") && !valid_endpoint("host:0") && !valid_endpoint("h o:1"));
        assert_eq!(parse_cidr("0.0.0.0/0"), Some(("0.0.0.0".parse().unwrap(), 0)));
        assert!(parse_cidr("10.0.0.1/33").is_none() && parse_cidr("fd00::1/129").is_none());
        let mut e = edit();
        e.peers.push(PeerEdit { public_key: K1.into(), allowed_ips: vec!["10.1.0.0/24".into()], ..Default::default() });
        assert!(validate(&e).unwrap_err().contains("same public key"));
        e.peers[1].public_key = K2.into();
        assert!(validate(&e).is_ok());
        e.peers[1].allowed_ips.clear();
        assert!(validate(&e).is_err());
        let mut e = edit();
        e.mtu = 100;
        assert!(validate(&e).is_err());
        let mut e = edit();
        e.private_key = Some("generate".into());
        assert!(validate(&e).is_ok());
        e.peers[0].psk = Some("nope".into());
        assert!(validate(&e).is_err());
    }
}
