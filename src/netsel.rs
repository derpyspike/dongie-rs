// SPDX-License-Identifier: EUPL-1.2
//! Manual mobile network selection ("pick a network" like on a phone).
//!
//! The UZ801 firmware can't scan for networks (QMI network scan aborts), but it can
//! register on a given MCC+MNC: NetworkManager's `gsm.network-id` on the modem
//! connection makes ModemManager register manually before connecting; an empty
//! value means automatic. ModemManager itself refuses to re-register while a data
//! connection is up, so we always go through a reconnect.
//!
//! Safety, because a wrong choice (e.g. a network that doesn't accept the SIM)
//! leaves the stick without any connection — and cuts it off if you're on Tailscale:
//! - after a selection, if the modem connection isn't back within SAFETY_SECS, the
//!   selection is reverted to automatic and the connection brought up again;
//! - a watchdog reverts any manual selection when the modem has been offline for
//!   WATCHDOG_SECS (covers reboots, travelling, a network going away).

use crate::cmd;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub const SAFETY_SECS: u64 = 150;
pub const WATCHDOG_SECS: u64 = 300;

/// Networks offered in the UI (the firmware can't scan, so this list is fixed).
pub const KNOWN: &[(&str, &str, &str)] = &[
    ("21401", "Vodafone", "ES"),
    ("21403", "Orange", "ES"),
    ("21404", "Yoigo", "ES"),
    ("21407", "Movistar", "ES"),
    ("26201", "Telekom", "DE"),
    ("26202", "Vodafone", "DE"),
    ("26203", "O2", "DE"),
];

pub fn name_of(code: &str) -> Option<String> {
    KNOWN.iter().find(|k| k.0 == code).map(|k| format!("{} {}", k.1, k.2))
}

