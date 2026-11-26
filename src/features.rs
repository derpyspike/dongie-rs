// SPDX-License-Identifier: EUPL-1.2
//! On/off switches in the UI (admin only):
//!
//! - **Tailscale serve**: `https://<stick>.<tailnet>.ts.net` → dongie, tailnet
//!   only. Needs `tailscale set --operator=dongie`. Only `tailscale serve` is
//!   ever run; Funnel (public internet) is never turned on, only reported.
//! - **VoWiFi** (dongie-ims): marker files in <state_dir>. `vowifi-on` = dongie
//!   starts dongie-ims when it starts itself (so it also works when the unit is
//!   not enabled in systemd); `vowifi-off` = the unit's drop-in
//!   (deploy/dongie-ims.service.d/dongie.conf) keeps systemd from starting it.
//!   Neither file = whatever systemd does.

use crate::cmd;
use crate::config::Config;
use serde::Serialize;
use std::path::Path;

const IMS_UNIT: &str = "dongie-ims.service";
const HTTPS_PORT: &str = "443";

#[derive(Debug, Serialize, Default)]
pub struct Serve {
    /// tailscaled reachable and logged in
    pub available: bool,
    pub on: bool,
    /// https://name.tailnet.ts.net
    pub url: Option<String>,
    /// what :443 proxies to
    pub target: Option<String>,
    /// Funnel is on for something (shown as a warning, never switched here)
    pub funnel: bool,
    /// dongie isn't the Tailscale operator, so it can't change serve
    pub need_operator: bool,
    /// serve config for other ports/paths exists (left alone)
    pub other: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct VoWifi {
    pub installed: bool,
    /// on | off | auto (no choice made in dongie: systemd decides)
    pub choice: &'static str,
    pub active: bool,
    pub enabled_at_boot: bool,
    /// access mode for dongie-ims (file `ims-mode` in state_dir):
    /// vowifi | volte | volte+vowifi | vowifi+volte
    pub mode: String,
}

/// IMS access modes dongie-ims understands ("off" = service stopped).
pub const IMS_MODES: &[&str] = &["vowifi", "volte", "volte+vowifi", "vowifi+volte"];
const IMS_MODE_FILE: &str = "ims-mode";

pub fn ims_mode(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(IMS_MODE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|m| IMS_MODES.contains(&m.as_str()))
        .unwrap_or_else(|| "vowifi".into())
}

/// Set the calling mode: "off" stops dongie-ims (like the old switch); any
/// other mode is written for dongie-ims and then applied (reload when it runs,
/// start when it doesn't).
pub async fn ims_mode_set(dir: &Path, socket: &str, mode: &str) -> Result<VoWifi, String> {
    if mode == "off" {
        return vowifi_set(dir, false).await;
    }
    if !IMS_MODES.contains(&mode) {
        return Err(format!("unknown mode {mode:?}"));
    }
    let path = dir.join(IMS_MODE_FILE);
    let tmp = dir.join(".ims-mode.tmp");
    std::fs::write(&tmp, format!("{mode}\n")).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| e.to_string())?;
    let st = vowifi_state(dir).await;
    if st.active && st.choice != "off" {
        match crate::ims::request(socket, "POST", "/v1/mode/reload").await {
            Some((202, _)) | Some((200, _)) => Ok(vowifi_state(dir).await),
            Some((code, body)) => Err(format!("dongie-ims answered {code}: {}", first_line(&body))),
            None => Err("dongie-ims isn't answering on its socket".into()),
        }
    } else {
        vowifi_set(dir, true).await
    }
}

#[derive(Debug, Serialize, Default)]
pub struct Features {
    pub serve: Serve,
    pub vowifi: VoWifi,
}

