// SPDX-License-Identifier: EUPL-1.2
//! Settings changed from the web UI (Settings tab, admin only).
//!
//! dongie runs as its own user and can't write /etc/dongie/config.toml (root's),
//! so UI changes go to <state_dir>/settings.toml, which overrides config.toml key
//! by key. Deleting a key there brings back the config.toml value.
//!
//! Changes take effect through a restart of dongie (systemd starts it again).
//! Changes to listen addresses could lock the admin out, so they are provisional:
//! unless confirmed from the new setup within CONFIRM_SECS, the previous
//! settings.toml is restored and dongie restarts with it.

use crate::config::{Config, NotifyTarget};
use serde_json::{json, Map, Value as Json};
use std::fs;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub const CONFIRM_SECS: u64 = 120;

/// Keys the UI may change. Paths and devices (users_file, state_dir, qmi_device)
/// stay in config.toml.
pub const EDITABLE: &[&str] = &[
    "web_listen",
    "web_listen_extra",
    "api_listen",
    "api_enabled",
    "api_allow_sms",
    "anonymous_guest",
    "guest_networks",
    "update_check",
    "hotspot_connection",
    "lte_connection",
    "poll_secs",
    "max_messages",
    "services",
    "muxtastic_link",
    "ims_socket",
    "ussd",
    "persist",
    "notify",
    "lnmsg",
    "lnmsg_home",
    "lxmf_name",
    "lnsd_unit",
    "lnstatus",
    "reticulum_config",
];

/// Changing one of these can make the web UI unreachable -> needs confirmation.
pub const RISKY: &[&str] = &["web_listen", "web_listen_extra", "api_listen", "api_enabled"];

pub fn override_path(state_dir: &Path) -> PathBuf {
    state_dir.join("settings.toml")
}
fn prev_path(state_dir: &Path) -> PathBuf {
    state_dir.join("settings.toml.prev")
}
fn pending_path(state_dir: &Path) -> PathBuf {
    state_dir.join("settings.pending")
}

