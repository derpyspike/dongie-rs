// SPDX-License-Identifier: EUPL-1.2
//! Reticulum tab: state of the stick's Reticulum daemon (Leviculum `lnsd`, see
//! contrib/lnsd), read with `lnstatus -j --tables` over the shared instance's
//! RPC. lnstatus authenticates with the `rpc_key` from a client config
//! (`reticulum_config`, default /etc/dongie/reticulum/config) that holds the
//! same key as lnsd's own config, since lnsd keeps its storage on a tmpfs the
//! tools can't see.

use crate::config::Config;
use crate::notify::{LxmfEnv, Record};
use serde_json::{json, Value};

/// Tables shown on the tab (name in lnstatus' census -> label).
const TABLES: &[(&str, &str)] = &[
    ("path_table", "Paths"),
    ("known_identities", "Identities"),
    ("known_ratchets", "Ratchets"),
    ("announce_table", "Announces queued"),
    ("link_table", "Links"),
    ("packet_cache", "Packet hashes"),
];

pub fn available(cfg: &Config) -> bool {
    !cfg.lnsd_unit.is_empty() && std::path::Path::new(&cfg.lnstatus).exists()
}

/// `systemctl show` properties of the daemon's unit.
async fn unit(name: &str) -> Value {
    let out = crate::cmd::run(
        "systemctl",
        &[
            "show",
            name,
            "-p",
            "ActiveState",
            "-p",
            "SubState",
            "-p",
            "ActiveEnterTimestamp",
            "-p",
            "MemoryCurrent",
            "-p",
            "MemoryMax",
            "-p",
            "NRestarts",
        ],
    )
    .await
    .unwrap_or_default();
    let p: std::collections::HashMap<&str, &str> = out.lines().filter_map(|l| l.split_once('=')).collect();
    let num = |k: &str| p.get(k).and_then(|v| v.parse::<u64>().ok());
    json!({
        "active": p.get("ActiveState").copied().unwrap_or("unknown"),
        "sub": p.get("SubState").copied().unwrap_or(""),
        "since": p.get("ActiveEnterTimestamp").copied().unwrap_or(""),
        "memory": num("MemoryCurrent"),
        "memory_max": num("MemoryMax"),
        "restarts": num("NRestarts"),
    })
}