/// "" (automatic) or 5-6 digits.
pub fn valid(n: &str) -> bool {
    n.is_empty() || ((5..=6).contains(&n.len()) && n.chars().all(|c| c.is_ascii_digit()))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Pending {
    pub network: String,
    pub conn: String,
    pub started: u64,
    pub deadline: u64,
    /// None while waiting; "ok" | "reverted to automatic" | error text
    pub result: Option<String>,
    pub finished: u64,
}

#[derive(Clone)]
pub struct NetSel {
    fallback_conn: String,
    pub pending: Arc<Mutex<Option<Pending>>>,
}

async fn network_id(conn: &str) -> String {
    cmd::run("nmcli", &["-g", "gsm.network-id", "con", "show", conn]).await.unwrap_or_default().trim().to_string()
}

/// (connection, network id) as shown on the Mobile tab, kept a few seconds so
/// every open tab's poll doesn't run nmcli twice; changes clear it.
static SHOWN: tokio::sync::Mutex<Option<(std::time::Instant, String, String)>> = tokio::sync::Mutex::const_new(None);

async fn set_network_id(conn: &str, id: &str) -> Result<(), String> {
    *SHOWN.lock().await = None;
    match cmd::run_any("nmcli", &["con", "modify", conn, "gsm.network-id", id]).await {
        Some((true, _, _)) => Ok(()),
        Some((false, _, e)) => Err(e.trim().to_string()),
        None => Err("nmcli timed out".into()),
    }
}

/// All gsm connections (one per SIM).
async fn gsm_conns() -> Vec<String> {
    let out = cmd::run("nmcli", &["-t", "-f", "NAME,TYPE", "con", "show"]).await.unwrap_or_default();
    out.lines()
        .filter_map(|l| l.rsplit_once(':').filter(|(_, t)| *t == "gsm").map(|(n, _)| n.replace("\\:", ":")))
        .collect()
}

/// Is a gsm connection active and fully activated?
async fn modem_online() -> bool {
    let out = cmd::run("nmcli", &["-t", "-f", "TYPE,STATE", "con", "show", "--active"]).await.unwrap_or_default();
    out.lines().any(|l| l == "gsm:activated")
}

/// MCC+MNC of the network the modem is registered on.
async fn registered_operator() -> Option<String> {
    let mm = cmd::run("mmcli", &["-m", "any", "-K"]).await?;
    cmd::kv(&mm).get("modem.3gpp.operator-code").cloned()
}

/// Online *on the wanted network*: modem connection activated and, for a manual
/// selection, registered on exactly that MCC+MNC.
async fn online_on(network: &str) -> bool {
    if !modem_online().await {
        return false;
    }
    network.is_empty() || registered_operator().await.as_deref() == Some(network)
}

async fn reconnect(conn: &str) {
    let down = cmd::run_any_t("nmcli", &["con", "down", conn], 30).await;
    let up = cmd::run_any_t("nmcli", &["con", "up", conn], 100).await;
    let show = |r: &Option<(bool, String, String)>| match r {
        Some((true, _, _)) => "ok".to_string(),
        Some((false, _, e)) => e.trim().chars().take(120).collect(),
        None => "timeout".to_string(),
    };
    eprintln!("netsel: reconnect {conn}: down {}, up {}", show(&down), show(&up));
}

impl NetSel {
    pub fn new(fallback_conn: String) -> NetSel {
        NetSel { fallback_conn, pending: Arc::new(Mutex::new(None)) }
    }

    async fn current_conn(&self) -> String {
        cmd::active_gsm_connection().await.unwrap_or_else(|| self.fallback_conn.clone())
    }

    /// For the UI: active connection, its selection, the pending change, the list.
    pub async fn info(&self) -> serde_json::Value {
        let (conn, id) = {
            let mut c = SHOWN.lock().await;
            match c.as_ref() {
                Some((at, conn, id)) if at.elapsed() < std::time::Duration::from_secs(10) => (conn.clone(), id.clone()),
                _ => {
                    let conn = self.current_conn().await;
                    let id = network_id(&conn).await;
                    *c = Some((std::time::Instant::now(), conn.clone(), id.clone()));
                    (conn, id)
                }
            }
        };
        let known: Vec<_> =
            KNOWN.iter().map(|(c, n, cc)| serde_json::json!({ "code": c, "name": n, "country": cc })).collect();
        serde_json::json!({
            "conn": conn,
            "network": id,
            "network_name": name_of(&id),
            "pending": *self.pending.lock().await,
            "known": known,
            "safety_secs": SAFETY_SECS,
            "watchdog_secs": WATCHDOG_SECS,
        })
    }

    /// Start a selection: set gsm.network-id, reconnect, revert if it doesn't come back.
    pub async fn select(&self, network: String) -> Result<Pending, String> {
        if !valid(&network) {
            return Err("network: empty (automatic) or 5-6 digit MCC+MNC".into());
        }
        {
            let p = self.pending.lock().await;
            if p.as_ref().is_some_and(|p| p.result.is_none()) {
                return Err("a network change is already in progress".into());
            }
        }
        let conn = self.current_conn().await;
        set_network_id(&conn, &network).await?;
        let now = cmd::now();
        let p = Pending {
            network: network.clone(),
            conn: conn.clone(),
            started: now,
            deadline: now + SAFETY_SECS,
            result: None,
            finished: 0,
        };
        *self.pending.lock().await = Some(p.clone());
        let me = self.clone();
        tokio::spawn(async move {
            reconnect(&conn).await;
            // the old connection may still look "activated" for a moment: give it
            // time, then require 3 good checks in a row on the wanted network
            tokio::time::sleep(Duration::from_secs(10)).await;
            let mut good = 0;
            let result = loop {
                if online_on(&network).await {
                    good += 1;
                    if good >= 3 {
                        break "ok".to_string();
                    }
                } else {
                    good = 0;
                }
                if cmd::now() >= now + SAFETY_SECS {
                    if network.is_empty() {
                        break "still offline (already automatic)".to_string();
                    }
                    eprintln!("netsel: no connection on {network} after {SAFETY_SECS}s, reverting to automatic");
                    let r = set_network_id(&conn, "").await;
                    reconnect(&conn).await;
                    break match r {
                        Ok(()) => "reverted to automatic".to_string(),
                        Err(e) => format!("revert failed: {e}"),
                    };
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            };
            eprintln!("netsel: {} -> {result}", if network.is_empty() { "automatic" } else { &network });
            if let Some(p) = me.pending.lock().await.as_mut() {
                p.result = Some(result);
                p.finished = cmd::now();
            }
        });
        Ok(p)
    }

    /// Forever: if the modem is offline for WATCHDOG_SECS while any SIM connection
    /// has a manual network, clear those and reconnect.
    pub async fn watchdog(self) {
        let mut offline_since: Option<u64> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let busy = self.pending.lock().await.as_ref().is_some_and(|p| p.result.is_none());
            if busy || modem_online().await {
                offline_since = None;
                continue;
            }
            let since = *offline_since.get_or_insert(cmd::now());
            if cmd::now() - since < WATCHDOG_SECS {
                continue;
            }
            let mut cleared = vec![];
            for c in gsm_conns().await {
                if !network_id(&c).await.is_empty() && set_network_id(&c, "").await.is_ok() {
                    cleared.push(c);
                }
            }
            if !cleared.is_empty() {
                eprintln!("netsel watchdog: modem offline {WATCHDOG_SECS}s+, manual network cleared on {cleared:?}");
                let conn = self.current_conn().await;
                reconnect(&conn).await;
            }
            offline_since = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_and_names() {
        assert!(valid("") && valid("21401") && valid("310410"));
        assert!(!valid("2140") && !valid("21401a") && !valid("1234567") && !valid("21 01"));
        assert_eq!(name_of("26203").as_deref(), Some("O2 DE"));
        assert_eq!(KNOWN.len(), 7);
    }
}
