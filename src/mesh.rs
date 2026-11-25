// SPDX-License-Identifier: EUPL-1.2
//! Meshtastic tab: what muxtastic (contrib/muxtastic) sees, read from its JSON
//! API on loopback. dongie shows a summary behind its own login and links to
//! muxtastic's full UI (no login of its own) on the same host the browser used.

use crate::config::Config;
use serde_json::{json, Value};
use std::time::Duration;

/// Shown when muxtastic_url is set and the unit is installed.
pub fn available(cfg: &Config) -> bool {
    !cfg.muxtastic_url.is_empty()
        && !cfg.muxtastic_unit.is_empty()
        && ["/etc/systemd/system", "/lib/systemd/system", "/usr/lib/systemd/system"]
            .iter()
            .any(|d| std::path::Path::new(d).join(format!("{}.service", cfg.muxtastic_unit)).exists())
}

/// "http://127.0.0.1:8090" -> "127.0.0.1:8090" (plain HTTP on loopback only).
fn host_port(url: &str) -> Option<&str> {
    let hp = url.strip_prefix("http://")?.trim_end_matches('/');
    (!hp.is_empty() && !hp.contains('/')).then_some(hp)
}

/// GET a JSON document from muxtastic (HTTP/1.0: the body runs to the end of the connection).
async fn get(cfg: &Config, path: &str) -> Result<Value, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let hp = host_port(&cfg.muxtastic_url).ok_or("muxtastic_url must look like http://127.0.0.1:8090")?;
    let fut = async {
        let mut s = tokio::net::TcpStream::connect(hp).await.map_err(|e| format!("muxtastic: {e}"))?;
        let req = format!("GET {path} HTTP/1.0\r\nHost: {hp}\r\nAccept: application/json\r\n\r\n");
        s.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        s.take(4 << 20).read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
        let body = crate::cmd::http10_body(&buf).ok_or_else(|| format!("muxtastic: bad answer for {path}"))?;
        serde_json::from_str(&body).map_err(|e| format!("muxtastic {path}: {e}"))
    };
    tokio::time::timeout(Duration::from_secs(8), fut).await.map_err(|_| "muxtastic: timed out".to_string())?
}

/// The bits of `systemctl show` the tab needs.
async fn unit(name: &str) -> Value {
    let out = crate::cmd::run(
        "systemctl",
        &["show", name, "-p", "ActiveState", "-p", "MemoryCurrent", "-p", "MemoryMax", "-p", "NRestarts"],
    )
    .await
    .unwrap_or_default();
    let p: std::collections::HashMap<&str, &str> = out.lines().filter_map(|l| l.split_once('=')).collect();
    let num = |k: &str| p.get(k).and_then(|v| v.parse::<u64>().ok());
    json!({
        "active": p.get("ActiveState").copied().unwrap_or("unknown"),
        "memory": num("MemoryCurrent"),
        "memory_max": num("MemoryMax"),
        "restarts": num("NRestarts"),
    })
}