/// Parse `tailscale serve status --json` / localapi serve-config for host:443.
pub fn parse_serve(json: &str, dns_name: Option<&str>) -> (bool, Option<String>, bool, bool) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let mut on = false;
    let mut target = None;
    let mut other = false;
    if let Some(web) = v["Web"].as_object() {
        for (hp, cfg) in web {
            let mine = hp.ends_with(&format!(":{HTTPS_PORT}")) && dns_name.is_none_or(|d| hp.starts_with(d));
            match (mine, cfg["Handlers"]["/"]["Proxy"].as_str()) {
                (true, Some(p)) => {
                    on = true;
                    target = Some(p.to_string());
                    if cfg["Handlers"].as_object().is_some_and(|h| h.len() > 1) {
                        other = true;
                    }
                }
                _ => other = true,
            }
        }
    }
    if let Some(tcp) = v["TCP"].as_object() {
        if tcp.keys().any(|k| k != HTTPS_PORT) {
            other = true;
        }
    }
    let funnel = v["AllowFunnel"].as_object().is_some_and(|m| m.values().any(|x| x.as_bool() == Some(true)));
    (on, target, funnel, other)
}

async fn ts_status() -> Option<serde_json::Value> {
    let j = cmd::tailscale("status?peers=false", &["status", "--json", "--peers=false"]).await?;
    serde_json::from_str(&j).ok()
}

async fn operator_is_dongie() -> bool {
    let me = std::env::var("USER").unwrap_or_else(|_| "dongie".into());
    match cmd::tailscale("prefs", &["debug", "prefs"]).await {
        Some(j) => serde_json::from_str::<serde_json::Value>(&j)
            .ok()
            .and_then(|v| v["OperatorUser"].as_str().map(|o| o == me))
            .unwrap_or(false),
        None => false,
    }
}

pub async fn serve_state() -> Serve {
    let mut s = Serve::default();
    let Some(st) = ts_status().await else { return s };
    s.available = st["BackendState"].as_str() == Some("Running");
    let dns = st["Self"]["DNSName"].as_str().map(|d| d.trim_end_matches('.').to_string()).filter(|d| !d.is_empty());
    s.url = dns.as_ref().map(|d| format!("https://{d}"));
    if let Some(j) = cmd::tailscale("serve-config", &["serve", "status", "--json"]).await {
        (s.on, s.target, s.funnel, s.other) = parse_serve(&j, dns.as_deref());
    }
    s.need_operator = !operator_is_dongie().await;
    s
}

/// The address `tailscale serve` should proxy to: the web UI's Tailscale
/// listener if there is one, else its first fixed address.
pub async fn serve_target(cfg: &Config) -> Option<String> {
    use crate::listen::{parse, resolve, Spec};
    let specs: Vec<Spec> = std::iter::once(&cfg.web_listen)
        .chain(cfg.web_listen_extra.iter())
        .filter_map(|a| parse(a))
        .flatten()
        .collect();
    for s in specs.iter().filter(|s| matches!(s, Spec::Tailscale { v6: false, .. })) {
        if let Some(a) = resolve(s).await {
            return Some(format!("http://{a}"));
        }
    }
    for s in &specs {
        if let Spec::Fixed(a) = s {
            if a.ip().is_unspecified() {
                return Some(format!("http://127.0.0.1:{}", a.port()));
            }
            if !a.ip().is_loopback() || specs.len() == 1 {
                return Some(format!("http://{a}"));
            }
        }
    }
    None
}

pub async fn serve_set(cfg: &Config, on: bool) -> Result<Serve, String> {
    let cur = serve_state().await;
    if !cur.available {
        return Err("Tailscale isn't running / logged in".into());
    }
    if cur.need_operator {
        return Err(
            "dongie isn't allowed to change Tailscale: run `sudo tailscale set --operator=dongie` on the stick".into(),
        );
    }
    let args: Vec<String> = if on {
        let t = serve_target(cfg).await.ok_or("dongie's web UI has no address to serve")?;
        vec!["serve".into(), "--bg".into(), format!("--https={HTTPS_PORT}"), t]
    } else {
        vec!["serve".into(), format!("--https={HTTPS_PORT}"), "off".into()]
    };
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match cmd::run_any_t("tailscale", &a, 30).await {
        Some((true, _, _)) => Ok(serve_state().await),
        Some((false, out, e)) => Err(first_line(if e.trim().is_empty() { &out } else { &e })),
        None => Err("tailscale serve timed out".into()),
    }
}

fn first_line(s: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("failed").chars().take(300).collect()
}

