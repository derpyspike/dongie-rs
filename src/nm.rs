// SPDX-License-Identifier: EUPL-1.2
//! Read-only snapshot of NetworkManager over D-Bus, taken once per status
//! round. Replaces ~8 `nmcli` runs every 10 s: on the stick one `nmcli` costs
//! ~200 ms of CPU (it loads NM's whole state each time), these calls a few ms.

use std::collections::HashMap;
use zbus::proxy::{Builder, CacheProperties};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const NM: &str = "org.freedesktop.NetworkManager";
/// NM_ACTIVE_CONNECTION_STATE_ACTIVATED
const ACTIVATED: u32 = 2;

pub type Section = HashMap<String, OwnedValue>;
pub type Settings = HashMap<String, Section>;

/// One saved connection profile.
#[derive(Debug, Clone, Default)]
pub struct Conn {
    pub id: String,
    pub uuid: String,
    /// connection.type, e.g. "gsm", "802-11-wireless", "wireguard"
    pub kind: String,
    pub settings: Settings,
    /// state of its active instance, if it has one (2 = activated)
    pub active_state: Option<u32>,
    /// interfaces it is active on
    pub devices: Vec<String>,
}

impl Conn {
    pub fn activated(&self) -> bool {
        self.active_state == Some(ACTIVATED)
    }
    pub fn str(&self, section: &str, key: &str) -> Option<String> {
        self.settings.get(section).and_then(|s| s.get(key)).and_then(|v| match &**v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        })
    }
    pub fn u32(&self, section: &str, key: &str) -> Option<u32> {
        self.settings.get(section).and_then(|s| s.get(key)).and_then(|v| u32::try_from(v).ok())
    }
    pub fn i32(&self, section: &str, key: &str) -> Option<i32> {
        self.settings.get(section).and_then(|s| s.get(key)).and_then(|v| i32::try_from(v).ok())
    }
    pub fn bool(&self, section: &str, key: &str) -> Option<bool> {
        self.settings.get(section).and_then(|s| s.get(key)).and_then(|v| bool::try_from(v).ok())
    }
    /// 802-11-wireless.ssid is a byte array
    pub fn ssid(&self) -> Option<String> {
        let v = self.settings.get("802-11-wireless")?.get("ssid")?;
        let b: Vec<u8> = v.try_clone().ok().and_then(|v| Vec::<u8>::try_from(v).ok())?;
        (!b.is_empty()).then(|| String::from_utf8_lossy(&b).into_owned())
    }
    /// connection.autoconnect defaults to true when unset
    pub fn autoconnect(&self) -> bool {
        self.bool("connection", "autoconnect").unwrap_or(true)
    }
    /// ipv4 addresses as "a.b.c.d/nn", like nmcli's ipv4.addresses
    pub fn ipv4_addresses(&self) -> Vec<String> {
        let Some(v) = self.settings.get("ipv4").and_then(|s| s.get("address-data")) else {
            return vec![];
        };
        let Ok(list) = v.try_clone().map(Vec::<HashMap<String, OwnedValue>>::try_from) else {
            return vec![];
        };
        list.unwrap_or_default()
            .iter()
            .filter_map(|d| {
                let a = match &**d.get("address")? {
                    Value::Str(s) => s.to_string(),
                    _ => return None,
                };
                let p = u32::try_from(d.get("prefix")?).ok()?;
                Some(format!("{a}/{p}"))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub conns: Vec<Conn>,
}

impl Snapshot {
    pub fn by_id(&self, id: &str) -> Option<&Conn> {
        self.conns.iter().find(|c| c.id == id)
    }
    /// The activated connection on an interface.
    pub fn on_device(&self, iface: &str) -> Option<&Conn> {
        self.conns.iter().find(|c| c.activated() && c.devices.iter().any(|d| d == iface))
    }
    pub fn active_of_kind(&self, kind: &str) -> Option<&Conn> {
        self.conns.iter().find(|c| c.kind == kind && c.activated())
    }
}

/// One system-bus connection for the whole process (zbus reconnects aren't free).
async fn bus() -> Result<Connection, String> {
    static BUS: tokio::sync::OnceCell<Connection> = tokio::sync::OnceCell::const_new();
    BUS.get_or_try_init(|| async { Connection::system().await.map_err(|e| e.to_string()) }).await.cloned()
}

async fn proxy<'a>(c: &'a Connection, path: &str, iface: &'a str) -> Result<Proxy<'a>, String> {
    Builder::<Proxy>::new(c)
        .destination(NM)
        .and_then(|b| b.path(path.to_string()))
        .and_then(|b| b.interface(iface))
        .map(|b| b.cache_properties(CacheProperties::No))
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())
}

pub async fn snapshot() -> Result<Snapshot, String> {
    let c = bus().await?;
    let settings =
        proxy(&c, "/org/freedesktop/NetworkManager/Settings", "org.freedesktop.NetworkManager.Settings").await?;
    let paths: Vec<OwnedObjectPath> = settings.call("ListConnections", &()).await.map_err(|e| e.to_string())?;
    let mut conns = vec![];
    for p in &paths {
        let cp = proxy(&c, p.as_str(), "org.freedesktop.NetworkManager.Settings.Connection").await?;
        // a profile can vanish between list and read: skip it
        let Ok(s) = cp.call::<_, _, Settings>("GetSettings", &()).await else { continue };
        let mut conn = Conn { settings: s, ..Default::default() };
        conn.id = conn.str("connection", "id").unwrap_or_default();
        conn.uuid = conn.str("connection", "uuid").unwrap_or_default();
        conn.kind = conn.str("connection", "type").unwrap_or_default();
        conns.push(conn);
    }
    // active instances: uuid -> (state, interfaces)
    let root = proxy(&c, "/org/freedesktop/NetworkManager", NM).await?;
    let active: Vec<OwnedObjectPath> = root.get_property("ActiveConnections").await.map_err(|e| e.to_string())?;
    for a in &active {
        let ap = proxy(&c, a.as_str(), "org.freedesktop.NetworkManager.Connection.Active").await?;
        let Ok(uuid) = ap.get_property::<String>("Uuid").await else { continue };
        let state: u32 = ap.get_property("State").await.unwrap_or(0);
        let devs: Vec<OwnedObjectPath> = ap.get_property("Devices").await.unwrap_or_default();
        let mut ifaces = vec![];
        for d in &devs {
            let dp = proxy(&c, d.as_str(), "org.freedesktop.NetworkManager.Device").await?;
            if let Ok(i) = dp.get_property::<String>("Interface").await {
                ifaces.push(i);
            }
        }
        if let Some(conn) = conns.iter_mut().find(|x| x.uuid == uuid) {
            conn.active_state = Some(state);
            conn.devices = ifaces;
        }
    }
    Ok(Snapshot { conns })
}

/// The snapshot of the current status round, shared by its parts (LTE, hotspot,
/// Wi-Fi) for a moment so one round takes it once.
pub async fn shared() -> Option<Snapshot> {
    static LAST: tokio::sync::Mutex<Option<(std::time::Instant, Snapshot)>> = tokio::sync::Mutex::const_new(None);
    let mut l = LAST.lock().await;
    if let Some((at, s)) = l.as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(2) {
            return Some(s.clone());
        }
    }
    match snapshot().await {
        Ok(s) => {
            *l = Some((std::time::Instant::now(), s.clone()));
            Some(s)
        }
        Err(e) => {
            eprintln!("nm: D-Bus snapshot failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    /// Needs a running NetworkManager: `cargo test -- --ignored nm_snapshot`
    #[tokio::test]
    #[ignore]
    async fn nm_snapshot_live() {
        let s = super::snapshot().await.expect("snapshot");
        for c in &s.conns {
            eprintln!(
                "{:<28} {:<16} active={:?} on={:?} ssid={:?} auto={} prio={:?} apn={:?} v4={:?}",
                c.id,
                c.kind,
                c.active_state,
                c.devices,
                c.ssid(),
                c.autoconnect(),
                c.i32("connection", "autoconnect-priority"),
                c.str("gsm", "apn"),
                c.ipv4_addresses()
            );
        }
        assert!(!s.conns.is_empty());
    }
}