/// muxtastic's /api/stats + /api/nodes + /api/messages -> what the tab shows.
/// `admin` adds messages, node positions and client addresses.
pub fn summarize(stats: &Value, nodes: &Value, messages: &Value, admin: bool) -> Value {
    let st = &stats["status"];
    let src = st["sources"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["kind"] == "physical"))
        .cloned()
        .unwrap_or(Value::Null);
    let node = &src["node"];
    let clients: Vec<Value> = src["clients"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| {
                    json!({
                        "peer": if admin { c["peer"].clone() } else { Value::Null },
                        "connected_secs": c["connected_secs"], "idle_secs": c["idle_secs"],
                        "rx_frames": c["rx_frames"], "tx_frames": c["tx_frames"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut heard: Vec<&Value> = nodes.as_array().map(|a| a.iter().collect()).unwrap_or_default();
    heard.sort_by_key(|n| std::cmp::Reverse(n["last_heard"].as_i64().unwrap_or(0)));
    let heard: Vec<Value> = heard
        .iter()
        .take(12)
        .map(|n| {
            json!({
                "name": n["long_name"], "short": n["short_name"], "id": n["node_id"],
                "last_heard": n["last_heard"], "snr": n["snr"], "hops": n["hops_away"],
                "battery": n["battery_level"], "hardware": n["hardware"], "via_mqtt": n["via_mqtt"],
                "has_position": admin && n["latitude_i"].is_i64(),
            })
        })
        .collect();
    let msgs: Vec<Value> = if admin {
        messages
            .as_array()
            .map(|a| {
                a.iter()
                    .take(10)
                    .map(|m| {
                        json!({
                            "sender": m["sender"], "channel": m["channel_name"], "text": m["text"],
                            "at": m["rx_time"], "snr": m["rx_snr"], "rssi": m["rx_rssi"],
                            "hops": match (m["hop_start"].as_i64(), m["hop_limit"].as_i64()) {
                                (Some(s), Some(l)) if s >= l => json!(s - l),
                                _ => Value::Null,
                            },
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec![]
    };
    let t = &stats["stats"]["totals"];
    json!({
        "version": st["version"], "uptime_secs": st["uptime_secs"],
        "connected": src["connected"].as_bool().unwrap_or(false),
        // endpoint holds the node's IP on the hotspot: admins only
        "endpoint": if admin { src["endpoint"].clone() } else { Value::Null },
        "details": src["details"],
        "node": if node.is_object() { json!({
            "long_name": node["long_name"], "short_name": node["short_name"],
            "firmware": node["firmware_version"], "hw_model": node["hw_model"],
            "lora": node["lora"]["summary"], "color": node["color"],
        }) } else { Value::Null },
        "clients": clients,
        "totals": { "packets_24h": t["packets_24h"], "messages_24h": t["messages_24h"], "nodes": t["nodes"] },
        "heard": heard,
        "messages": msgs,
        "storage": st["database"],
    })
}

pub async fn info(cfg: &Config, admin: bool) -> Value {
    let (unit, stats, nodes, messages) =
        tokio::join!(unit(&cfg.muxtastic_unit), get(cfg, "/api/stats"), get(cfg, "/api/nodes"), async {
            if admin {
                get(cfg, "/api/messages?limit=10").await
            } else {
                Ok(Value::Null)
            }
        },);
    // "" = the page builds http://<host it was opened with>:<web_port>/
    let link = (!cfg.muxtastic_link.is_empty()).then(|| cfg.muxtastic_link.clone());
    let web_port =
        host_port(&cfg.muxtastic_url).and_then(|hp| hp.rsplit_once(':')).and_then(|(_, p)| p.parse::<u16>().ok());
    let (mux, error) = match stats {
        Ok(s) => (summarize(&s, &nodes.unwrap_or(Value::Null), &messages.unwrap_or(Value::Null), admin), None),
        Err(e) => (Value::Null, Some(e)),
    };
    json!({ "unit_name": cfg.muxtastic_unit, "unit": unit, "link": link, "web_port": web_port, "mux": mux, "error": error })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Value, Value, Value) {
        let stats = json!({
            "status": {"version": "0.2.1", "uptime_secs": 60, "database": "memory (volatile)",
                "sources": [{"name": "default", "kind": "physical", "endpoint": "tcp 192.168.4.23:4403",
                    "connected": true, "details": "connected",
                    "node": {"node_num": 1, "long_name": "Hotspot node", "short_name": "HSN",
                        "firmware_version": "2.8.1", "hw_model": "HELTEC_V3", "color": "#aabbcc",
                        "lora": {"summary": "EU_868 · LongFast"}},
                    "clients": [{"id": 1, "peer": "127.0.0.1:50000", "connected_secs": 30, "idle_secs": 1,
                        "rx_frames": 5, "tx_frames": 9, "rx_bytes": 1, "tx_bytes": 2}]}]},
            "stats": {"totals": {"packets_24h": 120, "messages_24h": 3, "nodes": 2}}
        });
        let nodes = json!([
            {"long_name": "Old", "short_name": "OLD", "node_id": "!0000aaaa", "last_heard": 100, "latitude_i": 404000000},
            {"long_name": "New", "short_name": "NEW", "node_id": "!0000bbbb", "last_heard": 900, "snr": 6.5, "hops_away": 1}
        ]);
        let msgs = json!([{"sender": "New", "channel_name": "LongFast", "text": "hola", "rx_time": 900,
            "rx_snr": 6.5, "rx_rssi": -80, "hop_start": 3, "hop_limit": 2}]);
        (stats, nodes, msgs)
    }

    #[test]
    fn admin_sees_everything() {
        let (s, n, m) = sample();
        let v = summarize(&s, &n, &m, true);
        assert_eq!(v["connected"], true);
        assert_eq!(v["node"]["short_name"], "HSN");
        assert_eq!(v["endpoint"], "tcp 192.168.4.23:4403");
        assert_eq!(v["heard"][0]["name"], "New"); // newest first
        assert_eq!(v["heard"][1]["has_position"], true);
        assert_eq!(v["messages"][0]["hops"], 1);
        assert_eq!(v["clients"][0]["peer"], "127.0.0.1:50000");
        assert_eq!(v["totals"]["packets_24h"], 120);
    }

    #[test]
    fn guest_gets_no_messages_positions_or_addresses() {
        let (s, n, m) = sample();
        let v = summarize(&s, &n, &m, false);
        assert_eq!(v["messages"], json!([]));
        assert_eq!(v["endpoint"], Value::Null);
        assert_eq!(v["clients"][0]["peer"], Value::Null);
        assert_eq!(v["heard"][1]["has_position"], false);
        assert_eq!(v["heard"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn url_parsing() {
        assert_eq!(host_port("http://127.0.0.1:8090"), Some("127.0.0.1:8090"));
        assert_eq!(host_port("http://127.0.0.1:8090/"), Some("127.0.0.1:8090"));
        assert_eq!(host_port("https://x:1"), None);
        assert_eq!(host_port("http://x/y"), None);
    }

    #[test]
    fn not_connected_yet() {
        let s = json!({"status": {"sources": [{"kind": "physical", "connected": false, "details": "handshake pending", "node": null, "clients": []}]}, "stats": {"totals": {}}});
        let v = summarize(&s, &json!([]), &Value::Null, true);
        assert_eq!(v["connected"], false);
        assert_eq!(v["node"], Value::Null);
        assert_eq!(v["details"], "handshake pending");
    }
}
