// SPDX-License-Identifier: EUPL-1.2
//! Mesh tab, admin: edit the few muxtastic settings that matter on a stick
//! (which node, who may send admin packets, how long history stays in RAM) in
//! muxtastic's own config file, keeping its comments, then restart it.
//! The ports aren't editable: muxtastic.service's LTE drop rule names them.
//! Needs: the config file writable by group dongie (contrib/muxtastic), and
//! polkit allowing start/stop/restart of the muxtastic unit (deploy/50-dongie.rules).

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::Path;
use toml_edit::{value, Array, DocumentMut, Item, Table};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MuxSettings {
    /// node.tcp_host: the Meshtastic node's IP or hostname (on the hotspot)
    pub node_host: String,
    /// node.tcp_port (4403 on the firmware)
    pub node_port: u16,
    /// security.allow_admin_from: IPs/CIDRs that may send ADMIN_APP packets
    pub allow_admin_from: Vec<String>,
    /// web.retention_days (history is in RAM: keep it short)
    pub retention_days: u32,
}

#[derive(Debug, Serialize)]
pub struct View {
    pub path: String,
    pub writable: bool,
    pub values: MuxSettings,
    pub app_port: Option<i64>,
    pub web_port: Option<i64>,
    pub storage: Option<String>,
}

fn get_str(doc: &DocumentMut, t: &str, k: &str) -> Option<String> {
    doc.get(t)?.get(k)?.as_str().map(String::from)
}
fn get_int(doc: &DocumentMut, t: &str, k: &str) -> Option<i64> {
    doc.get(t)?.get(k)?.as_integer()
}

pub fn parse(text: &str) -> Result<(DocumentMut, MuxSettings), String> {
    let doc: DocumentMut = text.parse().map_err(|e| format!("muxtastic.toml: {e}"))?;
    let admin = doc
        .get("security")
        .and_then(|s| s.get("allow_admin_from"))
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let s = MuxSettings {
        node_host: get_str(&doc, "node", "tcp_host").unwrap_or_default(),
        node_port: get_int(&doc, "node", "tcp_port").and_then(|p| u16::try_from(p).ok()).unwrap_or(4403),
        allow_admin_from: admin,
        retention_days: get_int(&doc, "web", "retention_days").and_then(|d| u32::try_from(d).ok()).unwrap_or(1),
    };
    Ok((doc, s))
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && (h.parse::<IpAddr>().is_ok()
            || h.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !l.starts_with('-')
                    && !l.ends_with('-')
            }))
}

/// "100.64.0.0/10", "fd7a:115c:a1e0::/48" or a single address.
pub fn valid_cidr(c: &str) -> bool {
    match c.split_once('/') {
        None => c.parse::<IpAddr>().is_ok(),
        Some((a, p)) => match (a.parse::<IpAddr>(), p.parse::<u8>()) {
            (Ok(IpAddr::V4(_)), Ok(p)) => p <= 32,
            (Ok(IpAddr::V6(_)), Ok(p)) => p <= 128,
            _ => false,
        },
    }
}

pub fn validate(s: &MuxSettings) -> Result<(), String> {
    if !valid_host(s.node_host.trim()) {
        return Err("node address: an IP or hostname".into());
    }
    if s.node_port == 0 {
        return Err("node port: 1-65535 (the firmware uses 4403)".into());
    }
    if s.allow_admin_from.len() > 16 {
        return Err("admin allowed from: at most 16 entries".into());
    }
    if let Some(bad) = s.allow_admin_from.iter().find(|c| !valid_cidr(c.trim())) {
        return Err(format!("admin allowed from: {bad:?} is not an IP or IP/prefix"));
    }
    if !(1..=7).contains(&s.retention_days) {
        return Err("history: 1-7 days (it's kept in RAM)".into());
    }
    Ok(())
}

fn table<'a>(doc: &'a mut DocumentMut, name: &str) -> &'a mut Table {
    if !doc.contains_table(name) {
        doc.insert(name, Item::Table(Table::new()));
    }
    doc[name].as_table_mut().expect("just made sure it's a table")
}

/// Set the values in place: everything else in the file (comments included) stays.
pub fn apply(doc: &mut DocumentMut, s: &MuxSettings) {
    let node = table(doc, "node");
    node["type"] = value("tcp");
    node["tcp_host"] = value(s.node_host.trim());
    node["tcp_port"] = value(i64::from(s.node_port));
    let mut arr = Array::new();
    for c in &s.allow_admin_from {
        arr.push(c.trim());
    }
    // keep the comment that follows the value (its decor) when replacing it
    let sec = table(doc, "security");
    match sec.get_mut("allow_admin_from").and_then(|i| i.as_value_mut()) {
        Some(v) => {
            let decor = v.decor().clone();
            *v = toml_edit::Value::Array(arr);
            *v.decor_mut() = decor;
        }
        None => sec["allow_admin_from"] = value(arr),
    }
    table(doc, "web")["retention_days"] = value(i64::from(s.retention_days));
}