/// lnstatus JSON -> what the tab needs: totals, the network interfaces (not
/// the local shared-instance socket), table sizes.
pub fn summarize(st: &Value) -> Value {
    let ifaces: Vec<Value> = st["interfaces"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| i["type"].as_str() != Some("LocalServerInterface"))
                .map(|i| {
                    json!({
                        "name": i["name"], "type": i["type"], "up": i["status"],
                        "rxb": i["rxb"], "txb": i["txb"], "rxs": i["rxs"], "txs": i["txs"],
                        "announces_in_per_s": i["incoming_announce_frequency"],
                        "bitrate": i["bitrate"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let census = st["transport_tables"]["collections"].as_array().cloned().unwrap_or_default();
    let tables: Vec<Value> = TABLES
        .iter()
        .filter_map(|(name, label)| {
            census
                .iter()
                .find(|c| c["name"].as_str() == Some(name))
                .map(|c| json!({ "name": label, "entries": c["entries"], "capacity": c["capacity"] }))
        })
        .collect();
    json!({
        "rxb": st["rxb"], "txb": st["txb"], "rxs": st["rxs"], "txs": st["txs"],
        "interfaces": ifaces,
        "tables": tables,
    })
}

/// `df` of the RAM-only storage mounts (kB and inodes).
async fn storage(paths: &[&str]) -> Vec<Value> {
    let mut args = vec!["-k", "--output=target,size,used,itotal,iused"];
    args.extend(paths.iter().copied());
    let out = crate::cmd::run("df", &args).await.unwrap_or_default();
    out.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let n = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok());
            (f.len() == 5)
                .then(|| json!({ "path": f[0], "size_kb": n(1), "used_kb": n(2), "inodes": n(3), "inodes_used": n(4) }))
        })
        .collect()
}

/// The stick's own LXMF address (lnmsg's identity), read once.
async fn lxmf_address(env: Option<&LxmfEnv>) -> Option<String> {
    static ADDR: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();
    let env = env?;
    ADDR.get_or_init(|| async {
        // only read it if lnmsg made one: `lnmsg address` would create it otherwise
        if !env.identity.exists() {
            return None;
        }
        let out =
            tokio::process::Command::new(&env.bin).arg("address").env("LNMSG_HOME", &env.home).output().await.ok()?;
        let a = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (a.len() == 32 && a.chars().all(|c| c.is_ascii_hexdigit())).then_some(a)
    })
    .await
    .clone()
}

/// "lnsd 0.10.0-dev+mimalloc (395edd42…)" -> {name, version, commit}
pub fn parse_version(out: &str) -> Option<Value> {
    let line = out.lines().next()?.trim();
    let (name, rest) = line.split_once(' ')?;
    let (version, commit) = match rest.split_once(" (") {
        Some((v, c)) => (v, c.trim_end_matches(')')),
        None => (rest, ""),
    };
    Some(json!({ "name": name, "version": version, "commit": commit }))
}

/// Versions of the Leviculum tools next to lnstatus, read once per dongie run.
async fn versions(cfg: &Config) -> Value {
    static V: tokio::sync::OnceCell<Value> = tokio::sync::OnceCell::const_new();
    V.get_or_init(|| async {
        let dir = std::path::Path::new(&cfg.lnstatus).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let lnmsg = crate::notify::lxmf_env().map(|e| e.bin.clone());
        let mut bins: Vec<String> = vec![dir.join("lnsd").to_string_lossy().into_owned(), cfg.lnstatus.clone()];
        bins.extend(lnmsg);
        let mut out = vec![];
        for b in bins {
            if let Some((true, o, _)) = crate::cmd::run_any_t(&b, &["--version"], 10).await {
                out.extend(parse_version(&o));
            }
        }
        Value::Array(out)
    })
    .await
    .clone()
}

pub async fn info(cfg: &Config) -> Value {
    let unit = unit(&cfg.lnsd_unit).await;
    let status = crate::cmd::run_any_t(&cfg.lnstatus, &["--config", &cfg.reticulum_config, "-j", "--tables"], 15).await;
    let (st, err) = match status {
        Some((true, out, _)) => match serde_json::from_str::<Value>(&out) {
            Ok(v) => (Some(summarize(&v)), None),
            Err(e) => (None, Some(format!("lnstatus: {e}"))),
        },
        Some((false, out, e)) => {
            (None, Some(format!("{} {}", out.trim(), e.trim()).trim().chars().take(200).collect()))
        }
        None => (None, Some("lnstatus timed out".to_string())),
    };
    let env = crate::notify::lxmf_env();
    let recent: Vec<Record> = crate::notify::recent();
    json!({
        "unit_name": cfg.lnsd_unit,
        "versions": versions(cfg).await,
        "unit": unit,
        "status": st,
        "error": err,
        "storage": storage(&["/run/lnsd", &cfg.lnmsg_home]).await,
        "lxmf": {
            "address": lxmf_address(env).await,
            "name": env.map(|e| e.from.clone()),
            "targets": cfg.notify.iter().filter(|t| t.kind == "lxmf").count(),
        },
        "recent": recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse() {
        assert_eq!(
            parse_version("lnsd 0.10.0-dev+mimalloc (395edd42fbbffcdfb438fcd80afa17668459d331)\n").unwrap(),
            json!({"name": "lnsd", "version": "0.10.0-dev+mimalloc", "commit": "395edd42fbbffcdfb438fcd80afa17668459d331"})
        );
        assert_eq!(parse_version("lnmsg 0.1.0").unwrap()["commit"], "");
        assert!(parse_version("").is_none());
    }

    #[test]
    fn summarize_lnstatus_json() {
        // trimmed real `lnstatus -j --tables` output
        let st = json!({
            "rxb": 12484, "txb": 197, "rxs": 6343.4, "txs": 0.0,
            "interfaces": [
                {"type": "LocalServerInterface", "name": "Shared Instance[rns/default]", "status": true},
                {"type": "TCPClientInterface", "name": "TCPInterface[transport/node.example:4242]", "status": true,
                 "rxb": 12484, "txb": 197, "rxs": 6343.4, "txs": 0.0, "incoming_announce_frequency": 2.7, "bitrate": 10000000}
            ],
            "transport_tables": {"collections": [
                {"name": "path_table", "entries": 81, "capacity": 8192},
                {"name": "known_ratchets", "entries": 34, "capacity": 2000},
                {"name": "reverse_table", "entries": 0, "capacity": 16384}
            ]}
        });
        let s = summarize(&st);
        assert_eq!(s["interfaces"].as_array().unwrap().len(), 1);
        assert_eq!(s["interfaces"][0]["up"], true);
        assert_eq!(s["tables"][0], json!({"name": "Paths", "entries": 81, "capacity": 8192}));
        assert_eq!(s["tables"].as_array().unwrap().len(), 2); // reverse_table isn't shown
        assert_eq!(summarize(&json!({}))["interfaces"], json!([]));
    }
}