pub fn read_table(path: &Path) -> Result<Table, String> {
    match fs::read_to_string(path) {
        Ok(s) => toml::from_str::<Table>(&s).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Table::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// config.toml with the editable keys of settings.toml on top.
pub fn merge(base: &Table, over: &Table) -> Table {
    let mut t = base.clone();
    for (k, v) in over {
        if EDITABLE.contains(&k.as_str()) {
            t.insert(k.clone(), v.clone());
        }
    }
    t
}

pub fn to_config(t: Table) -> Result<Config, String> {
    Value::Table(t).try_into::<Config>().map_err(|e| e.to_string())
}

fn valid_addr(a: &str) -> bool {
    a.parse::<std::net::SocketAddr>().is_ok()
}

/// Checks beyond "it parses".
pub fn validate(c: &Config) -> Result<(), String> {
    if !valid_addr(&c.web_listen) {
        return Err(format!("web_listen: {:?} is not ip:port", c.web_listen));
    }
    if let Some(a) = c.web_listen_extra.iter().find(|a| !valid_addr(a)) {
        return Err(format!("web_listen_extra: {a:?} is not ip:port"));
    }
    if !crate::update::valid_repo(&c.update_repo) {
        return Err(format!("update_repo: {:?} is not owner/name", c.update_repo));
    }
    if let Some(n) = c.guest_networks.iter().find(|n| crate::wgedit::parse_cidr(n).is_none()) {
        return Err(format!("guest_networks: {n:?} is not a network like 192.168.100.0/24"));
    }
    if c.api_enabled && !valid_addr(&c.api_listen) {
        return Err(format!("api_listen: {:?} is not ip:port", c.api_listen));
    }
    if c.api_enabled && c.api_listen.starts_with("0.0.0.0") {
        return Err("api_listen: the API has no login, never bind it to 0.0.0.0".into());
    }
    if !(3..=300).contains(&c.poll_secs) {
        return Err("poll_secs: 3-300".into());
    }
    if !(10..=5000).contains(&c.max_messages) {
        return Err("max_messages: 10-5000".into());
    }
    let unit_ok = |s: &str| !s.is_empty() && s.chars().all(|ch| ch.is_ascii_alphanumeric() || "@._-".contains(ch));
    if let Some(s) = c.services.iter().find(|s| !unit_ok(s)) {
        return Err(format!("services: {s:?} is not a unit name"));
    }
    for (name, v) in [("hotspot_connection", &c.hotspot_connection), ("lte_connection", &c.lte_connection)] {
        if v.is_empty() || v.chars().any(char::is_control) {
            return Err(format!("{name}: must be a connection name"));
        }
    }
    for (i, t) in c.notify.iter().enumerate() {
        crate::notify::validate(t).map_err(|e| format!("notification {}: {e}", i + 1))?;
    }
    Ok(())
}

/// JSON -> TOML for the values the UI sends (no nulls: those mean "remove").
fn to_toml(v: &Json) -> Result<Value, String> {
    Ok(match v {
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => Value::Integer(n.as_i64().ok_or("numbers must be whole")?),
        Json::String(s) => Value::String(s.clone()),
        Json::Array(a) => Value::Array(a.iter().map(to_toml).collect::<Result<_, _>>()?),
        Json::Object(o) => {
            let mut t = Table::new();
            for (k, v) in o {
                if !v.is_null() {
                    t.insert(k.clone(), to_toml(v)?);
                }
            }
            Value::Table(t)
        }
        Json::Null => return Err("null not allowed here".into()),
    })
}

/// Notification targets from the UI: a target without "token" but with "keep_token"
/// (index into the current list) keeps its stored token, which the UI never sees.
fn notify_from_ui(v: &Json, current: &[NotifyTarget]) -> Result<Value, String> {
    let arr = v.as_array().ok_or("notify must be a list")?;
    let mut out = vec![];
    for t in arr {
        let mut o = t.as_object().ok_or("notification target must be an object")?.clone();
        let keep = o.remove("keep_token").and_then(|k| k.as_u64());
        if !o.contains_key("token") {
            if let Some(old) = keep.and_then(|i| current.get(i as usize)) {
                if !old.token.is_empty() {
                    o.insert("token".into(), Json::String(old.token.clone()));
                }
            }
        }
        out.push(to_toml(&Json::Object(o))?);
    }
    Ok(Value::Array(out))
}

/// What the Settings tab shows: effective values, which ones come from
/// settings.toml, tokens only as "set".
pub fn view(cfg: &Config, state_dir: &Path, base: &Table) -> Json {
    let full = serde_json::to_value(cfg).unwrap_or_default();
    let mut values = Map::new();
    for k in EDITABLE {
        let mut v = full.get(*k).cloned().unwrap_or(Json::Null);
        if *k == "notify" {
            if let Some(a) = v.as_array_mut() {
                for t in a.iter_mut().filter_map(|t| t.as_object_mut()) {
                    let set = t.get("token").and_then(|x| x.as_str()).is_some_and(|s| !s.is_empty());
                    t.remove("token");
                    t.insert("token_set".into(), Json::Bool(set));
                }
            }
        }
        values.insert(k.to_string(), v);
    }
    let over = read_table(&override_path(state_dir)).unwrap_or_default();
    let overridden: Vec<&String> = over.keys().filter(|k| EDITABLE.contains(&k.as_str())).collect();
    let in_config: Vec<&String> = base.keys().filter(|k| EDITABLE.contains(&k.as_str())).collect();
    json!({
        "values": values,
        "overridden": overridden,
        "in_config": in_config,
        "risky": RISKY,
        "pending": pending_left(state_dir),
        "confirm_secs": CONFIRM_SECS,
    })
}

pub struct Applied {
    pub risky: bool,
}

/// Apply changes from the UI: {"key": value} sets an override, {"key": null}
/// removes it (back to config.toml). Validated against the merged result before
/// anything is written.
pub fn apply(cfg_path: &str, cfg: &Config, changes: &Map<String, Json>) -> Result<Applied, String> {
    let base = read_table(Path::new(cfg_path))?;
    let opath = override_path(&cfg.state_dir);
    let old_text = fs::read_to_string(&opath).unwrap_or_default();
    let mut over = read_table(&opath)?;
    let mut risky = false;
    for (k, v) in changes {
        if !EDITABLE.contains(&k.as_str()) {
            return Err(format!("{k} can't be changed from the web UI"));
        }
        let before = over.get(k).cloned();
        if v.is_null() {
            over.remove(k);
        } else if k == "notify" {
            over.insert(k.clone(), notify_from_ui(v, &cfg.notify)?);
        } else {
            over.insert(k.clone(), to_toml(v)?);
        }
        if RISKY.contains(&k.as_str()) && before != over.get(k).cloned() {
            risky = true;
        }
    }
    let new_cfg = to_config(merge(&base, &over)).map_err(|e| format!("invalid settings: {e}"))?;
    validate(&new_cfg)?;
    let text = format!(
        "# Written by the dongie web UI (Settings tab). Overrides {cfg_path} key by key;\n\
         # delete a key (or use \"reset\" in the UI) to go back to the config.toml value.\n{}",
        toml::to_string(&over).map_err(|e| e.to_string())?
    );
    if risky {
        // remember how it was, so an unconfirmed change can be undone
        write_atomic(&prev_path(&cfg.state_dir), &old_text)?;
        write_atomic(&pending_path(&cfg.state_dir), &crate::cmd::now().to_string())?;
    }
    write_atomic(&opath, &text)?;
    Ok(Applied { risky })
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp).map_err(io)?;
        f.write_all(text.as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    fs::rename(&tmp, path).map_err(io)
}

/// Seconds left to confirm a risky change, if one is pending.
pub fn pending_left(state_dir: &Path) -> Option<u64> {
    let since: u64 = fs::read_to_string(pending_path(state_dir)).ok()?.trim().parse().ok()?;
    Some((since + CONFIRM_SECS).saturating_sub(crate::cmd::now()))
}

pub fn confirm(state_dir: &Path) -> bool {
    let had = pending_path(state_dir).exists();
    let _ = fs::remove_file(pending_path(state_dir));
    let _ = fs::remove_file(prev_path(state_dir));
    had
}

/// Undo an unconfirmed risky change: previous settings.toml back (or none).
pub fn rollback(state_dir: &Path) -> Result<(), String> {
    let prev = fs::read_to_string(prev_path(state_dir)).unwrap_or_default();
    let opath = override_path(state_dir);
    if prev.trim().is_empty() {
        let _ = fs::remove_file(&opath);
    } else {
        write_atomic(&opath, &prev)?;
    }
    let _ = fs::remove_file(pending_path(state_dir));
    let _ = fs::remove_file(prev_path(state_dir));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dongie-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn setup(name: &str) -> (PathBuf, String, Config) {
        let d = dir(name);
        let cfg_path = d.join("config.toml");
        fs::write(
            &cfg_path,
            format!(
                "web_listen = \"127.0.0.1:8040\"\nstate_dir = \"{}\"\nusers_file = \"{}/users\"\n",
                d.display(),
                d.display()
            ),
        )
        .unwrap();
        let p = cfg_path.to_string_lossy().into_owned();
        let cfg = Config::load(&p);
        (d, p, cfg)
    }

    fn obj(v: Json) -> Map<String, Json> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn override_merge_and_reset() {
        let (d, p, cfg) = setup("merge");
        let a = apply(&p, &cfg, &obj(json!({"persist": true, "poll_secs": 20}))).unwrap();
        assert!(!a.risky);
        let c = Config::load(&p);
        assert!(c.persist && c.poll_secs == 20);
        assert_eq!(c.web_listen, "127.0.0.1:8040"); // untouched keys from config.toml
        apply(&p, &c, &obj(json!({"poll_secs": null}))).unwrap();
        assert_eq!(Config::load(&p).poll_secs, Config::default().poll_secs);
        // not editable / invalid -> nothing written
        assert!(apply(&p, &c, &obj(json!({"state_dir": "/tmp"}))).is_err());
        assert!(apply(&p, &c, &obj(json!({"poll_secs": 1}))).is_err());
        assert!(apply(&p, &c, &obj(json!({"api_listen": "0.0.0.0:8041"}))).is_err());
        assert!(Config::load(&p).persist);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn tokens_are_kept_and_hidden() {
        let (d, p, cfg) = setup("tokens");
        let t = json!({"notify": [{"kind": "ntfy", "url": "https://ntfy.sh/abc", "token": "tk_secret"}]});
        apply(&p, &cfg, &obj(t)).unwrap();
        let c = Config::load(&p);
        assert_eq!(c.notify[0].token, "tk_secret");
        let v = view(&c, &d, &Table::new());
        assert!(!v.to_string().contains("tk_secret"));
        assert_eq!(v["values"]["notify"][0]["token_set"], true);
        // the UI sends the target back without token but with keep_token
        let t2 = json!({"notify": [{"kind": "ntfy", "url": "https://ntfy.sh/xyz", "keep_token": 0}]});
        apply(&p, &c, &obj(t2)).unwrap();
        let c2 = Config::load(&p);
        assert_eq!((c2.notify[0].url.as_str(), c2.notify[0].token.as_str()), ("https://ntfy.sh/xyz", "tk_secret"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn risky_change_rolls_back() {
        let (d, p, cfg) = setup("risky");
        apply(&p, &cfg, &obj(json!({"persist": true}))).unwrap();
        let c = Config::load(&p);
        let a = apply(&p, &c, &obj(json!({"web_listen": "127.0.0.1:9999"}))).unwrap();
        assert!(a.risky && pending_left(&d).is_some());
        assert_eq!(Config::load(&p).web_listen, "127.0.0.1:9999");
        rollback(&d).unwrap();
        let back = Config::load(&p);
        assert_eq!(back.web_listen, "127.0.0.1:8040");
        assert!(back.persist); // the earlier, confirmed change survives
        assert!(pending_left(&d).is_none());
        // confirmed -> stays
        apply(&p, &back, &obj(json!({"web_listen": "127.0.0.1:9998"}))).unwrap();
        assert!(confirm(&d));
        assert_eq!(Config::load(&p).web_listen, "127.0.0.1:9998");
        let _ = fs::remove_dir_all(d);
    }
}