fn marker(dir: &Path, on: bool) -> std::path::PathBuf {
    dir.join(if on { "vowifi-on" } else { "vowifi-off" })
}

pub fn vowifi_choice(dir: &Path) -> &'static str {
    if marker(dir, false).exists() {
        "off"
    } else if marker(dir, true).exists() {
        "on"
    } else {
        "auto"
    }
}

async fn unit_prop(prop: &str) -> Option<String> {
    cmd::run("systemctl", &["show", "-p", prop, "--value", IMS_UNIT]).await.map(|s| s.trim().to_string())
}

pub async fn vowifi_state(dir: &Path) -> VoWifi {
    let load = unit_prop("LoadState").await;
    VoWifi {
        installed: load.as_deref() == Some("loaded"),
        choice: vowifi_choice(dir),
        active: unit_prop("ActiveState").await.as_deref() == Some("active"),
        enabled_at_boot: unit_prop("UnitFileState").await.as_deref() == Some("enabled"),
        mode: ims_mode(dir),
    }
}

pub async fn vowifi_set(dir: &Path, on: bool) -> Result<VoWifi, String> {
    if !vowifi_state(dir).await.installed {
        return Err("dongie-ims isn't installed on this stick".into());
    }
    // marker first, so a reboot in between already does the right thing
    std::fs::write(marker(dir, on), b"").map_err(|e| e.to_string())?;
    match std::fs::remove_file(marker(dir, !on)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        _ => {}
    }
    let verb = if on { "start" } else { "stop" };
    match cmd::run_any("systemctl", &["--no-block", verb, IMS_UNIT]).await {
        Some((true, _, _)) => {}
        Some((false, _, e)) => return Err(first_line(&e)),
        None => return Err("systemctl timed out".into()),
    }
    Ok(vowifi_state(dir).await)
}

/// At dongie start: honour a `vowifi-on` choice even when the unit isn't
/// enabled in systemd.
pub async fn vowifi_boot(dir: std::path::PathBuf) {
    if vowifi_choice(&dir) == "on" {
        let s = vowifi_state(&dir).await;
        if s.installed && !s.active {
            eprintln!("vowifi: turned on in the UI, starting dongie-ims");
            let _ = cmd::run_any("systemctl", &["--no-block", "start", IMS_UNIT]).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_config() {
        // a UZ801, 2026-11-26 (host name and address made up)
        let j = r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"mystick.example-tailnet.ts.net:443":{"Handlers":{"/":{"Proxy":"http://100.64.12.34:8040"}}}}}"#;
        assert_eq!(
            parse_serve(j, Some("mystick.example-tailnet.ts.net")),
            (true, Some("http://100.64.12.34:8040".into()), false, false)
        );
        assert_eq!(parse_serve("{}", Some("other.example-tailnet.ts.net")), (false, None, false, false));
        let f = r#"{"TCP":{"443":{"HTTPS":true},"22":{"TCPForward":"127.0.0.1:22"}},"Web":{"a.ts.net:443":{"Handlers":{"/":{"Proxy":"http://x"}}}},"AllowFunnel":{"a.ts.net:443":true}}"#;
        assert_eq!(parse_serve(f, Some("a.ts.net")), (true, Some("http://x".into()), true, true));
    }

    #[test]
    fn ims_mode_file() {
        let d = std::env::temp_dir().join(format!("dongie-imsmode-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(ims_mode(&d), "vowifi", "default");
        std::fs::write(d.join(IMS_MODE_FILE), "volte+vowifi\n").unwrap();
        assert_eq!(ims_mode(&d), "volte+vowifi");
        std::fs::write(d.join(IMS_MODE_FILE), "rm -rf").unwrap();
        assert_eq!(ims_mode(&d), "vowifi", "garbage -> default");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn vowifi_markers() {
        let d = std::env::temp_dir().join(format!("dongie-feat-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(vowifi_choice(&d), "auto");
        std::fs::write(marker(&d, true), b"").unwrap();
        assert_eq!(vowifi_choice(&d), "on");
        std::fs::write(marker(&d, false), b"").unwrap();
        assert_eq!(vowifi_choice(&d), "off", "off wins if both exist");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