pub async fn read(path: &str) -> Result<View, String> {
    let text = tokio::fs::read_to_string(path).await.map_err(|e| format!("{path}: {e}"))?;
    let (doc, values) = parse(&text)?;
    let writable = writable(path);
    Ok(View {
        path: path.to_string(),
        writable,
        values,
        app_port: get_int(&doc, "server", "port"),
        web_port: get_int(&doc, "web", "port"),
        storage: get_str(&doc, "web", "storage"),
    })
}

/// Can we replace the file (write a temp file next to it and rename)?
fn writable(path: &str) -> bool {
    let dir = Path::new(path).parent().unwrap_or(Path::new("."));
    let probe = dir.join(".dongie-write-test");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Validate, edit, write atomically (old file kept as .bak). Returns the new view.
pub async fn write(path: &str, s: &MuxSettings) -> Result<View, String> {
    validate(s)?;
    let text = tokio::fs::read_to_string(path).await.map_err(|e| format!("{path}: {e}"))?;
    let (mut doc, _) = parse(&text)?;
    apply(&mut doc, s);
    let out = doc.to_string();
    // what we wrote must read back as what was asked
    let (_, check) = parse(&out)?;
    let want = MuxSettings {
        node_host: s.node_host.trim().into(),
        allow_admin_from: s.allow_admin_from.iter().map(|c| c.trim().to_string()).collect(),
        ..s.clone()
    };
    if check != want {
        return Err("internal error: the edited config doesn't read back the same".into());
    }
    let p = Path::new(path);
    let tmp = p.with_extension("toml.dongie-tmp");
    let bak = p.with_extension("toml.bak");
    let res = (|| -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;
        // muxtastic runs as a dynamic user: the file must stay world-readable
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o664))?;
        let _ = std::fs::copy(p, &bak);
        std::fs::rename(&tmp, p)?;
        if let Some(d) = p.parent() {
            std::fs::File::open(d)?.sync_all()?;
        }
        Ok(())
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {path}: {e}"));
    }
    read(path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# muxtastic on an OpenStick
[node]
type = "tcp"
# The Meshtastic node with Wi-Fi
tcp_host = "192.168.4.250"
tcp_port = 4403

[server]
host = "0.0.0.0"
port = 4404                     # also in muxtastic.service

[security]
blocked_portnums = ["ADMIN_APP"]
allow_admin_from = []           # IP/CIDR allowlist

[web]
host = "0.0.0.0"
port = 8090
storage = "memory"              # RAM only
retention_days = 1
"#;

    #[test]
    fn reads_values() {
        let (_, s) = parse(SAMPLE).unwrap();
        assert_eq!(s.node_host, "192.168.4.250");
        assert_eq!(s.node_port, 4403);
        assert!(s.allow_admin_from.is_empty());
        assert_eq!(s.retention_days, 1);
    }

    #[test]
    fn edit_keeps_comments_and_other_keys() {
        let (mut doc, _) = parse(SAMPLE).unwrap();
        let s = MuxSettings {
            node_host: "192.168.4.23".into(),
            node_port: 4403,
            allow_admin_from: vec!["100.64.0.0/10".into(), "fd7a:115c:a1e0::/48".into()],
            retention_days: 3,
        };
        apply(&mut doc, &s);
        let out = doc.to_string();
        assert!(out.contains("# The Meshtastic node with Wi-Fi"));
        assert!(out.contains("port = 4404                     # also in muxtastic.service"));
        assert!(out.contains("# IP/CIDR allowlist"));
        assert!(out.contains("storage = \"memory\"              # RAM only"));
        assert!(out.contains("tcp_host = \"192.168.4.23\""));
        assert_eq!(parse(&out).unwrap().1, s);
    }

    #[test]
    fn validation() {
        let ok = MuxSettings {
            node_host: "meshtastic-ab12".into(),
            node_port: 4403,
            allow_admin_from: vec![],
            retention_days: 1,
        };
        assert!(validate(&ok).is_ok());
        let bad = |f: &dyn Fn(&mut MuxSettings)| {
            let mut s = ok.clone();
            f(&mut s);
            validate(&s).is_err()
        };
        assert!(bad(&|s| s.node_host = "".into()));
        assert!(bad(&|s| s.node_host = "a b".into()));
        assert!(bad(&|s| s.node_host = "x\"; rm -rf".into()));
        assert!(bad(&|s| s.node_port = 0));
        assert!(bad(&|s| s.retention_days = 0));
        assert!(bad(&|s| s.retention_days = 30));
        assert!(bad(&|s| s.allow_admin_from = vec!["100.64.0.0/33".into()]));
        assert!(bad(&|s| s.allow_admin_from = vec!["everyone".into()]));
        assert!(valid_cidr("192.168.4.0/24") && valid_cidr("fd7a:115c:a1e0::/48") && valid_cidr("10.0.0.1"));
    }

    #[test]
    fn missing_tables_are_created() {
        let (mut doc, _) = parse("[node]\ntcp_host = \"x\"\n").unwrap();
        let s = MuxSettings {
            node_host: "10.0.0.2".into(),
            node_port: 4403,
            allow_admin_from: vec!["10.0.0.0/8".into()],
            retention_days: 2,
        };
        apply(&mut doc, &s);
        assert_eq!(parse(&doc.to_string()).unwrap().1, s);
    }
}
